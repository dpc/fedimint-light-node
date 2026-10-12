# Guardian RPC and a read-only mainnet demo

`serve` synchronizes headers while exposing **seven guardian-only RPC methods**.
It is not Bitcoin Core, a full validator, or a production guardian-finality
integration. The compatibility target is Fedimint
`502b7ec7da37d231b21d5bce993e9b9a9e7b63a6`, using `bitcoincore-rpc` 0.19.0 and
`jsonrpc` 0.18.0. Integration tests exercise those actual pinned clients.
No reference Fedimint source is modified.

## Mainnet walkthrough

This small demo uses a disposable private directory, a 144-header retained
window and no transaction submission. It downloads recent ancestry and a full
block, not the whole chain. Review [bootstrap trust](guide.md#initial-trusted-bootstrap)
and [security boundaries](../SECURITY.md) first. Internet HTTPS, DNS and outbound
Bitcoin P2P access are required. Public peers may fail or decline blocks.

Build with `nix build` and use `./result/bin/fedimint-light-node`, or
`nix develop -c cargo build --release` and use `./target/release/fedimint-light-node`.
Install `bitcoin-cli` separately (on NixOS, `nix shell nixpkgs#bitcoin`).
Use a shell that supports the function syntax below:

```sh
NODE=./result/bin/fedimint-light-node
DEMO=$(mktemp -d)
chmod 700 "$DEMO"
umask 077
PASSWORD=$(od -An -N24 -tx1 /dev/urandom | tr -d ' \n')
printf 'guardian:%s\n' "$PASSWORD" > "$DEMO/rpc.auth"
printf 'rpcuser=guardian\nrpcpassword=%s\nrpcconnect=127.0.0.1\nrpcport=18443\n' \
  "$PASSWORD" > "$DEMO/bitcoin.conf"
unset PASSWORD

# Create NEW state once. Never remove/rebootstrap an existing guardian store.
"$NODE" --db "$DEMO/chain.redb" --capacity 144 bootstrap \
  --confirmation-margin 6 --total-seconds 300

# Terminal A; leave running. Credentials stay out of process arguments.
"$NODE" --db "$DEMO/chain.redb" --capacity 144 \
  --fixed-fee-sats-per-kvb 1000 serve \
  --rpc-credentials "$DEMO/rpc.auth" --fake-bitcoind
```

Startup prints selected state, the loopback address, and the complete synthetic
field warning. Repeated `sync:` lines show selected height/hash, generation,
connection/candidate counts and visit results. Header selection can advance
from the bootstrap checkpoint to a peer's tip. Empty responses and unchanged
height are normal; neither proves freshness.

In terminal B, set `DEMO` to the **same directory** printed by `echo "$DEMO"` in
terminal A before starting `serve`:

```sh
DEMO=/tmp/the-directory-created-above
rpc() { bitcoin-cli -datadir="$DEMO" -conf="$DEMO/bitcoin.conf" "$@"; }
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
| `estimatesmartfee [1,"CONSERVATIVE"]` | Explicit fixed BTC/kvB rate, or `errors` with no `feerate`. `blocks:1` echoes the supported policy target, not a prediction. |
| `sendrawtransaction [hex]` | Disabled unless `--allow-rpc-submission`; txid after a completed local write only. |
| `getblockchaininfo []` | Disabled unless `--fake-bitcoind`; real selected height/hash/network plus synthetic status below. |
| `getnetworkinfo []` | Under the same flag, minimal synthetic version selector needed indirectly by the pinned client; not the full network-info schema. |

Fixed fee configuration is satoshis per **1,000 virtual bytes**, not sat/vB or
sat/kwu. Startup rejects rates that cannot round-trip exactly through the pinned
client's floating-point BTC amount conversion. No policy is persisted or fetched
from the network. An unavailable estimate can support walletv1 fallback, but is
not sufficient for walletv2 fee voting.

Submission is a separate explicit deployment acknowledgement. Even the
one-argument form does **not** enforce Core's default maximum fee or burn limit.
Extra safety-limit arguments are rejected. There is no input lookup, mempool,
script validation, policy acceptance, automatic retry or durable transaction
queue. Cancellation/post-write errors may follow delivery; retry may duplicate
submission. No `-27` already-confirmed claim is fabricated. Never test this
method against public peers with real transactions as part of a compatibility demo.

## Synthetic status, default off

`--fake-bitcoind` explicitly enables `difficulty=1`, `mediantime=0`,
`verificationprogress=1`, `initialblockdownload=false`, all-zero `chainwork`,
`size_on_disk=0`, `pruned=true`, and `version=190000`. These are schema sentinels,
not measured difficulty/time, freshness, full validation, absolute accumulated
work, database size, Core pruning behavior or a claim to run Core 0.19.
No softfork deployment state is invented. Native startup flags and the warning
enumerate every synthetic field; `getblockchaininfo.warnings` carries the same
inventory. Native `sync:` diagnostics remain truthful observations.

`blocks`, `headers` and `bestblockhash` use one real selected-header snapshot;
Core's stronger interpretation of fully validated blocks does not apply.
The synthetic progress is not the library's native progress, which is unavailable.
The flag never bypasses store opening, configuration checks or terminal local
failures. It does not require a working peer or a fresh tip; isolation is precisely
one reason the synthetic fields must not be treated as health evidence.

## Transport and resource boundaries

Only numeric loopback addresses are accepted. Credentials are mandatory, bounded
to 512 combined bytes and read from a private regular file in the CLI. No TLS,
cookie, public bind, CORS or wallet routes are provided; do not port-forward it.
Basic auth is compared by a constant-time fixed-size SHA-256 digest comparison.
Local connection flooding remains a denial-of-service risk.

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
* Four-second sync slices, followed by at least one second between visits.
  Network requests have a six-second owner deadline and five-second library
  deadline; block visits use two attempts of at most two seconds each.
* At most 4 MB block payload and one million decoded elements per foreground
  download; transaction hex decodes to at most 400,000 bytes/400,000 weight units.

These are application bounds, not an RSS, kernel socket-buffer, OS resolver or
wall-clock guarantee. Synchronous store scans (at most retained capacity),
validation, serialization and database work cannot be preempted. Cancellation
closes leased streams and drops queued work; shutdown aborts and joins every
owned HTTP task and closes the pool. The embedding caller still owns its runtime.

Error codes distinguish malformed requests (`-32700`, `-32600`, `-32602`),
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
