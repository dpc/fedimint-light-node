# Fedimint light node

An embeddable Rust Bitcoin backend for guardians that retain recent headers
and retrieve complete blocks from peers without full consensus validation.
This is an incremental implementation, **not a working node or a Bitcoin Core substitute**.

The library provides tested retention-window arithmetic and a synchronous,
bounded redb header store with versioned bincode records. Callers supply the
anchor, chain identity, header capacity and database cache budget. Atomic suffix
replacement and pruning preserve the pinned identity across restart. Storage
checks linkage and record consistency, **not Bitcoin header validity or trust**.
The separate `block_integrity` module checks caller-owned decoded blocks against
a requested hash, transaction structure and money ranges, Merkle and witness
commitments, duplicate defenses, serialization size and weight. Callers explicitly
select pre-activation or active witness rules from trusted context. The separate
`declared_pow` module checks target range and PoW against the declared target and
a caller-supplied network limit, **not expected difficulty or header validity**.
Neither integrity API authenticates the requested hash or establishes consensus acceptance.
The separate `HeaderContext` and `ValidatedHeaderStore` APIs implement contextual
mainnet header validation and bounded canonical selection as described below.
The `HeaderSync` API retrieves actual mainnet headers and complete matching
witness-bearing blocks on demand over outbound Bitcoin P2P.
There is no bootstrap authenticator, guardian adapter, or executable.
The intended boundaries are described in
[ARCH-fedimint-light-node](specs/ARCH-fedimint-light-node.md).

## Development

Enter the development environment with `nix develop` (or allow `.envrc`).
Run `treefmt`, `nix build -L .#ci.tests`, and `nix develop -c selfci check`.
The test gate runs real workspace tests with nextest; SelfCI also checks
formatting, prose, lockfile freshness, builds, and Clippy.

## Unsettled integration choices

Networks beyond mainnet, bootstrap quorum and confirmation margin, fee policy,
and a Fedimint Rust adapter versus a small RPC transport remain undecided.
No resource capacities or production defaults have been chosen. The store starts
at a caller-supplied anchor; queryable backfill, schema migration and capacity
resizing are not implemented. The validated store retains bounded context separately
from queryable headers. Record limits
and an explicit cache budget are not a measured RSS or physical file-size bound.
Actual fallback integration into Fedimint remains outside this implementation.

## Contextual mainnet headers

`HeaderContext::from_trusted_history` takes an explicitly trusted anchor plus the
exact bounded historical range returned by `context_start`. Hash linkage into the
anchor authenticates that history; it is not freshly validated, and its work is
not counted. Descendants must link, use the expected compact target (including
mainnet retargets), satisfy PoW, exceed median time past, stay within two hours of
an injected local clock, and satisfy buried header-version floors. Non-mainnet
networks return an explicit unsupported error. These checks do not validate spends,
scripts, transactions, bootstrap source agreement, or the freshness of the chain.

`ValidatedHeaderStore` persists relative work above the anchor and a fixed-size
timestamp/retarget checkpoint for each retained header. Even a capacity of one
can advance through a retarget after restart. A candidate may start at a retained
parent and replace the suffix only with strictly greater work; ties keep the
selected chain. Candidate batches and checkpoints are bounded by header capacity.
There is no orphan pool, persistent fork cache or archival recovery.

Call `protect_through(height, hash)` before promising a selected header as finalized
to a consumer. The monotonic protection floor survives pruning and restart; it is
not inferred from confirmation counts. Forks crossing that floor, the anchor, or
available context return `RecoveryRequired` without altering state. An unsupported
remote assertion does not prove selected state needs recovery; synchronization
rejects that peer candidate and permits others to progress. Actual local failures
stop affected service and require explicit recovery. Supported chain changes are
observable through explicit store calls or synchronization status.

Schema v2 commits headers, work, checkpoints and pruning atomically. Raw and
validated stores cannot open each other's mode; raw storage cannot promote
unvalidated rows or bypass admission. Schema v1 is rejected, not silently migrated.
Restart checks bounded records and retained context transitions, but does not
authenticate a database against malicious replacement. Check semantics follow
Bitcoin Core v30.0 `GetNextWorkRequired`, `ContextualCheckBlockHeader`,
`GetMedianTimePast`, and mainnet chain parameters, with `rust-bitcoin` 0.32.102
compact-target arithmetic. This is header-level checking, not full consensus.

