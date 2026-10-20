# Local RPC and guardian compatibility

`serve` synchronizes headers while exposing **nine guardian compatibility methods**.
The compatibility target is Fedimint's `IServerBitcoinRpc` and its Core adapter at
`60b0ceafb9e946207899f8d73b8c32a969505f3f`, using `bitcoincore-rpc` 0.19.0 and
`jsonrpc` 0.18.0. Integration tests exercise those actual pinned clients.
The separate client-facing `IBitcoindRpc` script-history/proof interface is outside
this profile.
No reference Fedimint source is modified. See the [operator runbook](operations.md)
for explicit guardian binding, startup preflight, stopped-store backup and recovery.

## Normal startup

Run `cargo run` (or `nix run .`). No arguments, prior bootstrap, credentials or
special demo mode are needed. The binary bootstraps a new mainnet store only when
the destination is absent, otherwise reopens pinned state, synchronizes headers
and serves read-only RPC at `127.0.0.1:18443`. It prints its persistent database
path: `$XDG_STATE_HOME/fedimint-light-node/chain.redb`, falling back to
`~/.local/state/fedimint-light-node/chain.redb` when XDG state home is empty, unset
or relative. It never silently resets existing state. Guardian-bound state
requires explicit `serve` with the independently recorded guardian preflight.

