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
/// Each retained header has a fixed-size validation checkpoint, so even a
/// one-header rolling window can validate its next retarget after restart.
/// Forks may start at any retained, unprotected parent at or above the anchor.
/// Candidates are bounded by header capacity and selected only by strictly greater
/// relative work. No orphans, alternative branches or automatic recovery persist.
/// A caller must stop affected service on `RecoveryRequired` and coordinate
/// explicit recovery with consumers (SPEC-chain-trust).
///
/// This is not a bootstrap authenticator, synchronizer, finality oracle or full
/// consensus validator. Protect consumed history explicitly before exposing it
/// as finalized; the library cannot infer consumers' finality.
pub struct ValidatedHeaderStore {
    /// Private ownership prevents raw writes bypassing validation.
    store: HeaderStore,
}

impl ValidatedHeaderStore {
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

    /// Read a selected header, without implying full consensus validity.
    pub fn header(&self, height: u64) -> Result<Header, StoreError> {
        self.store.header(height)
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
