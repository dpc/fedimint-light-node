use std::cmp::Ordering;
use std::path::Path;

use bitcoin::block::Header;
use bitcoin::{BlockHash, Network};

use crate::header_context::HeaderContext;
use crate::header_error::HeaderError;
use crate::header_store::HeaderStore;
use crate::store_anchor::StoreAnchor;
use crate::store_config::StoreConfig;
use crate::store_error::StoreError;

/// Mainnet contextual admission with atomic bounded canonical chain selection.
///
/// Each retained header at or above the trust anchor has a fixed-size checkpoint, so even a
/// one-header rolling window can validate its next retarget after restart.
/// Forks may start at any retained, unprotected parent at or above the anchor.
/// Candidates are bounded by header capacity and selected only by strictly greater
/// relative work. No orphans, alternative branches or automatic recovery persist.
/// `RecoveryRequired` forbids applying that candidate with the available context
/// and protection. An unsupported remote assertion does not establish that selected
/// state needs recovery: reject it without stopping unrelated peers. Actual local
/// state failures require stopping affected service and coordinating explicit
/// recovery with consumers (SPEC-chain-trust).
///
/// This is not a bootstrap authenticator, synchronizer, finality oracle or full
/// consensus validator. Protect consumed history explicitly before exposing it
/// as finalized; the library cannot infer consumers' finality.
pub struct ValidatedHeaderStore {
    /// Private ownership prevents raw writes bypassing validation.
    store: HeaderStore,
}

impl ValidatedHeaderStore {
    /// Reopen the locally pinned mainnet store without HTTP or replacing its anchor.
    ///
    /// Local disk is trusted against malicious replacement. Call `open` instead
    /// when an independently recorded anchor must also match.
    pub fn open_pinned(path: impl AsRef<Path>, config: StoreConfig) -> Result<Self, HeaderError> {
        Ok(Self {
            store: HeaderStore::open_pinned(path.as_ref(), config)?,
        })
    }

    /// Authenticate and atomically publish a bounded pre-checkpoint header window.
    ///
    /// `history` starts at the lesser of the retained floor and checkpoint context
    /// start, and ends at the trusted checkpoint. Historical rows are authenticated,
    /// not independently context-validated, and cannot be fork parents. They carry
    /// no invented work. Only descendants accrue relative work.
    ///
    /// A private sibling directory holds preparation. A same-filesystem hard link
    /// publishes the complete database without overwriting any destination. A crash
    /// may leave private temporary files, but never a partially initialized public
    /// store. Directory-sync failure after publication returns an error even though
    /// a complete store exists; inspect/reopen it, never automatically replace it.
    pub fn create_with_history(
        path: impl AsRef<Path>,
        config: StoreConfig,
        anchor: StoreAnchor,
        history: &[Header],
    ) -> Result<Self, HeaderError> {
        if config.header_capacity.get() > 105_120 {
            return Err(
                StoreError::InvalidUpdate("bootstrap header capacity exceeds limit").into(),
            );
        }
        let first = anchor
            .height
            .saturating_sub(config.header_capacity.get() - 1);
        let start = first.min(HeaderContext::context_start(anchor.height));
        if history.len() as u64 != anchor.height - start + 1 {
            return Err(HeaderError::MissingContext);
        }
        for pair in history.windows(2) {
            if pair[1].prev_blockhash != pair[0].block_hash() {
                return Err(HeaderError::Linkage);
            }
        }
        let path = path.as_ref();
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let temporary = tempfile::Builder::new()
            .prefix(".bootstrap-")
            .tempdir_in(parent)
            .map_err(StoreError::from)?;
        let private = temporary.path().join("headers.redb");
        let context_offset = (HeaderContext::context_start(anchor.height) - start) as usize;
        let mut result = Self::create(&private, config, anchor, &history[context_offset..])?;
        result
            .store
            .insert_history(first, &history[(first - start) as usize..])?;
        std::fs::hard_link(&private, path).map_err(StoreError::from)?;
        std::fs::File::open(parent)
            .and_then(|file| file.sync_all())
            .map_err(StoreError::from)?;
        Ok(result)
    }

