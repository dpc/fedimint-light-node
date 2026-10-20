use bitcoin::hex::FromHex;

use crate::fee_policy::FeePolicy;
use crate::rpc_method::fee_value;

#[test]
fn unavailable_errors_only_discuss_submission_for_submission() {
    let read = crate::rpc_method::RpcError::unavailable();
    let submission = crate::rpc_method::RpcError::submission_unavailable();
    assert_eq!(read.code, submission.code);
    assert!(!read.message.contains("submission"));
    assert!(
        submission
            .message
            .contains("submission may be indeterminate")
    );
}

#[test]
fn logging_names_do_not_echo_unknown_methods_or_parameters() {
    for (method, expected) in [
        ("getblock", "getblock"),
        ("getfeeobservations", "getfeeobservations"),
        ("getrawtransaction", "getrawtransaction"),
        ("secret-password", "unsupported"),
        ("sendrawtransaction", "sendrawtransaction"),
    ] {
        let request = crate::rpc_method::RpcRequest::parse(serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": method,
            "params": ["secret-transaction-bytes"],
        }))
        .ok()
        .unwrap();
        assert_eq!(request.log_method(), expected);
        if method == "getfeeobservations" {
            assert!(!request.is_network());
        }
        if method == "getrawtransaction" {
            assert!(request.is_network());
        }
    }
}

#[test]
fn fixed_fees_roundtrip_through_pinned_schema_including_one_satoshi() {
    for sats in [1, 999, 1000, 1001, 100_000_001, 2_100_000_000_000_000] {
        let value = fee_value(FeePolicy::Fixed {
            sats_per_kvb: sats.try_into().unwrap(),
        })
        .ok()
        .unwrap();
        let wire = serde_json::to_vec(&value).unwrap();
        let response: bitcoincore_rpc::json::EstimateSmartFeeResult =
            serde_json::from_slice(&wire).unwrap();
        assert_eq!(response.fee_rate.unwrap().to_sat(), sats);
    }
}

#[test]
fn raw_hex_serialization_preserves_witness_and_txid_distinction() {
    let bytes = include_bytes!("../tests/data/testnet-segwit.raw");
    let block: bitcoin::Block = bitcoin::consensus::deserialize(bytes).unwrap();
    let value = crate::rpc_method::block_value(&block);
    assert_eq!(Vec::<u8>::from_hex(value.as_str().unwrap()).unwrap(), bytes);
    assert!(
        block
            .txdata
            .iter()
            .any(|tx| tx.compute_txid().to_string() != tx.compute_wtxid().to_string())
    );
}
