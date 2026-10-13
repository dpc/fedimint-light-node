use std::net::SocketAddr;

/// Explicit opt-in loopback transport policy; credentials are never debug-printed.
pub struct GuardianRpcConfig {
    /// Numeric loopback address; port zero is useful for embedded tests.
    pub bind: SocketAddr,
    /// Explicit authentication policy; unauthenticated mode is read-only.
    pub auth: RpcAuth,
    /// Default-off permission to return enumerated synthetic Core status fields.
    pub fake_bitcoind: bool,
    /// Explicit acknowledgement of submission-only semantics, without Core fee guards.
    pub allow_submission: bool,
}

/// Loopback authentication policy, with no implicit unauthenticated default.
pub enum RpcAuth {
    /// Require bounded, nonempty Basic credentials.
    Basic {
        /// Username without a colon or control characters.
        username: String,
        /// Password without control characters.
        password: String,
    },
    /// Accept local read-only requests without credentials; submission is forbidden.
    NoAuth,
}
