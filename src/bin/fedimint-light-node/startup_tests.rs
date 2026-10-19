use clap::Parser;

use crate::options::Options;

#[test]
fn no_arguments_select_normal_read_only_startup() {
    let options = Options::try_parse_from(["node"]).unwrap();
    assert!(options.command.is_none());
    assert!(options.db.is_none());
    assert!(options.rpc.rpc_credentials.is_none());
    assert!(!options.rpc.allow_rpc_submission);
    assert!(!options.rpc.fake_bitcoind);
    assert!(!options.rpc.mempool);
    assert_eq!(options.rpc.rpc_bind.to_string(), "127.0.0.1:18443");
    assert_eq!(options.store_config().header_capacity.get(), 52_560);
}

#[test]
fn mempool_is_explicit_opt_in_without_credentials() {
    let options = Options::try_parse_from(["node", "--mempool"]).unwrap();
    assert!(options.rpc.mempool);
    assert!(options.rpc.rpc_credentials.is_none());
    assert!(!options.rpc.allow_rpc_submission);
    options.check_command_scope().unwrap();
}

#[test]
fn explorer_egress_defaults_manual_override_and_conflicts() {
    let normal = Options::try_parse_from(["node"]).unwrap();
    assert_eq!(normal.network.explorer_config().roots.len(), 2);
    for flag in ["--manual-only", "--no-explorer-headers"] {
        let options = Options::try_parse_from(["node", flag]).unwrap();
        assert!(options.network.explorer_config().roots.is_empty());
    }
    let explicit = Options::try_parse_from([
        "node",
        "--manual-only",
        "--header-explorer",
        "https://example.org/api",
    ])
    .unwrap();
    assert_eq!(
        explicit.network.explorer_config().roots,
        ["https://example.org/api"]
    );
    assert!(
        Options::try_parse_from([
            "node",
            "--no-explorer-headers",
            "--header-explorer",
            "https://example.org/api"
        ])
        .is_err()
    );
    let misplaced = Options::try_parse_from(["node", "--no-explorer-headers", "status"]).unwrap();
    assert!(misplaced.check_command_scope().is_err());
}

#[tokio::test]
async fn shutdown_cancels_initial_bootstrap_without_publishing_or_network() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("chain");
    let options = Options::try_parse_from([
        "node",
        "--db",
        db.to_str().unwrap(),
        "--rpc-bind",
        "127.0.0.1:0",
    ])
    .unwrap();
    // Bootstrap's biased cancellation branch wins before its network future is polled.
    let shutdown = std::future::ready(Ok(()));
    tokio::pin!(shutdown);
    let error = crate::startup::run(options, shutdown).await.unwrap_err();
    assert_eq!(
        error.downcast_ref::<std::io::Error>().unwrap().kind(),
        std::io::ErrorKind::Interrupted
    );
    assert!(!db.exists());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn state_paths_use_xdg_or_home_without_relative_working_directory_fallback() {
    use crate::startup::state_path;
    assert_eq!(
        state_path(Some("/state".into()), None).unwrap(),
        std::path::PathBuf::from("/state/fedimint-light-node/chain.redb"),
    );
    for xdg in [None, Some("".into()), Some("relative".into())] {
        assert_eq!(
            state_path(xdg, Some("/home/test".into())).unwrap(),
            std::path::PathBuf::from("/home/test/.local/state/fedimint-light-node/chain.redb"),
        );
    }
    assert!(state_path(None, None).is_err());
    assert!(state_path(None, Some("relative".into())).is_err());
}
