# Detailed operation and library guide

This experimental Rust backend bootstraps mainnet history, synchronizes recent
headers, retrieves complete blocks and submits caller-supplied transactions.
It exposes fixed fee configuration and optional historical fee observations.
See [security boundaries](../SECURITY.md) for the trust model and
[guardian RPC](guardian-rpc.md) for integration contracts.

## Periodic explorer header candidates

Normal startup, `serve` and ongoing `sync` enable two HTTPS transports:
`https://mempool.space/api` and `https://blockstream.info/api`. Any one source can
supply a supported higher-work branch through the same contextual validation and atomic
selection as P2P, even when every P2P peer withholds headers. This adds defense in
depth against P2P withholding.

Use `--no-explorer-headers` to disable this periodic traffic. Repeated
`--header-explorer HTTPS_API_ROOT` replaces the defaults; roots must be
credential-free HTTPS without a query or fragment. Up to eight distinct
host/port pairs are accepted. `--manual-only` suppresses the default periodic
explorers, but explicit header roots opt back in. It does **not** suppress existing
trusted initial HTTPS bootstrap. Trusted bootstrap `--source` policy remains
separate. One-shot operations, including `sync --once`, do not start this monitor.
Library constructors remain disabled; configure `ExplorerHeaderConfig` explicitly
and drive `explorer_step`, `step_with_explorers`, or `GuardianRpc`.

Each source has an independent initial delay of 0–30 minutes and subsequent
15–45 minute visit intervals. A visit attempts at most 16 requests within 120
seconds, including failures and cancellations. One application HTTP request is
active at a time, with at least one second between starts and a three-second
whole-request deadline. Standard tip-hash and single raw-header replies have
65-byte and 161-byte application body limits. No redirects, environment proxies,
retries or decompression are accepted. These are not HTTP-header, TLS, DNS or
process-memory limits, nor a hard HTTPS socket cap.
An attempt starts only if its full HTTP budget fits the remaining visit window.
Synchronous admission and runtime scheduling can still finish later; the visit
window is not a hard wall-clock completion guarantee.

The owner gives due checks explicit opportunities, not just idle time. RPC serves
cheap reads between bounded operations; a single HTTP request can add up to three
seconds of foreground delay. Caller-driven `sync` preserves configured P2P
exchange deadlines: maintenance runs during pacing and between exchanges, not by
repeatedly canceling a connection before it can be established. A long configured
discovery, handshake or exchange can defer maintenance; the cadence is not a strict
wall-clock guarantee. The existing four-P2P-socket limit is unchanged.

Each source pins a snapshot and follows hashes backwards until it reaches usable
retained ancestry. It retains at most **256 unknown headers**, further capped by
the configured candidate and store capacities. This adds an independent cache
budget of at most 160 KiB of raw header payload across eight sources, plus bounded
metadata and allocation overhead, outside the single P2P candidate budget.
Cursors survive visits and cancellation, but not process restart. Connected
suffixes validate completely before publication; unsupported depth, protection or
ancestry limits never authorize re-anchoring or automatic recovery.

**This is shallow ongoing defense, not arbitrary deep catch-up.** A snapshot
requiring 256 unknown headers needs up to 17 successful visits, roughly eight
hours at the mean cadence before foreground delays or failures. A larger
pre-existing gap may not advance at all through these transports. Pinned snapshots
avoid endlessly chasing a moving tip, but do not guarantee keeping up.

Failures stop only that source's visit. Local exponential backoff starts at
45–60 minutes and caps at 18–24 hours, never earlier than its ordinary deadline.
HTTP 429, and 503 with `Retry-After`, honor seconds or HTTP dates up to seven days;
larger/overflowing valid delays or overlong hints suspend that source for the
instance rather than retrying too early. Other malformed hints use local backoff.
Successful completed snapshots reset the exponent. State is instance-local, so
restarts do not preserve rate reservations. Actual local store failure still stops
the owner; explorer disagreement, stale data and unsupported candidates do not.

See [SPEC-chain-trust](../specs/SPEC-chain-trust.md) for the governing trust boundary
and [SECURITY.md](../SECURITY.md) for residual HTTP/DNS cancellation limits.

## Why a light node for guardians?

Fedimint already delays consumption of Bitcoin blocks behind the observed tip.
That finality delay gives each guardian room to follow the chain with the
**greatest cumulative valid-header proof of work**, anchored by external
bootstrap references, while retaining only a bounded recent window. The goal is
a lightweight, independently operated Bitcoin backend for each guardian, rather
than a full historical node and UTXO set per guardian. This is especially useful
in a BFT federation where guardians each run their own backend and participate
in federation consensus.

