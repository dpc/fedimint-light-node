use std::net::SocketAddr;

/// Explicit opt-in loopback transport policy; credentials are never debug-printed.
pub struct GuardianRpcConfig {
    /// Numeric loopback address; port zero is useful for embedded tests.
    pub bind: SocketAddr,
    /// Nonempty Basic authentication username, without a colon.
    pub username: String,
    /// Nonempty Basic authentication password.
    pub password: String,
    /// Default-off permission to return enumerated synthetic Core status fields.
    pub fake_bitcoind: bool,
    /// Explicit acknowledgement of submission-only semantics, without Core fee guards.
    pub allow_submission: bool,
}
