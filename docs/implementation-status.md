# Implementation status

The experimental Rust library and CLI support mainnet bootstrap, contextual
header synchronization, a bounded persistent header store, on-demand complete
witness-bearing blocks, and explicit transaction submission. A managed outbound
pool discovers public peers, retains useful connections and rotates an
exploratory connection. Manually configured peers and a manual-only mode remain
available. Discovery supplies availability hints, never chain authority.
Periodic [Esplora header candidates](guide.md#periodic-explorer-header-candidates)
add independent, low-rate defense against P2P withholding. They pass the same
contextual validation and work selection as P2P, not a reputation-based quorum.

The loopback [guardian RPC profile](guardian-rpc.md), with optional authentication,
supports the narrow Fedimint client profile. `--fake-bitcoind` explicitly enables
synthetic Core status.
Validation covers contextual headers and block integrity. UTXO/script validation
and automatic guardian commit coordination remain outside the current implementation.
