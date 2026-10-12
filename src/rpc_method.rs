use std::str::FromStr;
use std::time::Duration;

use bitcoin::hex::{DisplayHex, FromHex};
use bitcoin::{Amount, BlockHash};
use serde_json::{Value, json};

use crate::block_decode::BlockDecodeLimits;
use crate::block_download_config::BlockDownloadConfig;
use crate::block_download_error::BlockDownloadError;
use crate::fee_policy::FeePolicy;
use crate::guardian_rpc::SYNTHETIC_STATUS_WARNING;
use crate::header_sync::HeaderSync;
use crate::store_error::StoreError;
use crate::transaction_decode::decode_transaction;
use crate::transaction_submission_config::TransactionSubmissionConfig;

/// Parsed envelope with bounded method and positional parameters.
pub(crate) struct RpcRequest {
    /// Method name, limited to this small dialect.
    method: String,
    /// At most two positional arguments; no batches or named arguments.
    params: Vec<Value>,
}

impl RpcRequest {
    pub(crate) fn is_network(&self) -> bool {
        matches!(self.method.as_str(), "getblock" | "sendrawtransaction")
    }
    pub(crate) fn parse(mut value: Value) -> Result<Self, RpcError> {
        let invalid = || {
            RpcError::new(
                -32600,
                "invalid request; batches and notifications unsupported",
            )
        };
        let object = value.as_object_mut().ok_or_else(invalid)?;
        if object.len() > 4
            || object
                .keys()
                .any(|k| !["jsonrpc", "id", "method", "params"].contains(&k.as_str()))
            || !object.get("id").is_some_and(|id| {
                id.is_null()
                    || id.as_i64().is_some()
                    || id.as_u64().is_some()
                    || id.as_str().is_some_and(|s| s.len() <= 64)
            })
            || !object
                .get("jsonrpc")
                .is_none_or(|v| v.is_null() || v == "1.0" || v == "2.0")
        {
            return Err(invalid());
        }
        let method = object
            .remove("method")
            .and_then(|v| v.as_str().map(str::to_owned))
            .filter(|s| s.len() <= 64)
            .ok_or_else(invalid)?;
        let params = match object.remove("params").unwrap_or_else(|| json!([])) {
            Value::Array(params) if params.len() <= 2 => params,
            _ => return Err(RpcError::params()),
        };
        Ok(Self { method, params })
    }
}

/// Bounded public error dialect, with local failures distinct from peer failures.
pub(crate) struct RpcError {
    /// JSON-RPC standard or documented service-specific error code.
    code: i32,
    /// Static diagnostic; never echo credentials or transaction bytes.
    pub message: &'static str,
    /// Local corruption/storage failure stops the owner rather than retrying peers.
    pub fatal: bool,
}

impl RpcError {
    pub(crate) fn new(code: i32, message: &'static str) -> Self {
        Self {
            code,
            message,
            fatal: false,
        }
    }
    pub(crate) fn unavailable() -> Self {
        Self::new(
            -32012,
            "peer/operation deadline or unavailable; submission may be indeterminate",
        )
    }
    fn params() -> Self {
        Self::new(-32602, "unsupported positional parameters")
    }
    fn recovery() -> Self {
        Self {
            code: -32013,
            message: "local state failure; explicit recovery required",
            fatal: true,
        }
    }
    pub(crate) fn value(&self) -> Value {
        json!({"code": self.code, "message": self.message,
            "data": {"retryable": matches!(self.code, -32012 | -32014)}})
    }
}

fn storage(error: StoreError) -> RpcError {
    match error {
        StoreError::OutsideRetainedHistory => {
            RpcError::new(-32010, "outside retained history; no archival recovery")
        }
        StoreError::BeyondTip => RpcError::new(-8, "height beyond selected tip"),
        _ => RpcError::recovery(),
    }
}

/// Convert static sat/kvB policy only when the pinned client's f64 amount round-trips.
pub(crate) fn fee_value(policy: FeePolicy) -> Result<Value, RpcError> {
    match policy.sats_per_kvb() {
        None => Ok(json!({"blocks": 1, "errors": ["operator fee policy unavailable"]})),
        Some(rate) => {
            let btc = rate.get() as f64 / 100_000_000.0;
            if Amount::from_btc(btc).ok().map(Amount::to_sat) != Some(rate.get()) {
                return Err(RpcError::new(
                    -32602,
                    "fixed fee cannot round-trip through guardian BTC/kvB amount",
                ));
            }
            Ok(json!({"blocks": 1, "feerate": btc}))
        }
    }
}

/// Preserve witness serialization at the JSON raw-block boundary.
pub(crate) fn block_value(block: &bitcoin::Block) -> Value {
    Value::String(bitcoin::consensus::serialize(block).to_lower_hex_string())
}

