# SPEC-chain-trust: Anchored chain and body acceptance

## Status

The library checks decoded block structure, output money ranges, requested hash,
Merkle mutation/duplicates, witness commitments and size/weight limits. Witness
activation is explicit caller context, not inferred or authenticated. Separate
declared-target PoW checks enforce target range and a caller-supplied limit, not
expected difficulty. Complete payload admission implements bounded canonical wire
decoding followed by these body checks.
The v1 block-envelope boundary checks network magic and checksum, neither of
which authenticates a peer, requested hash or activation context.
No full consensus validation is provided. Mainnet contextual header validation
checks linkage, expected target, PoW, timestamps and buried version floors.
Historical context is hash-linked into a caller-trusted anchor, not freshly
validated; cumulative work counts only descendants above that common origin.
The validated store atomically selects strictly higher-work bounded suffixes,
retains validation checkpoints through pruning, and persists an explicit caller
protection floor. Other networks are unsupported. Raw storage remains untrusted
and cannot open validated state. Outbound mainnet synchronization applies that
validator to bounded peer candidates and exposes selected-tip changes, including
progress committed before a later peer failure or cancellation. Unsupported remote
candidates end that peer visit without replacing protected history or stopping
other peers; local storage failures stop synchronization. Optional independently
scheduled Esplora candidates enter the same atomic selection boundary without
requiring P2P to supply their headers. Bounded reverse cursors support shallow
ongoing defense, not arbitrary deep recovery. Fresh-store bootstrap
authenticates HTTPS endpoints and requires same-height agreement, with explicit
source-trust and resource policy. P2P backfill becomes queryable only after exact
linkage into that checkpoint; pre-checkpoint rows carry no independently validated
work or fork context. Atomic publication refuses overwrite, and restart uses the
persisted checkpoint without an oracle. On-demand body delivery binds the requested
height/hash to retained selected state and derives mainnet witness activation from
that height. Exclusive ownership prevents pruning or reorganization during retrieval;
after delivery, subsequent synchronization can change unprotected selection.
Body failure does not alter headers or establish freshness. Guardian consumption
and finality integration remain unimplemented.

## Record justification

Bootstrap source agreement, independent header transports, persistent chain selection, P2P body admission,
and guardian delivery jointly enforce this trust boundary, so none alone can
own the contract coherently.

## Anchor and headers

### Independent header transports

After bootstrap, public explorer responses have the same adversarial status as
P2P header candidates. Reputation and authenticated HTTPS identities confer no
consensus authority. No agreement, advertised height or claimed work overrides
local contextual validation and relative-work selection. Any single source may
supply a supported higher-work candidate through the same retention, protection
and atomic selection boundary; its headers need not also be available over P2P.

Low-rate independent retrieval reduces dependence on P2P availability, but
proves neither source independence, tip freshness, full consensus validity nor
body availability. Bounded ancestry support deliberately limits this defense:
unavailable context and deeper gaps remain degraded rather than creating new
anchors or automatic recovery. Source failure, stale tips and unsupported
candidates cannot stop healthy other sources; actual local storage failures do.
The caller controls this additional egress. Bounded scheduling and cancellation
must not monopolize the owner for the inter-visit interval or let continuous RPC
traffic starve due security checks. Numerical limits belong in the
[operation guide](../docs/guide.md#periodic-explorer-header-candidates).

### Trusted origin and contextual admission

DNS seeds, advertised addresses, successful-peer caches and maintained connection
counts are availability hints, not additional chain authorities or a quorum.
Useful service affects connection retention, never header or body acceptance.
Discovery cannot change the trusted checkpoint or protected history.

Bootstrap accepts a network, height, and hash only after independently operated
HTTPS sources agree at the same height. Reputation alone does not establish
independence. Quorum and confirmation margin are explicit configurable policy;
agreement and a timestamp freshness check do not establish consensus or an
objectively current tip. Persist that tuple and authenticate backfilled
ancestry by hash linkage into it before exposing retained history.
The trusted checkpoint, work origin and rollback boundary remain distinct from
the rolling retained floor. Authenticated historical rows below that checkpoint
may serve matching bodies but cannot become contextual fork parents. Only
descendants earn independently validated relative work.

Subsequent headers require linkage, valid target encoding and limits, PoW,
the network's expected difficulty including retarget rules, median-time-past
and future-time checks, and enough retained context to perform those checks.
Do not confuse satisfying a header's declared target with expected difficulty.
Select competing supported branches by cumulative work relative to a common
trusted anchor, not height or a peer's assertion.

A candidate requiring context outside supported history or conflicting with the
trusted anchor or protection floor must not be applied. An untrusted peer's
unsupported candidate is not evidence that selected state needs recovery:
reject that candidate with an explicit diagnostic and permit other peers to progress.
If local state cannot safely support selected-chain service, stop affected service
with an explicit recovery-required state. Do not silently replace history already
consumed as finalized. Recovery needs explicit operator action and coordination
with the embedding guardian; neither automatic re-anchoring nor an external oracle
is a substitute.
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

Optional historical fee samples add checked amount arithmetic over already admitted
selected bodies, not spend validation. Parent txids bind output amounts without
proving unspentness or legal spending. Missing amounts must stay unknown; detected
contradictions invalidate the observation. Explicit observed fee policy may use
qualified current-tip samples without new network requests; fixed and unavailable
policies remain authoritative. Current-member raw transaction reads must not add
unconfirmed outputs to selected-body amount provenance or mutate chain protection.

Bootstrap sources can agree on a bad anchor. PoW and local mechanical checks
do not eliminate eclipse attacks or consensus-invalid high-work chains.
Guardians sharing sources, peers, or this implementation can fail together;
federation agreement is not independent Bitcoin consensus validation.

This refines [REQ-guardian-backend](REQ-guardian-backend.md) and is constrained
by [REQ-bounded-history](REQ-bounded-history.md).

Protocol references: `bitcoin/bips`, `bip-0141.mediawiki` (witness commitment
and weight); Bitcoin Core `src/validation.cpp` (header and block checks).
These references define check semantics, not a claim to implement every Core check.
