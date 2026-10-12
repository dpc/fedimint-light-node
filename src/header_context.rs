use std::cmp::Ordering;

use bitcoin::block::Header;
use bitcoin::{CompactTarget, Network, Target, Work};

use crate::declared_pow::check_declared_pow;
use crate::header_error::HeaderError;
use crate::store_anchor::StoreAnchor;
use crate::store_error::StoreError;

/// Fixed-size schema-v2 context payload, without variable-length allocations.
type ContextRecord = (u64, [u8; 80], [u32; 11], u32, [u8; 32]);

/// Bounded mainnet validation state rooted at a caller-trusted anchor.
///
/// Historical context is authenticated by linkage into the anchor, not freshly
/// validated. Work is zero at the anchor and counts only validated descendants;
/// it is not Bitcoin's absolute chainwork. Cloning permits bounded candidate
/// evaluation without altering selected state. This does not validate transactions.
#[derive(Clone, Debug)]
pub struct HeaderContext {
    /// Immutable origin of relative work and historical trust.
    anchor: StoreAnchor,
    /// Height of the contextual tip.
    height: u64,
    /// Current contextual tip.
    tip: Header,
    /// Chronological last eleven timestamps, unused trailing entries zero.
    times: [u32; 11],
    /// Timestamp at the start of the tip's difficulty period.
    period_start: u32,
    /// Accumulated work strictly above the anchor.
    work: Work,
}

impl HeaderContext {
    /// Authenticate sufficient contiguous historical headers into a trusted anchor.
    ///
    /// Supply exactly heights `context_start(anchor.height)..=anchor.height`.
    /// At most 2016 headers are borrowed; none are retained except the tip and
    /// required timestamps. The caller establishes anchor trust, height and chain
    /// identity externally. No claimed work below that anchor is accepted.
    pub fn from_trusted_history(
        anchor: StoreAnchor,
        history: &[Header],
    ) -> Result<Self, HeaderError> {
        if anchor.network != Network::Bitcoin {
            return Err(HeaderError::UnsupportedNetwork);
        }
        let first = Self::context_start(anchor.height);
        if history.len() as u64 != anchor.height - first + 1 {
            return Err(HeaderError::MissingContext);
        }
        for pair in history.windows(2) {
            if pair[1].prev_blockhash != pair[0].block_hash() {
                return Err(HeaderError::Linkage);
            }
        }
        let tip = *history.last().ok_or(HeaderError::MissingContext)?;
        if tip.block_hash() != anchor.hash {
            return Err(HeaderError::Linkage);
        }
        if first <= 1
            && anchor.height >= 1
            && history[(1 - first) as usize].block_hash() != anchor.height_one_hash
        {
            return Err(HeaderError::Linkage);
        }
        // Trust authenticates history but does not make invalid target arithmetic
        // safe. The anchor itself must have a usable target and actual PoW.
        check_declared_pow(&tip, Target::MAX_ATTAINABLE_MAINNET)?;
        let count = timestamp_count(anchor.height);
        let mut times = [0; 11];
        for (slot, header) in times.iter_mut().zip(&history[history.len() - count..]) {
            *slot = header.time;
        }
        let period_start = history[(anchor.height / 2016 * 2016 - first) as usize].time;
        Ok(Self {
            anchor,
            height: anchor.height,
            tip,
            times,
            period_start,
            work: Work::from_be_bytes([0; 32]),
        })
    }

    /// Return the earliest required historical height for a trusted anchor.
    pub fn context_start(height: u64) -> u64 {
        height.saturating_sub(10).min(height / 2016 * 2016)
    }

    /// Return the height of this context's tip.
    pub fn height(&self) -> u64 {
        self.height
    }

    /// Return the tip header, not a claim of full consensus validity.
    pub fn tip(&self) -> Header {
        self.tip
    }

    /// Return accumulated work above, and excluding, the trusted anchor.
    pub fn relative_work(&self) -> Work {
        self.work
    }

    /// Compare work only when both contexts have the same trusted origin.
    pub fn compare_work(&self, other: &Self) -> Result<Ordering, HeaderError> {
        if self.anchor != other.anchor {
            return Err(HeaderError::DifferentAnchor);
        }
        Ok(self.work.cmp(&other.work))
    }

    /// Compute the exact compact target required for the next mainnet header.
    pub fn expected_target(&self) -> Result<CompactTarget, HeaderError> {
        let next = self.height.checked_add(1).ok_or(HeaderError::Overflow)?;
        if next % 2016 != 0 {
            return Ok(self.tip.bits);
        }
        // Core uses signed subtraction; negative intervals clamp to 1/4 period.
        // Avoid rust-bitcoin's header convenience method's unsigned subtraction.
        let elapsed = (i64::from(self.tip.time) - i64::from(self.period_start))
            .clamp(302_400, 4_838_400) as u64;
        Ok(CompactTarget::from_next_work_required(
            self.tip.bits,
            elapsed,
            bitcoin::consensus::Params::MAINNET,
        ))
    }