pub(crate) async fn dispatch(
    sync: &mut HeaderSync,
    request: &RpcRequest,
    fake: bool,
    submission: bool,
) -> Result<Value, RpcError> {
    let status = sync.status().map_err(storage)?;
    if status.stopped {
        return Err(RpcError::recovery());
    }
    let p = &request.params;
    match request.method.as_str() {
        "getblockcount" if p.is_empty() => Ok(json!(status.height)),
        "getblockhash" if p.len() == 1 => {
            let height = p[0].as_u64().ok_or_else(RpcError::params)?;
            let hash = if height == 1 {
                sync.chain_id()
            } else {
                sync.block_hash(height).map_err(storage)?
            };
            Ok(json!(hash.to_string()))
        }
        "getblockchaininfo" if p.is_empty() && fake => Ok(json!({
            "chain": "main", "blocks": status.height, "headers": status.height,
            "bestblockhash": status.hash.to_string(), "difficulty": 1.0, "mediantime": 0,
            "verificationprogress": 1.0, "initialblockdownload": false,
            "chainwork": "0000000000000000000000000000000000000000000000000000000000000000",
            "size_on_disk": 0, "pruned": true, "warnings": [SYNTHETIC_STATUS_WARNING]
        })),
        "getnetworkinfo" if p.is_empty() && fake => {
            Ok(json!({"version": 190000, "subversion": "/fedimint-light-node:fake-bitcoind/"}))
        }
        "getblockchaininfo" | "getnetworkinfo" if !fake => Err(RpcError::new(
            -32601,
            "synthetic Core status requires explicit --fake-bitcoind",
        )),
        "estimatesmartfee"
            if p.len() == 2 && p[0].as_u64() == Some(1) && p[1] == "CONSERVATIVE" =>
        {
            fee_value(sync.fee_policy())
        }
        "getblock" if p.len() == 2 && p[1].as_u64() == Some(0) => {
            let hash = BlockHash::from_str(p[0].as_str().ok_or_else(RpcError::params)?)
                .map_err(|_| RpcError::params())?;
            let height = sync
                .store()
                .height_for_hash(hash)
                .map_err(storage)?
                .ok_or_else(|| RpcError::new(-32011, "hash unknown or not retained"))?;
            let block = sync
                .fetch_block(
                    height,
                    hash,
                    BlockDownloadConfig {
                        max_attempts: 2,
                        total_timeout: Duration::from_secs(5),
                        visit_timeout: Duration::from_secs(2),
                        max_received_bytes: 8_100_000,
                        decode: BlockDecodeLimits {
                            max_payload_bytes: 4_000_000,
                            max_elements: 1_000_000,
                        },
                    },
                )
                .await
                .map_err(|e| match e {
                    BlockDownloadError::Store(e) => storage(e),
                    BlockDownloadError::Stopped | BlockDownloadError::HeaderIntegrity(_) => {
                        RpcError::recovery()
                    }
                    BlockDownloadError::OutsideRetainedHistory => {
                        RpcError::new(-32010, "outside retained history")
                    }
                    BlockDownloadError::BeyondTip | BlockDownloadError::NotCanonical => {
                        RpcError::new(-32011, "hash not selected")
                    }
                    _ => RpcError::unavailable(),
                })?;
            Ok(block_value(&block))
        }
        "sendrawtransaction" if !submission => Err(RpcError::new(
            -32601,
            "submission-only RPC requires --allow-rpc-submission; no Core fee guards",
        )),
        "sendrawtransaction" if p.len() == 1 => {
            let hex = p[0]
                .as_str()
                .filter(|s| s.len() <= 800_000)
                .ok_or_else(RpcError::params)?;
            let bytes = Vec::<u8>::from_hex(hex).map_err(|_| RpcError::params())?;
            let config = TransactionSubmissionConfig {
                max_attempts: 1,
                total_timeout: Duration::from_secs(5),
                visit_timeout: Duration::from_secs(4),
                ..TransactionSubmissionConfig::default()
            };
            let transaction = decode_transaction(&bytes, config).map_err(|_| RpcError::params())?;
            let report = sync
                .submit_transaction(&transaction, config)
                .await
                .map_err(|_| RpcError::unavailable())?;
            if report.submitted() == 0 {
                return Err(RpcError::unavailable());
            }
            Ok(json!(report.txid.to_string()))
        }
        "getblockcount" | "getblockhash" | "getblockchaininfo" | "getnetworkinfo"
        | "estimatesmartfee" | "getblock" | "sendrawtransaction" => Err(RpcError::params()),
        _ => Err(RpcError::new(
            -32601,
            "method not supported by guardian-only RPC",
        )),
    }
}
