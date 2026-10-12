use std::path::PathBuf;
use std::time::Duration;

/// Mainnet discovery and maintained-connection policy; addresses are never chain authority.
#[derive(Debug, Clone)]
pub struct PeerPoolConfig {
    /// Enable DNS and public legacy `addr` discovery, independently of manual peers.
    pub discovery: bool,
    /// Independently operated DNS seed names; at most eight, each at most 253 bytes.
    pub dns_seeds: Vec<String>,
    /// Total sockets, including a single pending dial; one through four.
    pub connections: usize,
    /// Explorer lifetime lower bound; the actual lifetime is randomized up to twice this.
    pub rotation: Duration,
    /// Optional disposable successful-peer cache in a caller-owned private directory.
    pub cache: Option<PathBuf>,
    /// Test-only numeric discovery injection after production routability admission.
    #[cfg(test)]
    pub(crate) discovered_for_test: Vec<std::net::SocketAddr>,
}

impl Default for PeerPoolConfig {
    fn default() -> Self {
        Self {
            discovery: true,
            // Bitcoin Core v30.0 mainnet seeds, three independently operated hints.
            dns_seeds: [
                "seed.bitcoin.sipa.be",
                "seed.bitcoin.sprovoost.nl",
                "dnsseed.emzy.de",
            ]
            .map(str::to_owned)
            .to_vec(),
            connections: 4,
            rotation: Duration::from_secs(600),
            cache: None,
            #[cfg(test)]
            discovered_for_test: Vec::new(),
        }
    }
}

impl PeerPoolConfig {
    /// Disable all automatic discovery and cache use; explicit local peers remain allowed.
    pub fn manual_only() -> Self {
        Self {
            discovery: false,
            dns_seeds: Vec::new(),
            ..Self::default()
        }
    }

    /// Bound configuration before starting network work.
    pub(crate) fn valid(&self) -> bool {
        (1..=4).contains(&self.connections)
            && !self.rotation.is_zero()
            && self.rotation <= Duration::from_secs(3600)
            && self.dns_seeds.len() <= 8
            && self.dns_seeds.iter().all(|s| {
                !s.is_empty()
                    && s.len() <= 253
                    && s.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
            })
    }
}
