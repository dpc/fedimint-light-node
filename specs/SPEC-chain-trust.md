# SPEC-chain-trust: Anchored chain and body acceptance

## Status

The library checks decoded block structure, output money ranges, requested hash,
Merkle mutation/duplicates, witness commitments and size/weight limits. Witness
activation is explicit caller context, not inferred or authenticated. Separate
declared-target PoW checks enforce target range and a caller-supplied limit, not
expected difficulty. Complete payload admission implements bounded canonical wire
decoding followed by these body checks; body delivery remains unimplemented.
No full consensus or contextual header validation is provided.
The header store persists caller-supplied anchors and linked canonical suffixes,
without authenticating bootstrap sources or selecting branches by work. Missing
retained ancestry produces a storage recovery error, not a node-wide recovery
state. The remaining contract describes the requested end state.

## Record justification

Bootstrap source agreement, persistent chain selection, P2P body admission,
and guardian delivery jointly enforce this trust boundary, so none alone can
own the contract coherently.

## Anchor and headers

Bootstrap accepts a network, height, and hash only after independently operated
HTTPS sources agree at the same height. Reputation alone does not establish
independence. Exact quorum and confirmation margin are unresolved policy,
not hard-coded assumptions. Persist that tuple and authenticate backfilled
ancestry by hash linkage into it before exposing retained history.

Subsequent headers require linkage, valid target encoding and limits, PoW,
the network's expected difficulty including retarget rules, median-time-past
and future-time checks, and enough retained context to perform those checks.
Do not confuse satisfying a header's declared target with expected difficulty.
Select competing supported branches by cumulative work relative to a common
trusted anchor, not height or a peer's assertion.

A fork requiring context outside supported history or conflicting with the
trusted anchor must stop affected service with an explicit recovery-required
state. It must not silently replace history already consumed as finalized.
Recovery needs explicit operator action and coordination with the embedding
guardian; neither automatic re-anchoring nor an external oracle is a substitute.
Within supported history, chain changes must be observable to callers rather
than claiming immutable finality.

## Mechanical body acceptance

Retrieve complete witness-aware blocks over P2P and bind them to a retained,
accepted header by hash. Before delivery, require bounded canonical decoding
with no trailing data; nonempty transactions; exactly one coinbase in first
position; nonempty transaction inputs and outputs; no duplicate inputs within
a transaction; no null previous outputs outside coinbase; valid coinbase script
length; and individually and cumulatively range-bounded transaction outputs.
These are local structural checks, not input-value or subsidy checks.

Check the transaction Merkle root and reject mutated Merkle trees. Enforce
block weight and serialization limits. Apply witness commitment rules,
including coinbase reserved value, commitment selection, witness root, and
absence rules, for the supported network and activation context. A matching
transaction Merkle root alone does not authenticate witness data.

Do not claim script or signature validity, input existence or unspentness,
coinbase maturity, lock-time/sequence contextual validity, input/output value
conservation, correct subsidy, or full consensus acceptance. Invalid spends,
inflation, or invalid signatures can pass this contract. Such blocks must
never be described as fully validated.

## Residual trust

Bootstrap sources can agree on a bad anchor. PoW and local mechanical checks
do not eliminate eclipse attacks or consensus-invalid high-work chains.
Guardians sharing sources, peers, or this implementation can fail together;
federation agreement is not independent Bitcoin consensus validation.

This refines [REQ-guardian-backend](REQ-guardian-backend.md) and is constrained
by [REQ-bounded-history](REQ-bounded-history.md).

Protocol references: `bitcoin/bips`, `bip-0141.mediawiki` (witness commitment
and weight); Bitcoin Core `src/validation.cpp` (header and block checks).
These references define check semantics, not a claim to implement every Core check.
