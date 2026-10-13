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
    assert_eq!(options.rpc.rpc_bind.to_string(), "127.0.0.1:18443");
    assert_eq!(options.store_config().header_capacity.get(), 52_560);
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
