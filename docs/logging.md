# Operational logging

The executable installs a human-readable `tracing` subscriber on stderr before
operational startup. The default level is `info`; `RUST_LOG=warn` reduces output,
and `RUST_LOG=fedimint_light_node=debug` enables project diagnostics. An empty
filter uses `info`. Invalid filters warn and fall back to `info` without echoing
the value. `RUST_LOG=off` suppresses even fatal diagnostics, not failure exit codes.
ANSI color is enabled only when stderr is a terminal.

The library emits events and async-safe spans but never installs a subscriber,
runtime or global logging configuration. Targets follow Rust module paths under
`fedimint_light_node`. Command results remain on `stdout`; help and version do not
initialize logging. Query command hints appear on stderr at debug level.

## Levels and fields

- `info`: startup, checkpoint agreement/publication, shutdown and selected-chain
  progress. `sync_progress` reports selection-generation changes immediately and
  unchanged state at most once per minute. Optional mempool summaries report at
  most once per minute; individual transactions are not logged.
- `debug`: normal peer acquisition, rotation, repeated visits, retry failures,
  RPC method/outcome/duration and bounded mempool coverage attempts.
- `warn`: degraded availability/cache persistence and invalid log configuration.
  Repeated or flapping degradation warns at most once per minute per CLI owner.
- `error`: fatal store/owner failures and unsuccessful command termination.

Events use named fields for heights, hashes, peers, counts, durations and outcomes
where available. Routine output reports operational state, not static policy
disclaimers. Trust and compatibility contracts live in the security and RPC guides.

Do not record credentials, authorization headers, raw transaction/block bytes,
arbitrary request bodies or unknown client-supplied method names. Instrumented
functions skip arguments by default. Bootstrap HTTP errors strip their URL before
propagation; source credentials are rejected without echoing them. Logging is
not an audit trail, and debug output from dependencies enabled by broad
`RUST_LOG=debug` is outside the project's event contract.

## Readiness tooling

Current tooling recognizes `event="rpc_listening"` with `rpc_listen`, and
`event="sync_progress"` with numeric `connections`, independently of timestamp
and target prefixes. The soak script pins `RUST_LOG=info` and records its sampling
contract. Connection histograms now sample progress/minute summaries, not every
visit; they cannot establish short-lived connection maxima. Historical campaign
logs and evidence retain their original format and meaning. Parser fixtures run
offline; changing logging does not imply a new live campaign.
