# REQ-guardian-backend: Guardian interoperability without a full node

## Status

An authenticated loopback RPC profile supports the pinned Fedimint bitcoind
client's seven methods. Its Core-status compatibility requires explicit
default-off `--fake-bitcoind`; the synthetic field inventory and wire dialect
are documented in the [RPC guide](../docs/guardian-rpc.md).
The library and CLI now support bounded caller-driven transaction submission;
completed writes do not establish peer acceptance, propagation or confirmation.
Cancellation can follow delivery; retry remains an explicit caller decision.
The concrete owner also exposes checked selected count, pinned height-one identity,
retained height-to-hash queries and unavailable synchronization progress. Reads and
body retrieval do not protect history. Fee policy defaults to unavailable or is an
explicit positive operator-supplied static rate in satoshis per 1,000 virtual bytes,
not a network estimate. The embedding example demonstrates these operations;
consumer database finality coordination and end-to-end guardian deployment remain absent.
Explicit local guardian binding, hash-checked administrative protection and startup
ancestry/range preflight now support manual operator checkpoints. Stopped-store
snapshots and conservative restore require independently current safety records;
they cannot establish guardian database recency or coordinate future commits.

## Authority and scope

The requester targets guardians, not client history queries, and explicitly
accepts PoW and mechanical validation without full consensus validation.
The interoperability source is Fedimint's `IServerBitcoinRpc` in
`fedimint-server-core/src/bitcoin_rpc.rs`.
The bootstrap was checked against Fedimint commit
`502b7ec7da37d231b21d5bce993e9b9a9e7b63a6`; integration must recheck that interface.

Return block count as selected tip height plus one; do not subtract wallet
finality delay. The Core wire method returns height, since Fedimint adds one.
Resolve retained heights to hashes and retrieve complete
matching blocks, provide synchronization progress, submit transactions to
peers, and expose the supported network's chain identity. Pin network identity
metadata (Fedimint uses the height-one hash) rather than retaining genesis
history solely to answer identity queries.

Expose permanent retention errors separately from transient peer failures.
Existing wallet callers retry block errors indefinitely; adapter integration
must address this mismatch rather than disguising permanent failure.
Transaction broadcast success means submission, not confirmation or acceptance.

Fee estimation is an explicit integration decision, not an inferred property
of header state. Returning no estimate can support walletv1 defaults but does
not universally satisfy walletv2 fee voting. Do not add a global mempool
subsystem implicitly to solve this.

The requester explicitly permits fake Core status only with `--fake-bitcoind`.
Actual selected height, hash, network identity, bodies and fee units remain real.
Synthetic status must identify its fabricated fields, never bypass local safety
checks, and never imply full validation, objective freshness or mempool acceptance.
Standard reads do not communicate guardian commitment and must not advance
durable protection. The initial RPC profile is not automatic guardian-finality
coordination; production use still requires an explicit consumer protection policy.

No UTXO set, script execution, contextual spend or value validation, client
script-history API, archival recovery, or automatic fallback is required.
The security boundary is [SPEC-chain-trust](SPEC-chain-trust.md); resource and
availability limits are [REQ-bounded-history](REQ-bounded-history.md).
