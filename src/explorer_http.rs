//! Bounded standard Esplora text requests, with no response-supplied URLs.

use std::time::{Duration, SystemTime};

use reqwest::{Client, Url};
use tokio::time::timeout;

use crate::explorer_header_config::ExplorerHeaderConfig;
use crate::sync_error::SyncError;

/// Shared by transport and visit admission so each attempt gets its full budget.
pub(crate) const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);

#[cfg(test)]
#[path = "explorer_http_tests.rs"]
mod tests;

/// Shared certificate-validating client; at most one application request at a time.
pub(crate) struct ExplorerHttp {
    /// No retries, proxies, redirects, pooling or automatic decompression.
    client: Client,
    /// Validated caller roots; test builds alone permit loopback HTTP.
    roots: Vec<Url>,
}

/// Safe categories only; never includes URLs, arbitrary headers or response bodies.
#[derive(Debug)]
pub(crate) enum FetchError {
    Transport,
    Timeout,
    Reply,
    RateLimited(Option<Duration>),
}

impl ExplorerHttp {
    pub fn new(config: &ExplorerHeaderConfig) -> Result<Self, SyncError> {
        if config.roots.len() > 8 {
            return Err(SyncError::Configuration);
        }
        let mut roots: Vec<Url> = Vec::new();
        for root in &config.roots {
            if root.len() > 2048 {
                return Err(SyncError::Configuration);
            }
            let url = Url::parse(root).map_err(|_| SyncError::Configuration)?;
            let allowed = url.scheme() == "https";
            #[cfg(test)]
            let allowed =
                allowed || (url.scheme() == "http" && url.host_str() == Some("127.0.0.1"));
            if !allowed
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
                || roots.iter().any(|other| {
                    other.host_str() == url.host_str()
                        && other.port_or_known_default() == url.port_or_known_default()
                })
            {
                return Err(SyncError::Configuration);
            }
            roots.push(url);
        }
        Ok(Self {
            client: crate::bootstrap_http::hardened_client(REQUEST_TIMEOUT)
                .map_err(|_| SyncError::Configuration)?,
            roots,
        })
    }

    /// Whole three-second deadline includes connect, headers and every body chunk.
    pub async fn text(
        &self,
        source: usize,
        path: &str,
        limit: usize,
    ) -> Result<String, FetchError> {
        timeout(REQUEST_TIMEOUT, async {
            let mut response = self
                .client
                .get(format!(
                    "{}/{}",
                    self.roots[source].as_str().trim_end_matches('/'),
                    path
                ))
                .send()
                .await
                .map_err(|_| FetchError::Transport)?;
            if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS
                || (response.status() == reqwest::StatusCode::SERVICE_UNAVAILABLE
                    && response
                        .headers()
                        .contains_key(reqwest::header::RETRY_AFTER))
            {
                let retry = response
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|value| value.to_str().ok())
                    .map(|value| retry_after(value, SystemTime::now()))
                    .unwrap_or(Some(Duration::ZERO));
                return Err(FetchError::RateLimited(retry));
            }
            if response.status() != reqwest::StatusCode::OK
                || response
                    .headers()
                    .contains_key(reqwest::header::CONTENT_ENCODING)
                || response.content_length().is_some_and(|n| n > limit as u64)
            {
                return Err(FetchError::Reply);
            }
            let mut body = Vec::with_capacity(limit);
            while let Some(chunk) = response.chunk().await.map_err(|_| FetchError::Transport)? {
                if chunk.len() > limit - body.len() {
                    return Err(FetchError::Reply);
                }
                body.extend_from_slice(&chunk);
            }
            String::from_utf8(body).map_err(|_| FetchError::Reply)
        })
        .await
        .map_err(|_| FetchError::Timeout)?
    }
}

/// Server scheduling hints are bounded input, not permission to retry immediately.
pub(crate) fn retry_after(value: &str, now: SystemTime) -> Option<Duration> {
    if value.len() > 128 {
        // Do not scan unbounded header values or retry before a potentially
        // excessive valid server delay. Only this source is suspended.
        return None;
    }
    let value = value.trim();
    let seconds = if !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()) {
        value.parse::<u64>().unwrap_or(u64::MAX)
    } else {
        httpdate::parse_http_date(value)
            .ok()
            .and_then(|date| date.duration_since(now).ok())
            .map_or(0, |duration| duration.as_secs())
    };
    (seconds <= 7 * 86400).then(|| Duration::from_secs(seconds))
}
