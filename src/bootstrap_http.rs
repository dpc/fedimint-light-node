//! Small, bounded Esplora text endpoints; never fetches block bodies.

use bitcoin::BlockHash;
use bitcoin::block::Header;
use bitcoin::hex::FromHex;
use reqwest::{Client, Url};
use tokio::time::timeout;

use crate::bootstrap_config::BootstrapConfig;
use crate::bootstrap_error::BootstrapError;

/// Sequential HTTPS requests with no redirects, retries, cookies or compression.
pub(crate) struct BootstrapHttp {
    /// Certificate-validating rustls client, without environment proxies.
    client: Client,
    /// Validated API roots; tests alone may use loopback HTTP.
    roots: Vec<Url>,
    /// Whole request including streaming response consumption.
    deadline: std::time::Duration,
    /// Global explicit request budget.
    remaining: usize,
}

impl BootstrapHttp {
    pub(crate) fn new(config: &BootstrapConfig) -> Result<Self, BootstrapError> {
        let mut roots: Vec<Url> = Vec::new();
        for source in &config.sources {
            let url = Url::parse(&source.api).map_err(|_| BootstrapError::Configuration)?;
            let allowed = url.scheme() == "https";
            #[cfg(test)]
            let allowed =
                allowed || (url.scheme() == "http" && url.host_str() == Some("127.0.0.1"));
            if !allowed
                || !url.username().is_empty()
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
                || url.host_str().is_none()
                || roots
                    .iter()
                    .any(|other| other.host_str() == url.host_str() && other.port() == url.port())
            {
                return Err(BootstrapError::Configuration);
            }
            roots.push(url);
        }
        Ok(Self {
            client: Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .retry(reqwest::retry::never())
                .use_rustls_tls()
                .no_gzip()
                .no_brotli()
                .no_zstd()
                .no_deflate()
                .no_proxy()
                .pool_max_idle_per_host(0)
                .connect_timeout(config.request_timeout)
                .build()?,
            roots,
            deadline: config.request_timeout,
            remaining: config.sources.len() * 4 + 3,
        })
    }

    pub(crate) async fn text(
        &mut self,
        source: usize,
        path: &str,
        limit: usize,
    ) -> Result<String, BootstrapError> {
        self.remaining = self
            .remaining
            .checked_sub(1)
            .ok_or(BootstrapError::Limit("HTTP requests"))?;
        let url = format!(
            "{}/{}",
            self.roots[source].as_str().trim_end_matches('/'),
            path
        );
        timeout(self.deadline, async {
            let mut response = self
                .client
                .get(url)
                .send()
                .await
                .map_err(reqwest::Error::without_url)?;
            if response.status() != reqwest::StatusCode::OK {
                return Err(BootstrapError::Reply("HTTP status (redirects forbidden)"));
            }
            if response
                .headers()
                .contains_key(reqwest::header::CONTENT_ENCODING)
            {
                return Err(BootstrapError::Reply("HTTP content encoding forbidden"));
            }
            if response.content_length().is_some_and(|n| n > limit as u64) {
                return Err(BootstrapError::Limit("HTTP response bytes"));
            }
            let mut body = Vec::with_capacity(limit);
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(reqwest::Error::without_url)?
            {
                if chunk.len() > limit - body.len() {
                    return Err(BootstrapError::Limit("HTTP response bytes"));
                }
                body.extend_from_slice(&chunk);
            }
            String::from_utf8(body).map_err(|_| BootstrapError::Reply("non-UTF8 text"))
        })
        .await
        .map_err(|_| BootstrapError::Timeout)?
    }

    pub(crate) async fn hash(
        &mut self,
        source: usize,
        height: u64,
    ) -> Result<BlockHash, BootstrapError> {
        let text = self
            .text(source, &format!("block-height/{height}"), 64)
            .await?;
        if text.len() != 64 {
            return Err(BootstrapError::Reply("hash length"));
        }
        text.parse()
            .map_err(|_| BootstrapError::Reply("hash encoding"))
    }

    pub(crate) async fn header(
        &mut self,
        source: usize,
        hash: BlockHash,
    ) -> Result<Header, BootstrapError> {
        let text = self
            .text(source, &format!("block/{hash}/header"), 160)
            .await?;
        let bytes =
            <[u8; 80]>::from_hex(&text).map_err(|_| BootstrapError::Reply("header encoding"))?;
        let header: Header =
            bitcoin::consensus::deserialize(&bytes).map_err(|_| BootstrapError::Reply("header"))?;
        if header.block_hash() != hash {
            return Err(BootstrapError::Reply("header hash mismatch"));
        }
        Ok(header)
    }
}