    /// Validate and return the next context, leaving this state unchanged on error.
    ///
    /// `now` is caller-supplied Unix seconds; use a trustworthy local clock, not a
    /// peer timestamp. Future-time rejection can be retried when time advances.
    /// Rules follow Core's mainnet difficulty, MTP, two-hour limit and buried
    /// version floors; this is not full consensus or body validation.
    pub fn validate_next(&self, header: Header, now: u64) -> Result<Self, HeaderError> {
        let height = self.height.checked_add(1).ok_or(HeaderError::Overflow)?;
        if header.prev_blockhash != self.tip.block_hash() {
            return Err(HeaderError::Linkage);
        }
        if header.bits != self.expected_target()? {
            return Err(HeaderError::Difficulty);
        }
        let count = timestamp_count(self.height);
        let mut sorted = self.times;
        sorted[..count].sort_unstable();
        if header.time <= sorted[count / 2] {
            return Err(HeaderError::MedianTime);
        }
        if u64::from(header.time) > now.saturating_add(7200) {
            return Err(HeaderError::FutureTime);
        }
        let minimum_version = if height >= 388_381 {
            4
        } else if height >= 363_725 {
            3
        } else if height >= 227_931 {
            2
        } else {
            1
        };
        if height >= 227_931 && header.version.to_consensus() < minimum_version {
            return Err(HeaderError::Version);
        }
        if height == 1 && header.block_hash() != self.anchor.height_one_hash {
            return Err(HeaderError::Linkage);
        }
        check_declared_pow(&header, Target::MAX_ATTAINABLE_MAINNET)?;
        let work = checked_work(self.work, header.work())?;
        let mut next = self.clone();
        if count == 11 {
            next.times.rotate_left(1);
        }
        next.times[count.min(10)] = header.time;
        if height % 2016 == 0 {
            next.period_start = header.time;
        }
        next.height = height;
        next.tip = header;
        next.work = work;
        Ok(next)
    }

    pub(crate) fn encode(&self) -> Vec<u8> {
        let header: [u8; 80] = bitcoin::consensus::serialize(&self.tip)
            .try_into()
            .expect("header");
        bincode::encode_to_vec(
            (
                self.height,
                header,
                self.times,
                self.period_start,
                self.work.to_be_bytes(),
            ),
            codec(),
        )
        .expect("fixed-size context")
    }

    pub(crate) fn decode(bytes: &[u8], anchor: StoreAnchor) -> Result<Self, StoreError> {
        if bytes.len() > 256 {
            return Err(StoreError::Corrupt("oversized validation context"));
        }
        let ((height, header, times, period_start, work), consumed): (ContextRecord, usize) =
            bincode::decode_from_slice(bytes, codec())
                .map_err(|_| StoreError::Corrupt("invalid validation context"))?;
        if consumed != bytes.len() || anchor.network != Network::Bitcoin || height < anchor.height {
            return Err(StoreError::Corrupt("invalid validation context origin"));
        }
        let tip: Header = bitcoin::consensus::deserialize(&header)
            .map_err(|_| StoreError::Corrupt("invalid context header"))?;
        let count = timestamp_count(height);
        if times[count - 1] != tip.time
            || times[count..].iter().any(|time| *time != 0)
            || (height % 2016 == 0 && period_start != tip.time)
            || (height == anchor.height && (tip.block_hash() != anchor.hash || work != [0; 32]))
        {
            return Err(StoreError::Corrupt("inconsistent validation context"));
        }
        check_declared_pow(&tip, Target::MAX_ATTAINABLE_MAINNET)
            .map_err(|_| StoreError::Corrupt("context target or proof of work"))?;
        Ok(Self {
            anchor,
            height,
            tip,
            times,
            period_start,
            work: Work::from_be_bytes(work),
        })
    }
}

fn timestamp_count(height: u64) -> usize {
    height.min(10) as usize + 1
}

fn codec() -> impl bincode::config::Config {
    bincode::config::standard()
        .with_fixed_int_encoding()
        .with_little_endian()
        .with_limit::<256>()
}

fn checked_work(left: Work, right: Work) -> Result<Work, HeaderError> {
    let mut result = left.to_be_bytes();
    let right = right.to_be_bytes();
    let mut carry = 0u16;
    for (left, right) in result.iter_mut().zip(right).rev() {
        let sum = u16::from(*left) + u16::from(right) + carry;
        *left = sum as u8;
        carry = sum >> 8;
    }
    if carry != 0 {
        return Err(HeaderError::Overflow);
    }
    Ok(Work::from_be_bytes(result))
}

#[cfg(test)]
#[path = "header_context_tests.rs"]
mod tests;
