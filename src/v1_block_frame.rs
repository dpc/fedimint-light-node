//! Allocation-free Bitcoin v1 envelope admission for `block` messages only.
//!
//! Collect exactly 24 header bytes in a fixed buffer, call [`V1BlockFrame::parse`],
//! then collect exactly [`V1BlockFrame::payload_len`] bytes under caller accounting.
//! Fragmentation and EOF handling belong to the transport: incomplete headers or
//! payloads are errors, not implicit buffering. Do not read/allocate a declared body
//! before header admission. Do not resynchronize by scanning rejected bytes.
//!
//! This is not a handshake, general message dispatcher, or BIP324 transport.
//! Other commands (including otherwise valid handshake messages) are rejected.
//! Checksums detect corruption, not malicious peers or authenticated blocks.
//!
//! Layout follows Bitcoin Core v30.0 `CMessageHeader` in `src/protocol.h`.
//! Magic and SHA256d use rust-bitcoin; checksum byte order matches its 0.32.102
//! `RawNetworkMessage::new`. Its broad decoder allocates and dispatches payloads
//! before our structural budgets, so it is deliberately not used for admission.

use bitcoin::hashes::{Hash, sha256d};
use bitcoin::{Block, BlockHash, Network};

use crate::block_decode::{BlockDecodeLimits, decode_block};
use crate::block_integrity::WitnessRules;
use crate::frame_error::FrameError;

/// An admitted v1 `block` header with fixed-size state and caller decode budgets.
#[derive(Debug, Clone, Copy)]
pub struct V1BlockFrame {
    /// Admitted payload length, never greater than the caller or block byte cap.
    payload_len: usize,
    /// First four raw SHA256d bytes, not a displayed/reversed hash.
    checksum: [u8; 4],
    /// Budgets preserved between header admission and body decoding.
    limits: BlockDecodeLimits,
}

impl V1BlockFrame {
    /// Number of wire bytes required before any payload allocation or reading.
    pub const HEADER_LEN: usize = 24;

    /// Admit exactly one header without allocating or reading its body.
    ///
    /// Requires exact caller network magic and the zero-padded `block` command.
    /// Every other command encoding is rejected, including embedded NULs, bad
    /// padding, non-ASCII bytes and unsupported commands. Length is little-endian
    /// u32 and capped at 4 MB even when the caller supplies a larger budget.
    pub fn parse(
        header: &[u8],
        network: Network,
        limits: BlockDecodeLimits,
    ) -> Result<Self, FrameError> {
        let header: &[u8; Self::HEADER_LEN] =
            header.try_into().map_err(|_| FrameError::HeaderLength)?;
        if header[..4] != network.magic().to_bytes() {
            return Err(FrameError::Magic);
        }
        if &header[4..16] != b"block\0\0\0\0\0\0\0" {
            return Err(FrameError::Command);
        }
        let length = u32::from_le_bytes(header[16..20].try_into().expect("fixed header"));
        let payload_len = usize::try_from(length).map_err(|_| FrameError::PayloadLimit)?;
        if payload_len > limits.max_payload_bytes.min(4_000_000) {
            return Err(FrameError::PayloadLimit);
        }
        Ok(Self {
            payload_len,
            checksum: header[20..24].try_into().expect("fixed header"),
            limits,
        })
    }

    /// Return the admitted body length, excluding the fixed 24-byte header.
    pub fn payload_len(&self) -> usize {
        self.payload_len
    }

    /// Check exact body length and checksum, then perform bounded block admission.
    ///
    /// No hashing or decoding occurs on a length mismatch. The borrowed body is
    /// caller-owned; hashing takes O(body bytes) work without allocation. Decoding
    /// and integrity checks use the budgets captured at header admission.
    /// `requested_hash` and witness activation must come from caller trust context.
    /// This does not check PoW, expected difficulty or full consensus validity;
    /// use the separate declared-target API where appropriate. Rejection must not
    /// invalidate the requested header. Concurrent buffers/results need global
    /// caller budgets; this API supplies neither deadlines nor peer policy.
    pub fn decode(
        &self,
        payload: &[u8],
        requested_hash: BlockHash,
        witness_rules: WitnessRules,
    ) -> Result<Block, FrameError> {
        if payload.len() != self.payload_len {
            return Err(FrameError::PayloadLength);
        }
        if sha256d::Hash::hash(payload)[..4] != self.checksum {
            return Err(FrameError::Checksum);
        }
        Ok(decode_block(
            payload,
            requested_hash,
            witness_rules,
            self.limits,
        )?)
    }
}
