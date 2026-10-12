# ARCH-fedimint-light-node: Embeddable guardian backend

## Status

This is the requested target architecture for incremental implementation.
The Rust library implements retention-range arithmetic and a synchronous redb
store with versioned bincode anchor, identity and canonical-header records.
Atomic suffix updates and pruning preserve coherent bounds across restart.
The caller supplies all trust decisions and resource capacities; storage checks
structure, not consensus. Separate stateless functions check decoded block integrity
and declared-target PoW, using caller-supplied hash, witness activation and target
limit rather than authenticated chain context. Complete payload admission bounds
wire bytes and decoded structure before allocating and checking bodies.
A bounded v1 block-envelope boundary checks caller network, command, length and
checksum before that payload admission; it owns no transport or peer lifecycle.
Validation context, bootstrap, synchronization, body retrieval and guardian integration
below remain unimplemented.

## Boundaries and flow

The library owns header synchronization, bounded persistent chain state,
peer selection, and on-demand body retrieval. An embedding caller supplies
configuration and controls startup, cancellation, and shutdown. No separate
process, global singleton, process signal handler, or global mempool is required.
Embedding must not mandate runtime creation, global tracing configuration,
listening sockets, or process exit.
Shutdown must stop background work and complete or cancel outstanding requests
before releasing storage. A future executable or transport remains a thin caller.

Independently operated HTTPS sources establish a trusted bootstrap anchor;
Bitcoin P2P then supplies ancestry, subsequent headers, and complete
witness-bearing blocks. The validator admits headers into a bounded chain
store and admits matching bodies to callers under
[SPEC-chain-trust](SPEC-chain-trust.md). Sources are not permanent chain oracles.

The store uses redb transactions and bincode-encoded, explicitly versioned
records. It persists the anchor, rolling headers, and minimal validation context;
updates must preserve a coherent tip and retention boundary across restart.
Bodies need not persist; any body cache is optional and bounded.
[REQ-bounded-history](REQ-bounded-history.md) constrains all retained state.

The guardian-facing boundary exposes the behavior in
[REQ-guardian-backend](REQ-guardian-backend.md), without depending on Fedimint
internals. The library must remain usable in-process, including possible future
fallback use; this does not implement or authorize automatic fallback.