Defaults retain 52,560 headers with an 8 MiB database cache, use the existing
bounded public peer pool and require agreement of the two default HTTPS sources
with a six-block confirmation margin. First bootstrap has a ten-minute total
asynchronous deadline; Ctrl-C cancels bootstrap or serving. Synchronous work and
OS resolver shutdown have the limits described in [SECURITY.md](../SECURITY.md).
The store persists across stops, including a checkpoint published just before
cancellation. No fee estimate or synthetic Core status is enabled by default.
Ongoing [periodic Esplora header candidates](guide.md#periodic-explorer-header-candidates)
are enabled by default in the CLI, independently of trusted bootstrap. Due checks
receive maintenance opportunities even under continuous RPC load. Each HTTP
request can add up to three seconds of foreground delay; a complete multi-request
visit never blocks the owner as one operation. Disable with `--no-explorer-headers`;
`--manual-only` suppresses default explorers unless explicit header roots opt in.

Once startup prints `rpc_listen=`, use either:

```sh
bitcoin-cli -datadir=/tmp -conf=/dev/null -rpcconnect=127.0.0.1 -rpcport=18443 getblockcount
curl --noproxy '*' -H 'Content-Type: application/json' \
  --data '{"jsonrpc":"2.0","id":1,"method":"getblockcount","params":[]}' \
  http://127.0.0.1:18443/
```

No credentials or Core daemon are needed; Bitcoin Core CLI 31.1.0 was tested
without a username or password. `-datadir=/tmp` selects an existing directory,
avoiding packaged defaults such as a nonexistent `/var/lib/bitcoind`.
`-conf=/dev/null` prevents your local `bitcoin.conf` from affecting the query.
These options do not create a Core database or modify your Core configuration.
Use `cargo run -- --fake-bitcoind` only when you want the synthetic
`getblockchaininfo` and `getnetworkinfo` compatibility schema described below.
`--db /trusted/path/chain.redb` overrides the path; custom `--capacity` must match
on every reopen. `--rpc-bind 127.0.0.1:0` chooses an available port if the default
is occupied; startup prints the chosen address. Debug logging includes matching
query commands.

For authenticated normal startup instead:

```sh
umask 077
printf 'guardian:replace-with-a-private-password\n' > /private/rpc.auth
cargo run -- --rpc-credentials /private/rpc.auth
```

Create the file in a private trusted directory and configure the matching
credentials in your client's private `bitcoin.conf`. Explicit `serve` retains
its opt-in policy: supply either `--rpc-credentials FILE` or `--rpc-no-auth`.
The latter conflicts with credentials and `--allow-rpc-submission`; normal
startup also requires credentials to enable submission. Both forms support only
numeric IPv4 or IPv6 loopback binds. Do not expose or forward the listener.

## Approximate mempool observation

`cargo run -- --mempool` enables general legacy transaction announcements and
bounded background downloads toward the latest twenty retained selected bodies.
Explicit serving also accepts `serve --mempool`. The library equivalent is
`HeaderSync::new_with_mempool`; embedding callers drive `cover_mempool` between
higher-priority work. Ordinary startup leaves observation and this extra traffic off.

```sh
bitcoin-cli -datadir=/tmp -conf=/dev/null -rpcconnect=127.0.0.1 -rpcport=18443 getrawmempool
```

`getrawmempool` and `getrawmempool false` return full txid strings. They work with
unauthenticated local reads independently of submission authorization or synthetic
Core status. Verbose and sequence parameters are rejected; `getmempoolentry`
and `getmempoolinfo` remain unsupported.

`getrawtransaction [txid,false]` fetches canonical witness-bearing bytes only for
current observed membership. It leases the latest announcer if still maintained,
then another maintained witness peer, without dialing or caching transaction bodies.
The request uses txid-based `MSG_WITNESS_TX`, not `wtxid` inventory. Membership is
checked again before return. This supports walletv2's output scan, not archive or
transaction-index queries. Peer availability is not guaranteed.

Each lookup permits two sequential peers, five seconds total, two seconds per
visit, 32 frames per visit, 1,000,000 aggregate received bytes and 4,096 control-write
bytes. Transactions must fit 400,000 bytes and weight units. Absent, expired or
mined membership, no maintained peer, `notfound`, or local transaction admission
limits return RPC `-5`; exhausted transport/corruption failures return `-32012`.
Malformed or interrupted leases close; only clean completed exchanges return to
the pool, without useful header/block service credit. Transaction bytes do not
enter the selected-body fee amount cache.

The view starts empty and grows from peer announcements; it does not request
mempool dumps. Only legacy `MSG_TX` inventory contributes;
witness transaction identities are not reinterpreted as transaction IDs.

Logical budgets (engineering defaults, not measured RSS guarantees):

| Resource | Bound |
| --- | --- |
| Pending full transaction IDs | 50,000, with two exact bounded expiry/LRU indexes |
| Lifetime | Two-hour idle expiry; 24-hour hard age from admission |
| Sources per entry | Four address-prefix groups, saturated; not independent operators |
| Routing hint | One latest actual `SocketAddr` per entry, replaced in place without TTL/LRU refresh |
| Confirmed filter | Latest twenty retained selected blocks, 1,000,000 keyed 64-bit fingerprints aggregate |
| Inbox | 4,096 observations; excess dropped and counted |
| Checksum staging | 1,024 transaction IDs per socket, at most four sockets |
| Inventory receive | 8 MiB/minute aggregate, 2 MiB/minute and 1,024 frames per connection |
| Background bodies | 8 MiB/minute, 4 MiB reserved per attempt including failed/canceled work |
| Coverage slice | One body attempt, at most five seconds and limited by time until due headers; queued RPC preempts it |
| RPC result | At most 50,000 strings, about 3.35 MB compact JSON |

Repeated source groups do not refresh idle life, LRU position or source count.
A newly observed group can refresh idle life until source tracking saturates;
reads never refresh it. A fresh later announcement can re-admit an expired entry.
The four-socket cap is unchanged. Inventory uses separate bounded accounting from
control traffic and coverage bodies; full checksums precede inbox admission.
The routing hint adds at most 32 inline bytes per pending record (1.6 MB at the
50,000-entry cap), plus at most 32 bytes per inbox record (128 KiB at capacity),
before existing tree/deque allocation overhead; it has no separate index.
Transaction reads charge inventory frames to their aggregate exchange budget.

Coverage prefers the newest missing selected body and may lag indefinitely under
load or peer failure. Successful consumer body fetches also populate the filter.
Enabling observation increases the interval after each header visit from one to
five seconds, trading up to four seconds of additional polling latency for useful
body-download time. Disabled mode retains its one-second interval. Coverage uses
the actual remaining monotonic time before the next header deadline, with a small
cleanup margin, and does not reserve an attempt with less than 250 ms available.
RPC traffic and failed coverage never reset that header deadline. No full
five-second attempt is guaranteed; queued requests can consume its allowance.
The owner discards bodies after extracting fingerprints and bounded fee metadata. Hash/height reconciliation
removes disconnected and out-of-window buckets before reads, including same-height
reorganizations. Suppressed entries are removed, not hidden for later resurrection;
a disconnected block does not manufacture observations. Fingerprint collisions can
falsely suppress entries. Neither presence nor absence proves acceptance or confirmation.

The existing eight HTTP connection limit bounds concurrent snapshot replies,
including queued `oneshot` results: at most eight result trees and their serialized
copies can be in flight, plus one bounded construction scratch vector. Requests
queued for the owner contain no snapshot. These allocations and allocator overhead
are additional to the cache; the logical txid cap is not a process-memory promise.
Native `approximate_mempool` diagnostics report pending/covered/target counts,
fingerprints, evictions, expired entries, coverage-capacity losses and inbox drops.
The normal diagnostic includes selected generation and tip separately. These are
not fabricated Core mempool status fields.

## Historical fee observations

`--mempool` also enables `getfeeobservations` (no parameters), an unauthenticated
read-only native RPC. The library equivalent is `HeaderSync::fee_observations`.
Disabled mode returns the usual observation-disabled method error (library:
`None`). Enabled mode starts empty on every restart; a null `block_hash` means
no retained sample, not zero fees. It needs neither `--fake-bitcoind` nor
submission authorization and uses the same storage-readiness gate as other reads.

```sh
cargo run -- --mempool
# In another terminal:
bitcoin-cli -datadir=/tmp -conf=/dev/null -rpcconnect=127.0.0.1 -rpcport=18443 getfeeobservations
```

The component makes **zero network requests**. It inspects selected bodies that
the existing background coverage or consumer fetches already delivered. It does
not change coverage priority, download budgets, peer sockets or explorer requests.

### Fee policy for the guardian adapter

With no fixed override, CLI `--mempool` selects `FeePolicy::Observed`; without
observation the policy is unavailable. Library callers choose `Observed`,
`Fixed` or the explicit `Unavailable` opt-out. Fixed rates always win.
Observed policy is a historical sample heuristic, not Core's confirmation-target
estimator. It selects the fixed sample's **finite positive median upper endpoint**
only for the current selected tip hash, zero height lag, age strictly under thirty
minutes, no invalid observations, and at least 32 resolved transactions or a fully
resolved population smaller than 32. A 128-member sample with only 32 resolved
rates is unavailable; 64 resolved rates can provide the finite upper endpoint.
Exact rational ordering precedes integer ceiling of `fee_sat * 1000 / vsize`.
No rounded diagnostic quantile participates in the policy.

`estimatesmartfee [1,"CONSERVATIVE"]` returns `{"blocks":1,"feerate":BTC_PER_KVB}`
or `{"blocks":1}` when no usable rate exists, including a failed BTC amount
round-trip. Reads do not fetch, redraw, refresh age or change coverage. Fedimint's
Core adapter converts the BTC amount directly to sat/kvB; the node applies no
wallet multiplier. Walletv1 can use its configured fallback for `None`; walletv2
retracts that guardian's fee vote.

Each newly inspected body newer than the active sample supplies one uniform
sample without replacement: up to 128 transactions, excluding coinbase. Selection
happens before parent resolution; failures are never replaced by easier samples.
Duplicate body deliveries and RPC reads do not redraw it. The observer resolves
full input outpoints against the resident body and a recent-output amount cache;
later older-body coverage can fill the active sample's missing slots. It retains
no raw transactions, scripts or witnesses.

Each exact observation needs **all input amounts**. The arithmetic checks amount
ranges, sums, negative fees and output indices, while body admission also rejects
duplicate and null inputs. Zero fees are legitimate observations. A parent txid
binds its outputs, but proves neither unspentness nor script validity.
Detected contradictions set `invalid_observation`
and suppress all quantiles and bounds rather than silently dropping bad samples.
This is additional local arithmetic, not spend or full consensus validation.
Rates use satoshis divided by `ceil(BIP141_weight / 4)`, not Core's potentially
sigop-adjusted policy size. Floating-point result fields are finite approximations.

### Reading the result

* `block_hash`, `height`, `selected_tip_hash` and `blocks_behind_tip` identify the
  historical sample and current **local** selection. `observed_at` is initial
  wall-clock Unix seconds; `age_seconds` uses monotonic time. Parent resolution
  does not refresh either. `stale` means no sample, a non-tip sample, or age at
  least thirty minutes. Neither a false stale flag nor a recent timestamp proves
  network freshness or consensus validity.
* `non_coinbase_tx_count`, `sampled_tx_count`, `resolved_tx_count`,
  `budget_unresolved_tx_count` and `invalid_tx_count` describe transaction coverage.
  `resolved_percent` is resolved/sample percentage, **not confidence**.
* `sampled_input_count` is exact. It equals `resolved_tracked_input_count` plus
  `tracked_missing_input_count` plus `untracked_input_count`. A selected transaction
  whose whole input set cannot fit the remaining slot budget is kept in the sample
  but all its inputs are untracked. `total_missing_input_count` is null in that
  case, otherwise the tracked missing count. No fraction of total input value
  known can be inferred. Invalid transactions are flagged separately even when
  some of their tracked inputs lack amounts.
* `subset_p25_sat_per_vb`, `subset_p50_sat_per_vb` and `subset_p75_sat_per_vb` use
  nearest ranks `ceil(p * resolved_count)` (one-based), without interpolation;
  p50 is the lower median for an even count. They remain null until at least
  32 transactions resolve, or all transactions of a smaller block resolve.
  This display gate does **not** remove resolution bias.
* `sample_median_lower_sat_per_vb` and `sample_median_upper_sat_per_vb` bound the
  **fixed sample's lower median**, conditional on valid nonnegative rates.
  For sample size `n`, resolved count `r`, missing count `m=n-r`, and rank
  `k=ceil(n/2)`, the lower endpoint is zero if `k<=m`, otherwise resolved rank
  `k-m`. The upper is resolved rank `k` if `k<=r`, otherwise null with
  `sample_median_upper_unbounded=true`. Budget-unresolved transactions count as
  missing here. Empty or detected-invalid samples have null endpoints and
  `upper_unbounded=false`. All-resolved bounds collapse to the sample median.
  These are not confidence intervals for the block or future fee market.
* `output_cache_blocks`, `output_cache_entries`, `output_cache_evictions` and
  `output_cache_allocated_bytes` report actual amount-cache retention and losses.
  Evictions include records discarded or never admitted under capacity pressure,
  window expiration and selection invalidation. `body_coverage_complete` and
  `body_coverage_target` report the separate existing fingerprint/body coverage;
  complete body coverage does not imply complete output or fee coverage.

The resolved subset favors locally available parents. Transactions with many
inputs are especially likely to remain unresolved; old parents may never appear.
Even complete individual fee rates omit package incentives, excluded transactions
and future demand. Do not use a subset quantile as a guaranteed admission threshold.

### Retention and resource limits

There is one active sample and no summary history. Ordinary tip advancement keeps
the preceding sample available for older-parent resolution until a newer body is
inspected. Full-hash reconciliation clears all fee state on a detected fork or
discontinuous window change. A sample or any of its known parent-body provenance
leaving the selected twenty-block window discards that sample. Amount-cache
capacity eviction alone does not discard already resolved slots. Seen-body
identities remain bounded by the window; invalidation does not request or redraw
an already seen body just to restore a sample. Wait for a newer inspected body.

| Resource | Bound |
| --- | --- |
| Amount records | 250,000 records; full outpoint, output count, value and source height |
| Amount provenance | At most twenty selected body buckets, newest retained under pressure |
| Sample | 128 transaction metadata entries; no retained raw bodies |
| Tracked inputs | 4,096 slots including resolved slots, each with full parent-body provenance |
| Temporary indexes | 128 reservoir positions; 4,096 slot indices for resident-body matching; 128 rates |
| Total fee-component logical allocation | At most 16 MiB, checked at compile time including allocated capacities, indexes and scratch |
| RPC | One fixed-size summary per read, under the existing eight-connection reply limit |

On the supported 64-bit build the output record is 56 bytes: 14,000,000 bytes
for its fixed-capacity vector, plus a 160-byte height index. The allocation occurs
only after the first admitted body. Remaining sample, identity and scratch budgets
fit within the 16 MiB cap. Sorting records uses no additional heap index; wanted
outpoints are matched by a bounded index while scanning each resident body.
The cache evicts whole oldest buckets and drops incoming older buckets rather
than displacing newer ones. This is a logical allocation bound, **not** a quota
on allocator bookkeeping, stack, RSS, caller-owned block memory, existing mempool
state or serialized RPC copies. No UTXO database, wallet tracking, parent fetching
or archival recovery is added.

## Explicit disposable mainnet walkthrough

This small demo uses a disposable private directory, a 144-header retained
window and no transaction submission. It downloads recent ancestry and a full
block. Internet HTTPS, DNS and outbound
Bitcoin P2P access are required. Public peers may fail or decline blocks.

Build with `nix build` and use `./result/bin/fedimint-light-node`, or
`nix develop -c cargo build --release` and use `./target/release/fedimint-light-node`.
Install `bitcoin-cli` separately (on NixOS, `nix shell nixpkgs#bitcoin`).
Use a shell that supports the function syntax below:

```sh
NODE=./result/bin/fedimint-light-node
DEMO=$(mktemp -d)
chmod 700 "$DEMO"

# Create NEW state once. Never remove/rebootstrap an existing guardian store.
"$NODE" --db "$DEMO/chain.redb" --capacity 144 bootstrap \
  --confirmation-margin 6 --total-seconds 300

# Terminal A; leave running. Local reads need no server credentials.
"$NODE" --db "$DEMO/chain.redb" --capacity 144 \
  --fixed-fee-sats-per-kvb 1000 serve \
  --rpc-no-auth --fake-bitcoind --standalone
```

Startup prints selected state, the loopback address, and enabled modes.
Bounded synchronization summaries show selected height/hash, generation,
connection/candidate counts and visit results. Header selection can advance
from the bootstrap checkpoint to a peer's tip. Empty responses and unchanged
height are normal; neither proves freshness.

In terminal B, set `DEMO` to the **same directory** printed by `echo "$DEMO"` in
terminal A before starting `serve`:

```sh
DEMO=/tmp/the-directory-created-above
rpc() { bitcoin-cli -datadir="$DEMO" -conf=/dev/null -rpcconnect=127.0.0.1 -rpcport=18443 "$@"; }
rpc getblockchaininfo
rpc getnetworkinfo
HEIGHT=$(rpc getblockcount)
HASH=$(rpc getblockhash "$HEIGHT")
printf 'height=%s hash=%s\n' "$HEIGHT" "$HASH"
rpc getblockhash 1
rpc estimatesmartfee 1 CONSERVATIVE
rpc getblock "$HASH" 0 > "$DEMO/block.hex"
wc -c "$DEMO/block.hex"
```

Expected: real mainnet selected height, a 64-character selected hash, the pinned
height-one identity, and `{"blocks":1,"feerate":0.00001000}` (equivalent decimal
formatting is normal). The final file contains complete witness-bearing block
hex, not a verbose summary. A block may be unavailable within the short RPC
deadline; retry explicitly or use the longer-deadline native `fetch` command.
If a reorganization/prune occurs between separate requests, retry from height.
No read advances durable protection.

Stop terminal A with Ctrl-C. Reopen offline with
`"$NODE" --db "$DEMO/chain.redb" --capacity 144 status` to inspect unchanged
checkpoint/protection. Restart `serve` against that same store; do not bootstrap
it again. The demo directory can be removed after stopping, **only because it is
disposable test state**, never guardian state.

## Exact supported dialect

| Call | Behavior |
|---|---|
| `getblockcount []` | Real selected height. Fedimint adds one to obtain count. |
| `getblockhash [height]` | Retained selected hash; height 1 has a pinned identity exception. |
| `getblock [hash,0]` | Complete canonical witness-bearing bytes as hex. Omitted/verbose modes are rejected. Unknown hashes are not automatically archival queries. |
| `estimatesmartfee [1,"CONSERVATIVE"]` | Fixed or qualified observed BTC/kvB rate; otherwise only `blocks:1`. The block count echoes the supported target. |
| `sendrawtransaction [hex]` | Disabled unless `--allow-rpc-submission`; txid after a completed local write only. |
| `getblockchaininfo []` | Disabled unless `--fake-bitcoind`; real selected height/hash/network plus synthetic status below. |
| `getnetworkinfo []` | Under the same flag, minimal synthetic version selector needed indirectly by the pinned client; not the full network-info schema. |
| `getrawmempool []` | Current approximate txid membership under `--mempool`; `[false]` is also accepted. |
| `getrawtransaction [txid,false]` | Current-membership witness transaction lookup under `--mempool`; no verbose or block-hash forms. |

Fixed fee configuration is satoshis per **1,000 virtual bytes**, not sat/vB or
sat/kwu. Startup rejects rates that cannot round-trip exactly through the pinned
client's floating-point BTC amount conversion. No policy is persisted or fetched
from the network. An unavailable estimate can support walletv1 fallback, but is
not sufficient for walletv2 fee voting.

Submission requires `--allow-rpc-submission` and credentials. The method returns
the txid after a completed local write. Extra fee/burn-limit arguments are rejected;
Core's implicit limits are not implemented. See
[transaction submission](transaction-submission.md) for report and retry semantics.

## Synthetic status, default off

`--fake-bitcoind` explicitly enables `difficulty=1`, `mediantime=0`,
`verificationprogress=1`, `initialblockdownload=false`, all-zero `chainwork`,
`size_on_disk=0`, `pruned=true`, and `version=190000`. These are schema sentinels,
not measured difficulty/time, freshness, full validation, absolute accumulated
work, database size, Core pruning behavior or a claim to run Core 0.19.
No softfork deployment state is invented. Startup reports `fake_bitcoind=true`;
the field definitions above document this compatibility mode.
`getblockchaininfo.warnings` is empty during normal operation.

`blocks`, `headers` and `bestblockhash` use one real selected-header snapshot;
Core's stronger interpretation of fully validated blocks does not apply.
The synthetic progress is not the library's native progress, which is unavailable.
The flag never bypasses store opening, configuration checks or terminal local
failures. It does not require a working peer or a fresh tip; isolation is precisely
one reason the synthetic fields must not be treated as health evidence.

## Transport and resource boundaries

Only numeric loopback addresses are accepted. When authentication is enabled,
credentials are bounded to 512 combined bytes and read from a private regular
file in the CLI. No TLS,
cookie, public bind, CORS or wallet routes are provided; do not port-forward it.
Basic auth is compared by a constant-time fixed-size SHA-256 digest comparison.
Local connection flooding remains a denial-of-service risk.

Without authentication, HTTP requests must include a single `Host` naming the
bound IP or `localhost`, optionally with the actual listen port, and a single
`Content-Type: application/json`. Bare IPv6 and bracketed IPv6 numeric hosts are
accepted. Any `Origin` header is forbidden. These native-client restrictions
block browser-origin requests and DNS rebinding without adding CORS or changing
the authenticated dialect. A supplied Authorization header is ignored in
unauthenticated mode, but duplicate headers and malformed HTTP still fail.
The library rejects `RpcAuth::NoAuth` combined with `allow_submission`, so
embedding callers cannot bypass the read-only policy.

The parser accepts one HTTP/1.1 `POST /` per connection with a single decimal
`Content-Length`; it closes after an explicit-length JSON reply. It rejects
chunked/content-encoded bodies, duplicate framing/auth headers, pipelining and
request-side half-close. `httparse` handles header grammar; custom framing stays
small to enforce bounds before body allocation. A general HTTP framework's
broader protocol surface is unnecessary here.

JSON-RPC 2.0 and legacy 1.0/absent version are supported. Application errors use
HTTP 200; unauthorized requests use 401. IDs are null, integers or strings of at
most 64 bytes; methods are at most 64 bytes. Notifications, batches, unknown
envelope keys, named parameters and more than two arguments are rejected.

Budgets are fixed and global to one service:

* Eight accepted connections/tasks, plus the OS listen backlog.
* Four queued cheap reads and one queued network request. Cheap reads take
  priority between operations; a deadline check guarantees continuing sync visits.
* One exclusive store/pool owner and one foreground P2P operation.
* 4,096 header bytes, 24 HTTP headers, 801,024 body bytes, JSON nesting at most
  eight and at most 64 structural separators/openers before serde allocation.
* Responses at most 8,001,024 bytes; at most eight completed response writers.
  Block serialization/hex/JSON introduce bounded copies, not zero-copy streaming.
* Two seconds to read headers/body, eleven seconds waiting for the owner, fourteen
  seconds for the entire HTTP connection. The pinned client waits fifteen seconds.
* Four-second sync slices, followed by one second between visits, or five seconds
  with observation enabled to allow bounded background body retrieval.
  Network requests have a six-second owner deadline and five-second library
   deadline; block and transaction reads use two attempts of at most two seconds each.
* At most 4 MB block payload and one million decoded elements per foreground
  download; transaction hex decodes to at most 400,000 bytes/400,000 weight units.

These are application bounds, not an RSS, kernel socket-buffer, OS resolver or
wall-clock guarantee. Synchronous store scans (at most retained capacity),
validation, serialization and database work cannot be preempted. Cancellation
closes leased streams and drops queued work; shutdown aborts and joins every
owned HTTP task and closes the pool. The embedding caller still owns its runtime.

Error codes distinguish unavailable observed transactions (`-5`),
malformed requests (`-32700`, `-32600`, `-32602`),
unsupported methods (`-32601`), below-retention height (`-32010`), unknown or
not-retained hash (`-32011`), temporary peer/deadline failure (`-32012`), terminal
local recovery (`-32013`), queue saturation (`-32014`) and future height (`-8`).
Only temporary/unavailable and busy errors carry `data.retryable=true`.
These service-specific codes are not broad Core error compatibility.

## What this does not establish

An unchanged pinned client can decode the supported responses, but real guardian
database commitment/finality coordination is not implemented. Ordinary reads
never protect history. Standard RPC cannot tell the node when a guardian
durably commits a block. Existing guardian callers may also retry permanent
retention errors forever. Deployments need explicit guardian binding, protection
and recovery policy before relying on this backend for funds.

A successful read-only mainnet smoke proves connectivity and this narrow wire
path only. It is not a long soak, fault/resource campaign, full consensus audit,
independent-source proof or production-readiness certification.

## Recorded read-only smoke

The [subsequent default-scale campaign](readiness-evidence.md) identified a
protocol-version defect behind the repeated legacy-alert failures below.
Successful retry/warmup alone did not rule out that interoperability bug.
Advertising 70013 instead of 70012 avoids Core's final-alert trigger without
accepting alert payloads; the subsequent bounded mainnet run reached four peers.

On October 13, 2026 UTC, a disposable 144-header store bootstrapped at 971152.
The CLI visibly advanced to 971158, then 971159; reopening preserved checkpoint
and protection at 971152. Bitcoin Core CLI v31.1.0 successfully queried status,
network schema, selected height/hash, pinned height-one identity and fixed
1,000 sat/kvB fee policy.

An actual `getblock [hash,0]` returned block 971159, hash
`000000000000000000005f0d757d7c79fc1c27a4b861269cf99eecc79afe6bb9`:
1,617,758 binary bytes (3,235,516 hex characters) in 1.43 seconds. The independently
hashed 80-byte header matched the requested hash; all returned bytes matched a
separate native fetch, including the witness serialization marker/flag.

This was not a first-attempt availability guarantee. One bootstrap failed on
an unsupported peer command before an explicit fresh retry succeeded; later
diagnostics identified rejected `alert` messages on some peer visits. Three
initial short-deadline block RPC attempts failed. A native fetch succeeded, then
a subsequent RPC run succeeded after allowing 30 seconds for pool maintenance.
No command admission was relaxed, no history was replaced, and no transactions
were submitted. The disposable evidence occupied about 3.2 MiB on disk. These
observations are a bounded compatibility smoke, not resource or uptime guarantees.