## Outbound header synchronization

`HeaderSync::new(store, config)` takes an initialized `ValidatedHeaderStore` and
explicit limits. Repeatedly await `step(now)` on a caller-owned Tokio runtime
with I/O and time enabled. Each step visits one configured socket address, performs
a mainnet v1 version/verack handshake, and requests headers from bounded locators.
It validates expected difficulty, PoW and header context before selecting strictly
higher relative work. Canonical progress commits in bounded chunks; a supported
fork can accumulate across requests in that visit, but not across separate visits.
Small store windows can consume protocol-sized 2,000-header replies without storing
the entire reply at once. No live Internet access is needed by tests.

The service visits peers sequentially and rotates after every attempt, including
timeout, failure and cancellation. Every step incurs explicit retry pacing and has
a fixed request budget. Absolute connect/handshake and request deadlines include
all reads, writes and chatter. There is one socket, request and candidate globally
per service, at most 16 configured addresses, no orphan pool, and no task/event
queues. Each exchange admits at most 32 frames and 2 MB; the largest buffered
payload is 162,003 bytes, and ignored bounded chatter streams through a 4 KiB
scratch buffer. Header decoding adds at most 2,000 fixed-size headers; candidate
storage is separately configured up to 32,000 headers and cannot exceed store
capacity. Temporary store validation checkpoints also remain capacity-bounded.
Unsolicited blocks are rejected before payload reads. This is bounded application
state, not a measured RSS guarantee; caller-created service instances multiply it.

Inspect each `SyncReport` for the attempted peer and its typed result, and inspect
`status()` for selected height/hash and an in-memory selection generation.
Advertised heights are ignored. Empty responses are observations, **not proof of
freshness or an honest chain**. Generations expose changes even if a later exchange
fails; they reset on restart and are not a durable notification log. Intermediate
tips may be coalesced within one visit. Coordinate consumer reads between visits
and call `protect_through` before promising finality.

To cancel or shut down, drop the pending step future, then inspect status and drop
the service or recover storage with `into_store()`. Sockets and candidates drop;
no detached work survives. A completed atomic chunk remains committed. Validation
and synchronous redb operations between awaits cannot be interrupted and can block
the executor; latency-sensitive callers should use a dedicated caller-owned runtime
thread. Unsupported remote candidates stop only their visit, without changing
protected history; local storage/invariant failures latch the service stopped.
There is no automatic re-anchoring or recovery.

Tokio supplies maintained transport/deadlines; `rust-bitcoin` supplies message
encoding, hashes and fixed header decoding. A narrow admission layer is used
instead of its broad allocating network decoder: wire lengths, header counts,
zero transaction counts and version user-agent lengths are checked before variable
decoding. This avoids adding a general P2P framework for a single outbound flow.
Only a bounded set of handshake and ordinary control/announcement messages is
accepted. There is no peer discovery, inbound service, BIP324 transport, parallel
peer diversity, transaction relay, or production-readiness claim.

## On-demand blocks

Call `HeaderSync::fetch_block(height, hash, config)` for a retained selected header.
The method checks the height/hash before connecting and applies mainnet witness
rules at height 481,824, then binds the complete body to that hash. It uses the
existing bounded decoder, Merkle/witness commitments and mechanical integrity
checks, not full consensus validation. No bodies persist or remain cached.

The method exclusively borrows the same service used for header synchronization:
one socket, payload and decoder globally per instance, no concurrent header
selection, pruning or downloads. No database transaction crosses an await.
Selection is still canonical when the method returns, but a later `step` may
replace unprotected history. Retrieval does not automatically protect consumed
history; call `protect_through` before promising finality.

