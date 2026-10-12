use std::path::Path;

use bitcoin::{BlockHash, Network};
use tokio::time::timeout;

use crate::bootstrap_config::BootstrapConfig;
use crate::bootstrap_error::BootstrapError;
use crate::bootstrap_history::history;
use crate::bootstrap_http::BootstrapHttp;
use crate::header_context::HeaderContext;
use crate::header_error::HeaderError;
use crate::store_anchor::StoreAnchor;
use crate::validated_header_store::ValidatedHeaderStore;

/// Initialize a fresh mainnet store from explicitly trusted HTTPS source agreement.
///
/// Default sources must agree at the *same height*, six successor blocks behind
/// the lower reported tip. Independent operators are a caller trust assertion.
/// HTTPS authenticates endpoints, not Bitcoin consensus or objective freshness.
/// A local clock bounds checkpoint age; colluding sources can still lie.
///
/// HTTP supplies only identity, heights, hashes and two headers. P2P supplies the
/// bounded history, authenticated backwards into the frozen checkpoint. No blocks
/// are fetched over HTTP. Header work starts at zero at that checkpoint; retained
/// earlier rows are trusted ancestry, not independently validated descendants.
///
/// There is no maintenance loop or runtime creation. Dropping this future before
/// publication stops awaiting HTTP, closes owned P2P work, and leaves no public
/// store. The HTTP client's OS DNS jobs can outlive cancellation and delay runtime
/// shutdown; HTTP connection setup may race multiple resolved addresses. Synchronous
/// authentication and atomic database publication run between awaits and cannot
/// be interrupted; run on a caller-owned dedicated runtime thread if needed.
/// A crash can leave private staging files. Existing destinations are never replaced.
/// Reopen with `ValidatedHeaderStore::open_pinned` offline, not by bootstrapping again.
pub async fn bootstrap(
    path: impl AsRef<Path>,
    config: BootstrapConfig,
    now: u64,
) -> Result<ValidatedHeaderStore, BootstrapError> {
    if !config.valid() {
        return Err(BootstrapError::Configuration);
    }
    let path = path.as_ref();
    match std::fs::symlink_metadata(path) {
        Ok(_) => return Err(BootstrapError::AlreadyExists),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let mut http = BootstrapHttp::new(&config)?;
    let (anchor, headers) = timeout(config.total_timeout, async {
        let (anchor, source) = agree(&config, &mut http, now).await?;
        let floor = anchor
            .height
            .saturating_sub(config.store.header_capacity.get() - 1);
        let start = floor.min(HeaderContext::context_start(anchor.height));
        let seed_hash = http.hash(source, start).await?;
        let seed = http.header(source, seed_hash).await?;
        let headers = history(&config, anchor, start, seed).await?;
        Ok::<_, BootstrapError>((anchor, headers))
    })
    .await
    .map_err(|_| BootstrapError::Timeout)??;
    Ok(ValidatedHeaderStore::create_with_history(
        path,
        config.store,
        anchor,
        &headers,
    )?)
}

/// Mainnet's fixed height-one identity, independent of rolling history or sources.
pub(crate) fn height_one_hash() -> BlockHash {
    "00000000839a8e6886ab5951d76f411475428afc90947ee320161bbf18eb6048"
        .parse()
        .expect("mainnet height one")
}

async fn agree(
    config: &BootstrapConfig,
    http: &mut BootstrapHttp,
    now: u64,
) -> Result<(StoreAnchor, usize), BootstrapError> {
    let genesis = bitcoin::blockdata::constants::genesis_block(Network::Bitcoin).block_hash();
    let mut tips = Vec::with_capacity(config.sources.len());
    let mut failure = BootstrapError::Reply("insufficient source quorum");
    for source in 0..config.sources.len() {
        let result = async {
            if http.hash(source, 0).await? != genesis
                || http.hash(source, 1).await? != height_one_hash()
            {
                return Err(BootstrapError::WrongNetwork);
            }
            let text = http.text(source, "blocks/tip/height", 10).await?;
            if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
                return Err(BootstrapError::Reply("height encoding"));
            }
            let height: u32 = text
                .parse()
                .map_err(|_| BootstrapError::Reply("height overflow"))?;
            Ok(u64::from(height))
        }
        .await;
        match result {
            Ok(height) => tips.push((source, height)),
            Err(BootstrapError::WrongNetwork) => return Err(BootstrapError::WrongNetwork),
            Err(error) => failure = error,
        }
    }
    if tips.len() < config.quorum {
        return Err(failure);
    }
    let height = tips
        .iter()
        .map(|(_, height)| *height)
        .min()
        .expect("quorum")
        .checked_sub(config.confirmation_margin)
        .ok_or(BootstrapError::Stale)?;
    let mut agreed = None;
    let mut votes = 0;
    let mut selected_source = 0;
    for (source, _) in tips {
        match http.hash(source, height).await {
            Ok(hash) => {
                if agreed.is_some_and(|previous| previous != hash) {
                    return Err(BootstrapError::Disagreement);
                }
                agreed = Some(hash);
                selected_source = source;
                votes += 1;
            }
            Err(error) => failure = error,
        }
    }
    if votes < config.quorum {
        return Err(failure);
    }
    let hash = agreed.expect("quorum");
    let header = http.header(selected_source, hash).await?;
    if u64::from(header.time) > now.saturating_add(7200) {
        return Err(HeaderError::FutureTime.into());
    }
    if now.saturating_sub(u64::from(header.time)) > config.max_checkpoint_age.as_secs() {
        return Err(BootstrapError::Stale);
    }
    crate::declared_pow::check_declared_pow(&header, bitcoin::Target::MAX_ATTAINABLE_MAINNET)
        .map_err(HeaderError::from)?;
    Ok((
        StoreAnchor {
            network: Network::Bitcoin,
            height_one_hash: height_one_hash(),
            height,
            hash,
        },
        selected_source,
    ))
}

#[cfg(test)]
#[path = "bootstrap_tests.rs"]
mod tests;
