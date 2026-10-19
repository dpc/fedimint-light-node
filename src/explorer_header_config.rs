/// Explicit opt-in to untrusted, independently fetched Esplora header candidates.
///
/// HTTPS authenticates the transport, not the chain. See SPEC-chain-trust.
/// Empty roots disable this traffic. Existing `HeaderSync` constructors disable it.
#[derive(Clone, Debug, Default)]
pub struct ExplorerHeaderConfig {
    /// Up to eight credential-free HTTPS API roots, each at most 2048 bytes.
    /// No query, fragment, duplicate host/port, redirects or discovered URLs.
    pub roots: Vec<String>,
}

impl ExplorerHeaderConfig {
    /// Reject unsafe or excessive roots without making a network request.
    pub fn validate(&self) -> Result<(), crate::sync_error::SyncError> {
        crate::explorer_http::ExplorerHttp::new(self).map(|_| ())
    }

    /// Conventional public transports, not checkpoints or an operator quorum.
    pub fn public_defaults() -> Self {
        Self {
            roots: vec![
                "https://mempool.space/api".into(),
                "https://blockstream.info/api".into(),
            ],
        }
    }
}