`BlockDownloadConfig` explicitly bounds attempts (at most 16 and no more than one
per configured peer entry), total duration (at most 120 seconds), each complete visit
(at most 60 seconds), aggregate received bytes across all attempts (at most 64 MB),
payload bytes (at most 4 MB), and decoded elements (at most one million).
Both deadlines include control messages and fragmented reads; total duration also
includes retry pacing. Handshake and response each admit at most 32 messages.
Malformed/partial envelopes count against the byte budget; admitted payloads reserve
their entire declared length before reading. Each visit makes one witness-aware
`getdata` request. The payload and decoded result briefly coexist, and integrity
scratch space scales with the admitted structure; these are not exact RSS limits.
Caller-owned returned blocks and caller-created service instances need separate
accounting.

Only peers advertising both `NODE_NETWORK` and `NODE_WITNESS` qualify. BIP159
limited-only peers guarantee their own last 288 blocks, not our retained window.
Even our locally recent tip may be stale, so this implementation conservatively
requires full-serving peers for every body. Advertised height is never evidence
of freshness or availability. BIP144 witness requests still use the header hash
and receive ordinary `block` messages, including for legacy blocks.

`OutsideRetainedHistory`, `BeyondTip`, and `NotCanonical` are distinct from
transient `Unavailable`, which reports attempts and the last peer/budget failure.
Bad hashes, checksums, malformed bodies, witness failures, `notfound`, withholding
and unsuitable peers end that visit and permit bounded retries with other peers.
Any `notfound` ends the visit without interpreting its inventory; it cannot invalidate headers.
Local state failures stop the service. No archival recovery or fallback occurs.

Drop the future to cancel: the socket, payload and exclusive download slot release
without store writes or leaked tasks. Synchronous bounded decoding cannot be
preempted; the service rechecks its deadline after decoding and refuses late
success. Use a caller-owned dedicated runtime thread if that latency is unacceptable.
Availability is not a chain freshness, finality or consensus-security claim.

## Integrity boundary

Body checks borrow an already decoded block. An allocation-free counting sink
enforces the caller's byte budget (capped at 4 MB) before scratch allocation and
hashing. Scratch trees and sets scale with accepted transaction/input counts;
the caller owns existing block allocations. The `block_decode` entry point first
scans complete payloads without allocation, enforcing explicit byte and aggregate
element budgets and rejecting malformed lengths, encodings that are not canonical,
truncation and trailing bytes. It then uses `rust-bitcoin` decoding and the same
integrity checks. Structural budgets bound decoded storage growth, not exact
allocator bytes or RSS. The service above supplies global download limits;
process memory accounting remains incomplete. A body rejection must not poison the
requested header or prevent retrying a different body.

The `v1_block_frame` boundary admits only Bitcoin v1 `block` envelopes. Callers
collect a fixed 24-byte header, admit its network, command and bounded length,
then collect exactly the admitted payload length before checksum and block
decoding. Header admission and checksum checks allocate nothing. All other
commands are rejected; this is not a peer dispatcher or handshake. There is no
BIP324 support, and the checksum does not authenticate a peer or block. Callers
own fragmented reads, EOF handling, deadlines, buffers and concurrent budgets.
The wire layout and checksum byte order follow Bitcoin Core v30.0
`CMessageHeader` and `rust-bitcoin` 0.32.102 `RawNetworkMessage`.

The mechanical checks deliberately do not execute scripts, consult a UTXO set,
check subsidy/value conservation, contextual transaction validity, signature
operation limits, coinbase height or signet solutions. Invalid spends and inflation
can pass. Header linkage, timestamps, expected difficulty, authenticated anchors
and branch selection are not established by either integrity API.

Check semantics were compared with Bitcoin Core v30.0 `CheckTransaction`,
`CheckMerkleRoot`, `CheckWitnessMalleation`, `DeriveTarget`, and
`arith_uint256::SetCompact`, BIP141, and the pinned `rust-bitcoin` source. The API
does not claim to reproduce all Core validation. In particular it avoids
the dependency's witness convenience check, which accepts a present commitment with
an absent reserved value when all witnesses are empty.

## AI usage disclosure

[I use LLMs when working on my projects.](https://dpc.pw/posts/he-went-to-the-slop-side/)
