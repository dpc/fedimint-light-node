use clap::Parser;

use super::*;

#[test]
fn actual_rpc_options_select_observed_fees_and_fixed_override() {
    for (args, expected) in [
        (vec!["node"], FeePolicy::Unavailable),
        (vec!["node", "--mempool"], FeePolicy::Observed),
        (
            vec![
                "node",
                "serve",
                "--mempool",
                "--standalone",
                "--rpc-no-auth",
            ],
            FeePolicy::Observed,
        ),
        (
            vec!["node", "serve", "--standalone", "--rpc-no-auth"],
            FeePolicy::Unavailable,
        ),
        (
            vec!["node", "--fixed-fee-sats-per-kvb", "7", "--mempool"],
            FeePolicy::Fixed {
                sats_per_kvb: 7.try_into().unwrap(),
            },
        ),
        (
            vec![
                "node",
                "--fixed-fee-sats-per-kvb",
                "7",
                "serve",
                "--mempool",
                "--standalone",
                "--rpc-no-auth",
            ],
            FeePolicy::Fixed {
                sats_per_kvb: 7.try_into().unwrap(),
            },
        ),
    ] {
        assert_eq!(
            Options::try_parse_from(args).unwrap().fee_policy(),
            expected
        );
    }
}