    /// Create mainnet state from caller-trusted, hash-linked historical context.
    ///
    /// The history has the exact range required by `HeaderContext::context_start`.
    /// Only the anchor becomes queryable; earlier headers are context, not a
    /// freshly validated historical chain or a queryable backfill.
    pub fn create(
        path: impl AsRef<Path>,
        config: StoreConfig,
        anchor: StoreAnchor,
        history: &[Header],
    ) -> Result<Self, HeaderError> {
        let context = HeaderContext::from_trusted_history(anchor, history)?;
        Ok(Self {
            store: HeaderStore::create_inner(
                path.as_ref(),
                config,
                anchor,
                context.tip(),
                Some(&context),
            )?,
        })
    }

    /// Reopen validated state with matching identity, capacity and schema.
    ///
    /// No automatic migration, raw-store promotion, re-anchoring or recovery is
    /// performed. Disk is trusted against malicious replacement, as in HeaderStore.
    pub fn open(
        path: impl AsRef<Path>,
        config: StoreConfig,
        anchor: StoreAnchor,
    ) -> Result<Self, HeaderError> {
        if anchor.network != Network::Bitcoin {
            return Err(HeaderError::UnsupportedNetwork);
        }
        Ok(Self {
            store: HeaderStore::open_inner(path.as_ref(), config, anchor, true)?,
        })
    }

    /// Return the immutable caller-trusted origin and network identity.
    pub fn anchor(&self) -> StoreAnchor {
        self.store.anchor()
    }

    /// Return the inclusive queryable canonical range.
    pub fn bounds(&self) -> Result<(u64, u64), StoreError> {
        self.store.bounds()
    }

    /// Return the configured maximum retained headers and candidate suffix length.
    pub fn capacity(&self) -> u64 {
        self.store.capacity()
    }

    /// Read a selected header, without implying full consensus validity.
    ///
    /// Heights at or below `anchor().height` are trusted, hash-authenticated
    /// ancestry. Only later heights passed independent contextual admission.
    pub fn header(&self, height: u64) -> Result<Header, StoreError> {
        self.store.header(height)
    }

    /// Resolve a selected hash by bounded retained-header scan, without protecting it.
    ///
    /// `None` means unknown or not retained, not proof that a block was pruned.
    /// The pinned height-one identity exception does not make its body available.
    pub fn height_for_hash(&self, hash: BlockHash) -> Result<Option<u64>, StoreError> {
        self.store.height_for_hash(hash)
    }

    /// Return a copy of the tip's bounded validation and relative-work context.
    pub fn tip_context(&self) -> Result<HeaderContext, StoreError> {
        self.store.context(self.store.bounds()?.1)
    }

    /// Return the persisted rollback protection floor, initially the anchor.
    pub fn protected_height(&self) -> Result<u64, StoreError> {
        self.store.protected()
    }

    /// Persist a monotonic protection floor for a currently retained canonical hash.
    ///
    /// Call before promising that height as finalized to a consumer. Lowering the
    /// floor, replacing protected history, and automatic re-anchoring are forbidden.
    /// Header pruning does not remove this protection.
    /// Retained heights already at or below `protected_height()` need no update;
    /// attempting to lower the floor returns an error, even for trusted ancestry.
    pub fn protect_through(&mut self, height: u64, hash: BlockHash) -> Result<(), StoreError> {
        self.store.protect(height, hash)
    }

    /// Validate a bounded candidate suffix and atomically select it if it wins.
    ///
    /// Passing the tip appends. Other retained parents permit shallow forks only
    /// above the protected floor. Equal work keeps the existing chain; empty or
    /// lower-work candidates never roll it back. All validation, context, work,
    /// tip changes and pruning succeed together or leave persistent state unchanged.
    /// Successful fork selection is observable through the explicit parent and new
    /// bounds; callers must coordinate the resulting canonical change with readers.
    pub fn select_suffix(
        &mut self,
        parent: u64,
        replacement: &[Header],
        now: u64,
    ) -> Result<(), HeaderError> {
        if replacement.len() as u64 > self.store.capacity() {
            return Err(StoreError::InvalidUpdate("batch exceeds header capacity").into());
        }
        let (first, tip) = self.store.bounds()?;
        if parent < first || parent > tip || parent < self.store.protected()? {
            return Err(HeaderError::RecoveryRequired);
        }
        let mut context = self.store.context(parent)?;
        let mut contexts = Vec::with_capacity(replacement.len());
        for header in replacement {
            context = context.validate_next(*header, now)?;
            contexts.push(context.clone());
        }
        if context.compare_work(&self.store.context(tip)?)? != Ordering::Greater {
            return Err(HeaderError::InsufficientWork);
        }
        self.store
            .replace_inner(parent, replacement, Some(&contexts))?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "validated_header_store_tests.rs"]
mod tests;