The delay and BFT consensus do not prove Bitcoin script or UTXO validity, or
eliminate correlated source failures and eclipse risk. Existing guardians agree
on block counts and independently fetch bodies; they do not thereby agree on
an authenticated hash chain. Here, external references establish the initial
trusted checkpoint; contextual header validation and cumulative work select its
descendants. Complete blocks still require download and mechanical integrity
checks. A narrow guardian RPC profile exists; automatic consumer finality
coordination and end-to-end guardian deployment remain future work.

## Implementation

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
The `bootstrap` API initializes a fresh store from trusted HTTPS source agreement
and bounded P2P ancestry. A thin executable exposes these library operations;
the [guardian RPC](guardian-rpc.md) adds loopback service with optional authentication
and a mainnet walkthrough. There is no validated mempool or fee estimator.
Explicit `--mempool` adds a bounded, unvalidated
[transaction observation view](guardian-rpc.md#approximate-mempool-observation)
and background recent-body downloads; it remains off by default.
The same opt-in mode exposes
[historical fee observations](guardian-rpc.md#historical-fee-observations) from
already inspected blocks, with missing-input diagnostics and fixed-sample median
bounds. Fee observation makes no additional requests and does not change fee policy.
The intended boundaries are described in
[ARCH-fedimint-light-node](../specs/ARCH-fedimint-light-node.md).

The [local release resource exercise](resource-exercise.md) records measured
raw-store rollover, smaller real validated-chain storage, body-decoding pressure
and scripted-peer failures. These workload-specific results are not production
memory or disk guarantees.

## Operational walkthrough

Build the release executable with `cargo build --release` inside `nix develop`,
or run `nix build` and use `./result/bin/fedimint-light-node`. `nix run .# -- --help`
also exposes the packaged executable. The library does not require this process.
The CLI supports mainnet only; only the explicit `serve` command opens a
loopback RPC listener. Public peer discovery
is enabled by default; explicit numeric peer addresses are optional.

Use a private directory on a local filesystem supporting hard links and durable
directory sync. Repeating `--peer` adds up to sixteen explicit addresses.
Use `--manual-only` to disable automatic discovery for private deployments.
Peer configuration and prefix diversity do not guarantee eclipse resistance.

```sh
mkdir -m 700 node-data
NODE=./result/bin/fedimint-light-node
DB=./node-data/headers.redb

# New path only. Default sources: Blockstream and mempool.space, both must agree.
"$NODE" --db "$DB" bootstrap

# Offline: opens the pinned checkpoint, never repeats bootstrap or creates a DB.
"$NODE" --db "$DB" status

# One bounded visit, or paced visits until Ctrl-C / SIGTERM.
"$NODE" --db "$DB" sync --once
"$NODE" --db "$DB" sync
```

Status prints the pinned mainnet height-one identity and checkpoint, selected tip
height/hash and **count = height + 1**, inclusive retained bounds, capacity and
protection height and guardian binding. No wallet finality delay is subtracted.
Only one process should own a database at a time: stop sync before running offline
status, fetch, broadcast or protect against the same database. redb enforces exclusive opens.

Choose a retained height and its expected hash (for example, copy `selected_height`
and `selected_hash` from status). Fetch checks that selection before requesting it:

```sh
HEIGHT=... # retained height from status
HASH=...   # expected selected hash at that height
"$NODE" --db "$DB" fetch --height "$HEIGHT" --hash "$HASH" \
  --output ./node-data/block.raw
# Only when your consumer deliberately commits to that selected history:
"$NODE" --db "$DB" protect --height "$HEIGHT" --hash "$HASH"
```

Fetch writes canonical Bitcoin block serialization, including witness data, not a
P2P envelope or debug output. Omitting `--hash` uses the currently selected header
at `--height`. It publishes a complete new file without replacing existing paths,
including symlinks, hard links and the database itself. Failures before publication
leave no output; an error after publication may leave a complete file. Inspect it
rather than retrying with deletion or overwrite. Fetch never automatically protects
history. Protection is persistent and monotonic; a wrong hash fails, and there is
no unprotect operation. Applications must coordinate protection with their own
durable consumer state before promising irreversible results.

Submit a canonical **binary** raw transaction from a trusted regular file (not
hex text, a PSBT, symlink or FIFO):

```sh
"$NODE" --db "$DB" broadcast --input ./node-data/transaction.raw
```

Broadcast tries up to three pool peers sequentially, with 20 seconds per
visit, 60 seconds total and five seconds of pacing before each new dial.
Reused connections do not repeat handshake or dial pacing.
It admits at most 400,000 bytes and 400,000 weight units; these are resource
ceilings, **not** script, spend, fee or standardness validation. It reserves at
most 2 MB received and 1,210,000 sent bytes across the whole call, including
handshakes and incomplete frames. No transaction is persisted or retried later.
Output reports identifiers and each attempt; exit success requires at least
one completed local payload write, not mempool acceptance, propagation or
confirmation. Cancellation may follow delivery and loses the in-flight report;
explicit retry/rebroadcast is allowed. See [submission semantics and protocol
rationale](transaction-submission.md).

All commands accept `--capacity` (1..105,120; default 52,560) and `--cache-mib`
(1..64; default 8). Capacity must match creation on every reopen; it cannot resize
an existing database. Example: add `--capacity 2016 --cache-mib 1` to **each**
invocation for a smaller experimental store. These are logical state and cache
budgets, not a measured RSS or file-size promise.

### Explicit fee policy

`HeaderSync::new` defaults to `FeePolicy::Unavailable`; use
`with_fee_policy(FeePolicy::Fixed { sats_per_kvb })` to supply a positive
`NonZeroU64` rate. Units are **satoshis per 1,000 virtual bytes**, not sat/vB
or sat/kwu. The value is returned unchanged: 2,500 means 2.5 sat/vB.
Zero is rejected rather than treated as an unavailable or adaptive policy.
There is no HTTP fee service, mempool or UTXO estimator, confirmation-target
model, or guarantee that a fixed rate will confirm. Policy does not compute or
validate fees of submitted transactions.

All CLI operations accept `--fixed-fee-sats-per-kvb INTEGER` and report the
policy as `fixed-operator-supplied`. Serving with `--mempool` and no fixed override
selects `FeePolicy::Observed`; otherwise fees are unavailable. Configuration
belongs to the invocation, **not the database**: supply it on each restart.

```sh
fedimint-light-node --db ./node-data/headers.redb \
  --fixed-fee-sats-per-kvb 2500 status
```

The [guardian RPC profile](guardian-rpc.md#fee-policy-for-the-guardian-adapter)
documents observed-rate selection, wire units and the pinned Fedimint interface.
Library callers can explicitly choose `Observed` or retain the `Unavailable`
opt-out independently of enabling observation.

### Network operation limits

Network command help exposes exchange deadlines, pacing and operation-specific
limits. Defaults are 20-second exchanges, five seconds before each sync/fetch
connection, four header requests per sync visit, and at most 8,000 candidate
headers (capped by capacity). Every failure and empty response pays the pacing
delay. Fetch allows at most sixteen attempts, 120 seconds total, 32 MiB received,
a 4 MB payload and one million decoded elements; no body cache persists.
Bootstrap defaults to ten minutes total, 128 header requests and 32 MiB received
P2P data. These fixed CLI ceilings remain configurable through the library.
Bootstrap has its own bounded attempt policy and no continuing retry loop.

Bootstrap source overrides replace, rather than augment, the two defaults:
repeat `--source 'operator=https://host/api'` and set `--quorum` if appropriate.
At least two distinct operator identities and a strict majority are required;
any observed disagreement fails closed. Naming two operators does not prove their
independence. `--confirmation-margin` controls the checkpoint's successor-block
margin, not guardian finality. Review the [bootstrap trust policy](#initial-trusted-bootstrap)
and [security boundaries](../SECURITY.md) before relying on any source set.

Errors go to stderr and return nonzero. `sync --once` returns failure for a failed
visit; continuous sync logs retryable peer failures and rotates, but stops on
terminal local failures. Selected progress can commit before a later failure.
SIGINT/SIGTERM cancels awaited P2P work and exits 130 (a clean `serve` shutdown exits
zero); sync prints selected state
after cancellation. Synchronous database, validation and publication work cannot
be interrupted between awaits. HTTPS OS resolver jobs can outlive cancellation
and delay runtime shutdown; neither the CLI nor the library promises a hard
wall-clock shutdown bound. Crashes can leave private staging files. Never delete,
replace or re-anchor the database automatically on an operational failure.

## Embed without a standalone process

The compiling [`examples/embed.rs`](../examples/embed.rs) walkthrough explicitly
bootstraps a new store or opens a pinned store offline, queries count/identity/fees,
drives one bounded peer visit, fetches a selected block, optionally protects and
submits a caller-owned transaction, then reopens to inspect durable protection.
It owns its Tokio runtime and deadline; there is no library-installed
signal handler, global logging setup, detached maintenance task or listener.
Replace its deadline future with your application's cancellation channel or token.

```sh
PEER=127.0.0.1:8333 # your own node; this example deliberately uses manual-only mode
cargo run --example embed -- --db ./node-data/headers.redb --peer "$PEER"
# Explicitly opt into persisting protection for the returned selected block:
cargo run --example embed -- --db ./node-data/headers.redb --peer "$PEER" --protect
# Explicit NEW-store creation, using the documented default trusted sources:
cargo run --example embed -- --db ./node-data/new.redb --peer "$PEER" --bootstrap
# Optional submission of canonical binary transaction bytes, not hex or a PSBT:
cargo run --example embed -- --db ./node-data/headers.redb --peer "$PEER" \
  --fixed-fee-sats-per-kvb 2500 --transaction ./signed-tx.raw
```

Use `bootstrap(path, config, now).await` only for initial creation, then
`ValidatedHeaderStore::open_pinned` for restart. `HeaderSync` exclusively owns the
store across `step` and `fetch_block`; both borrow it mutably so selection cannot
change during delivery. Cancel by dropping the pending future, then inspect
`status` because earlier batches may have committed. `protect_through(height, hash)`
persists a rollback floor before you expose irreversible consumer results;
`into_store` returns storage ownership. Do not run the next sync visit between
fetch and a required protection decision.

`block_count()` returns checked selected tip height plus one, including height
zero and without subtracting wallet finality delay. A supported higher-work reorg
can reduce it. `chain_id()` returns the pinned **height-one** hash, even after
pruning; `block_hash(height)` only resolves retained selected heights.
`sync_progress()` returns `None`, not a guessed percentage or a freshness claim.
Neither count nor fetch implicitly protects the tip. Protection starts at the
trusted anchor and is durable and monotonic; for older already-protected ancestry,
`protect_through` verifies the retained hash and returns success without lowering
the floor. A pruned hash must match the exact persisted protected checkpoint.
See [guardian operations](operations.md) for binding, preflight and safe recovery.

The example's optional submission shares the same exclusive owner and finite
budgets as the CLI. Completed writes do not prove receipt, acceptance, propagation
or confirmation. Cancellation can follow delivery and lose the report; the caller
must decide whether to retry. Fee policy neither creates transactions nor checks
that they pay the configured rate.

The example returns typed library errors rather than hiding them behind a retry
loop. Real integrations should distinguish permanent outside-history requests
from transient body unavailability, and terminal local errors from retryable peer
failures. They must also design crash ordering with their own durable state.
Keep synchronous redb work off latency-sensitive runtime threads when needed.
Returned block allocations belong to the host; retaining many blocks defeats
the library's one-download memory bound.

## Resource and failure exercises

These paths support measurement; they do not establish resource guarantees.
Use a disposable private directory and record capacity, cache, peers, selected
heights, duration, and build profile alongside results. Never share live production
database files with a second process.

```sh
# GNU time reports elapsed time and peak RSS for initial bootstrap.
/usr/bin/time -v "$NODE" --db ./node-data/measure.redb --capacity 2016 \
  --cache-mib 1 bootstrap --peer "$PEER"
du -B1 ./node-data/measure.redb
du --apparent-size -B1 ./node-data/measure.redb

# Measure bounded header progress separately from startup/HTTP.
/usr/bin/time -v "$NODE" --db ./node-data/measure.redb --capacity 2016 \
  --cache-mib 1 sync --peer "$PEER" --once

# Finite unreachable/stalled-peer exercise; replace port if it is in use.
"$NODE" --db "$DB" sync --peer 127.0.0.1:1 --once \
  --timeout-seconds 1 --retry-seconds 1

# Deterministic local tests; no public network is used.
cargo test --test cli
cargo test --test header_sync --test block_download
```

On NixOS use `nix shell nixpkgs#time` and `time -v` if `/usr/bin/time` is absent.
Repeat fetch measurements with **new output filenames**, compare serialized body
size with peak RSS, then remove only the disposable block files deliberately.
Interrupt continuous sync and reopen status to check committed progress. Try an
out-of-range fetch, wrong protection hash, existing bootstrap path and existing
fetch output; each should fail without recovery or overwrite. Library tests also
exercise loopback stalls, invalid frames, cancellation and deadline exhaustion.
Longer steady-state runs, physical database growth under pruning, real peer
availability, HTTPS resolver behavior and guardian crash integration still require
operational measurement; no production readiness is claimed.

## Development

Enter the development environment with `nix develop` (or allow `.envrc`).
Run `treefmt`, `nix build -L .#ci.tests`, and `nix develop -c selfci check`.
The test gate runs real workspace tests with nextest; SelfCI also checks
formatting, prose, lockfile freshness, builds, and Clippy.

## Unsettled integration choices

Networks beyond mainnet, automatic consumer finality binding and a measured
network fee estimator remain unimplemented. The narrow RPC transport supports
explicit static, observed or unavailable fee policy.
Bootstrap supplies conservative configurable defaults; lower-level APIs still take
explicit capacities. Schema migration and capacity resizing are not implemented.
The validated store retains bounded context separately
from queryable headers. Record limits
and an explicit cache budget are not a measured RSS or physical file-size bound.
Actual fallback integration into Fedimint remains outside this implementation.

## Initial trusted bootstrap

Call `bootstrap(path, config, now)` on a caller-owned Tokio runtime with I/O and
time enabled. `BootstrapConfig::peer_pool` enables mainnet discovery by default;
`peers` optionally adds manual addresses. Discovery never establishes chain trust.
The default HTTPS sources are
Blockstream Esplora and mempool.space, with **2-of-2 agreement at the same height**.
The checkpoint is six **successor blocks** behind the lower reported tip
(seven confirmations when counting inclusion). These are implementation defaults,
not a user-mandated finality rule. More sources require at least two operators and
a strict-majority quorum. Failed requests cannot lower that quorum; any observed
same-height hash disagreement or wrong network aborts even with a majority.
Operator independence is a configuration trust assertion, not something TLS,
different domain names, or source reputation proves.

The default window retains 52,560 headers, approximately one year at ten minutes
per block, with an 8 MiB database cache. Capacity can range from one to 105,120.
The chosen checkpoint must be no more than 48 hours old or two hours ahead of the
supplied trustworthy Unix clock. This is a stale-checkpoint sanity check, **not
proof of current tip freshness or Bitcoin consensus validity**. Sources can collude,
share infrastructure, or agree on a consensus-invalid chain.

HTTPS provides mainnet identity checks, reported tips, same-height hashes, and a
checkpoint and seed header. The official Esplora API documents individual header
requests and ten-block listings, not a large raw-header-range endpoint. Bootstrap
therefore pages forward over P2P `getheaders` in up to 2,000-header replies, rather
than issuing a request for every historical header. A year takes about 27 replies.
The candidate seed starts at the earlier of the serving floor and required
checkpoint timestamp/retarget context. Its claimed hash/height is not a second
trust anchor: exact count and hash linkage must reach the agreed checkpoint before
any history becomes public. Bodies remain exclusively on-demand P2P.

The immutable checkpoint stays separate from the rolling serving floor.
Pre-checkpoint rows are authenticated trusted history, not independently
context-validated headers; they have no work or contextual fork checkpoints.
They can serve matching bodies, but cannot be fork parents. Work starts at zero
at the checkpoint and counts only independently admitted descendants. Existing
per-header checkpoints preserve descendant validation context through pruning.

Bootstrap bounds sources (eight), peers (sixteen), history, HTTP requests
(`4 * sources + 3`), response bodies (at most 160 bytes), and P2P bytes/requests
globally across peers. Defaults allow 128 `getheaders` requests, 32 MiB received
P2P bytes, 20-second whole exchanges, and ten minutes of total network work.
Each P2P exchange also caps frame counts. Redirects, environment proxies,
compression, automatic HTTP retries, and unencrypted production HTTP are disabled.
The small HTTP body bound is not a bound on TLS/HTTP transport buffers.
The in-memory history has at most `max(capacity, 2016)` headers, plus one protocol
reply. Bootstrap issues one application request at a time, and owns at most one
P2P socket. The HTTP client's connection setup may race resolved addresses, and
its OS DNS jobs can outlive a canceled request and delay runtime shutdown.
Repeated canceled attempts can overlap those jobs; see [SECURITY.md](../SECURITY.md)
for embedding assumptions. Library-owned logical
records, buffers and cache have finite budgets, not a measured RSS/disk-size promise.

All awaited network exchanges finish before private database preparation. A sibling temporary
directory and an atomic no-overwrite hard link publish only a complete initialized
database, followed by a parent-directory sync. Cancellation before publication
leaves no public store; process crashes may leave private staging files for
operator cleanup. Synchronous validation/creation between awaits is bounded but
cannot be interrupted. An I/O error after publication can leave a complete store:
inspect/reopen rather than blindly retry or remove it. Use a local filesystem
supporting hard links and directory sync.

On restart call `ValidatedHeaderStore::open_pinned(path, store_config)` to use
the persisted identity **without HTTP**, or `open` with an independently saved
anchor to also check it. Local disk is trusted against malicious replacement.
Bootstrap refuses any existing destination and never silently re-anchors.
Pass the returned store to `HeaderSync` for ongoing headers and retained bodies;
the HTTPS sources are not permanent oracles.

Protocol references: Blockstream `esplora/API.md` (Blocks endpoints) and Bitcoin
Developer Reference `getheaders`/`headers`. Deterministic local HTTP/P2P tests use
recorded mainnet header fixtures; the test-only HTTP override cannot be configured
by production callers.

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
timestamp/retarget checkpoint for each retained header at or above that anchor. Even a capacity of one
can advance through a retarget after restart. A candidate may start at a retained
parent and replace the suffix only with strictly greater work; ties keep the
selected chain. Candidate batches and checkpoints are bounded by header capacity.
There is no orphan pool, persistent fork cache or archival recovery.

Call `protect_through(height, hash)` before promising a selected header as finalized
to a consumer. Already-protected ancestry is an idempotent success only when its
hash can be verified; the floor never decreases.
The monotonic protection floor survives pruning and restart; it is
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
with I/O and time enabled. Each step leases a maintained peer connection, or dials
and handshakes a new candidate, then requests headers from bounded locators.
It validates expected difficulty, PoW and header context before selecting strictly
higher relative work. Canonical progress commits in bounded chunks; a supported
fork can accumulate across requests in that visit, but not across separate visits.
Small store windows can consume protocol-sized 2,000-header replies without storing
the entire reply at once. No live Internet access is needed by tests.

The service leases peers sequentially while idle drivers maintain other sockets.
New dials incur explicit retry pacing; every step has a fixed request budget.
Absolute handshake and request deadlines include all reads, writes and chatter.
There is one foreground request and candidate globally, no orphan pool, and
bounded control tasks described in [peer management](peer-management.md).
Each exchange admits at most 32 frames and 2 MB; the largest foreground buffered
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

To cancel, drop the pending step future and inspect status. The leased socket and
candidate close; idle connections remain maintained. Call `shutdown().await` to
close and join all idle drivers, or drop the service to abort them. Recover storage
with `into_store()`. A completed atomic chunk remains committed. Validation
and synchronous redb operations between awaits cannot be interrupted and can block
the executor; latency-sensitive callers should use a dedicated caller-owned runtime
thread. Unsupported remote candidates stop only their visit, without changing
protected history; local storage/invariant failures latch the service stopped.
There is no automatic re-anchoring or recovery.

Tokio supplies maintained transport/deadlines; `rust-bitcoin` supplies message
encoding, hashes and fixed header decoding. A narrow admission layer is used
instead of its broad allocating network decoder: wire lengths, header counts,
zero transaction counts and version user-agent lengths are checked before variable
decoding. This preserves the same global foreground budget across the pool.
Only a bounded set of handshake and ordinary control/announcement messages is
accepted. Public DNS and legacy address discovery supplement explicit peers.
There is no inbound service, BIP324 transport, automatic transaction relay or
production-readiness claim.

## On-demand blocks

Call `HeaderSync::fetch_block(height, hash, config)` for a retained selected header.
The method checks the height/hash before connecting and applies mainnet witness
rules at height 481,824, then binds the complete body to that hash. It uses the
existing bounded decoder, Merkle/witness commitments and mechanical integrity
checks, not full consensus validation. No bodies persist or remain cached.

The method exclusively borrows the same service used for header synchronization:
one foreground payload and decoder globally per instance, no concurrent header
selection, pruning or downloads. No database transaction crosses an await.
Selection is still canonical when the method returns, but a later `step` may
replace unprotected history. Retrieval does not automatically protect consumed
history; call `protect_through` before promising finality.

`BlockDownloadConfig` explicitly bounds attempts (at most 16), total duration
(at most 120 seconds), each complete visit
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
