# Fedimint light node

We explore how little CPU, memory, storage and bandwidth a Bitcoin data backend
can use while meeting Fedimint's needs, without making a third-party service the
authority on the chain.

The aim is a robust backend each guardian can operate with modest resources,
without maintaining a full historical database or UTXO set.

## Reliability and trust

### Bitcoin tip and block data

We validate proof of work and contextual header rules locally, retain bounded
recent ancestry, and select the greatest cumulative work observed. Bitcoin P2P
provides headers and complete blocks. We also check public Esplora sources on
independently randomized schedules to discover competing headers when peers
withhold them. We apply the same validation and work selection to those candidates.
Bounded ancestry limits how deep a competing branch we can connect. Downloaded
bodies must match their headers and transaction Merkle and witness commitments.

On initial bootstrap, we pin a checkpoint using agreement between configured HTTPS
sources and preserve it on subsequent runs. We trust that starting point, then
check descendant headers and block integrity locally. We do not perform full
UTXO/script validation. See the [chain trust contract](specs/SPEC-chain-trust.md)
and [header transport details](docs/guide.md#periodic-explorer-header-candidates).

### The mempool

With `--mempool`, we build a bounded observed view from P2P transaction
announcements. Unique address-prefix groups contribute sightings; repeated
sightings from the same group cannot keep entries alive indefinitely.
Idle/hard expiry and least-recently-used eviction bound retention. Background
downloads cover the latest twenty retained selected blocks under a separate
budget. We discard the bodies and use keyed transaction fingerprints to remove
mined entries. On reorganizations, we reconcile coverage against the selected chain.

You get a local view of peer announcements, with coverage and loss counters
to explain gaps. Membership records observations, not spend validation or a
transaction's fee. See the [mempool mechanics and bounds](docs/guardian-rpc.md#approximate-mempool-observation).

### Fee estimation

The same body downloads supply a fixed uniform sample of up to 128 non-coinbase
transactions and a bounded recent-output amount cache. We calculate fees locally
from input amounts and transaction sizes, with no fee-quote service, extra block
requests or parent-transaction queries. Unresolved inputs stay in the sample
instead of giving way to easier observations. Missing-input counts and median
bounds expose their effect.

With `--mempool`, `estimatesmartfee` uses the sample median's finite positive
upper endpoint only when resolution checks pass, the sample matches the current
selected tip, and the sample is under thirty minutes old. Otherwise it omits the
rate. This is a historical-sample heuristic, not a confirmation-time prediction.
An explicit fixed override takes precedence. Rates use integer sat/kvB internally
and BTC/kvB on the RPC wire: 1 sat/vB is 1,000 sat/kvB or 0.00001000 BTC/kvB.
See the [fee policy and qualification rules](docs/guardian-rpc.md#fee-policy-for-the-guardian-adapter).

## Why this fits Fedimint

Fedimint's guardian model suits a small Bitcoin backend:

* **Federation resilience.** Guardians establish Byzantine fault-tolerant (BFT)
  consensus on the chain progress they consume, using agreed block counts.
  One misled guardian does not determine that result. Different guardian backends
  and sources reduce shared failure modes.
* **Time to compare chains.** Guardians trail the chain tip by a finality delay,
  giving their backends time to discover competing branches before consuming blocks.
* **Recent blocks, not an archive.** Normal operation needs relatively few recent
  blocks as the federation advances. On-demand bodies and bounded retained ancestry
  fit that workload without a historical block database.
* **Shared fee estimation.** Fee estimates already combine inputs from multiple
  guardians through federation consensus. Guardians can use different fee backends,
  so one light node's estimate is only one input.
* **Best-effort mempool.** Guardians use mempool visibility for pending-deposit UX.
  A bounded observed view can show progress before confirmation; settlement follows
  confirmed blocks and federation consensus.

See the [guardian workload](docs/guide.md#guardian-workload) and
[fee-source integration](docs/guardian-rpc.md#fedimint-fee-sources) for details.

## Try it

```sh
cargo run
```

You need no setup command or credentials file. On first use, normal startup creates
persistent mainnet state, then synchronizes headers and serves **read-only,
unauthenticated RPC on `127.0.0.1:18443`**. Later runs reopen that state; they never
reset or bootstrap over an existing file. State lives at
`$XDG_STATE_HOME/fedimint-light-node/chain.redb`, or
`~/.local/state/fedimint-light-node/chain.redb` when XDG state home is unset.
Startup prints the path, synchronization progress and RPC address.
Operational logs use `tracing` on stderr at `info` by default; set
`RUST_LOG=fedimint_light_node=debug` for detailed diagnostics. See the
[logging policy](docs/logging.md) for levels, privacy and progress sampling.

After the first bootstrap finishes, query from another terminal:

```sh
bitcoin-cli -datadir=/tmp -conf=/dev/null -rpcconnect=127.0.0.1 -rpcport=18443 getblockcount
```

You need neither credentials nor a Bitcoin Core daemon. The client options select an
existing directory and an empty configuration instead of your local Core setup;
they do not create a Core database. Ctrl-C stops the node and preserves state.
Initial bootstrap needs Internet HTTPS, DNS and outbound Bitcoin P2P access;
it can take minutes or fail when sources or peers are unavailable.

Optional overrides: `cargo run -- --db /trusted/path/chain.redb`,
`cargo run -- --rpc-bind 127.0.0.1:0` for an available port, or
`cargo run -- --rpc-credentials /private/rpc.auth` for authentication.
`cargo run -- --fake-bitcoind` also enables the explicitly synthetic
Core-status compatibility calls; they remain off by default. See the
[RPC guide](docs/guardian-rpc.md) for curl queries, authentication, resource
limits and explicit operational commands. With Nix, `nix run .` starts the same
service; you can also use `nix develop -c cargo run`.

### Optional approximate mempool

```sh
cargo run -- --mempool
```

From another terminal:

```sh
bitcoin-cli -datadir=/tmp -conf=/dev/null -rpcconnect=127.0.0.1 -rpcport=18443 getrawmempool
```

This default-off mode observes general transaction announcements, not a watch
list. It also downloads recent selected block bodies under a background budget,
keeps only keyed transaction fingerprints, and discards the bodies. The result
starts empty and builds a bounded local sample. Diagnostics report coverage and
losses. See the [observation limits](docs/guardian-rpc.md#approximate-mempool-observation).

The same mode reports **partial historical fee observations**, without fetching
extra blocks or parent transactions:

```sh
bitcoin-cli -datadir=/tmp -conf=/dev/null -rpcconnect=127.0.0.1 -rpcport=18443 getfeeobservations
```

The result includes missing-parent counts, coverage and sample-median bounds.
See [fee observation semantics](docs/guardian-rpc.md#historical-fee-observations).

## How does this compare?

Compare where each approach stores state, sends traffic and processes data,
as well as the size of its local database:

| Approach | Storage, traffic and operating cost |
|---|---|
| **This project** | Bounded recent headers and context, no UTXO database or persistent block-body cache. Startup synchronizes headers from a pinned checkpoint; complete blocks arrive on demand. Optional mempool observation adds bounded in-memory state and recent-body downloads. |
| **Pruned Bitcoin Core** | Bounds retained block-body disk use, but keeps chainstate and a block index and still processes chain history. AssumeUTXO can shorten time to use, with background historical processing and extra transitional storage. |
| **Kyoto / Neutrino** | Header and compact-filter traffic plus selective block downloads. Filtering saves body bandwidth when an application can skip blocks; it cannot remove downloads of complete blocks a guardian needs. |
| **Electrum / Esplora clients** | A small client can query a remote indexed backend rather than maintain that index locally. The server's storage, processing and operating cost remain; self-hosting puts them back in the operator's budget. Complete-block retrieval still costs bandwidth. |
| **Floresta / utreexod** | Compact accumulator state replaces a full local UTXO database, but headers, caches and block processing remain. Proof data adds traffic and processing; proof-serving infrastructure and bootstrap choices add operational complexity. |

Memory use includes more than retained chain state: database caches, peer queues
and decoded blocks also matter. For this project, requested blocks still consume
bandwidth and transient memory even though we do not store their bodies. Optional
mempool observation adds ongoing traffic. A small on-disk header store is not a
process-memory bound or a complete deployment budget.

See the [detailed comparison](docs/alternative-approaches.md) for primary sources
checked on October 12, 2026, configuration caveats and resource tradeoffs. This
is not a measured performance ranking.

## Explicit operations

The quickstart above runs bootstrap, synchronization and read-only RPC for you.
For manual control, run the individual operations instead. For example, with Nix
installed:

```sh
nix build
mkdir -m 700 node-data
NODE=./result/bin/fedimint-light-node
DB=./node-data/headers.redb

# Creates a NEW store. Default bootstrap sources must agree.
"$NODE" --db "$DB" bootstrap

# Reopens the pinned checkpoint and reports selected-chain progress.
"$NODE" --db "$DB" sync
```

The default network mode discovers peers and periodically checks public explorer
header candidates; you do not need to hand-pick public IP addresses. `--no-explorer-headers`
disables periodic explorer traffic without changing trusted initial bootstrap.
To use only your own node, add `--manual-only --peer 127.0.0.1:8333` to network
commands; this also suppresses default periodic explorers, but not initial
bootstrap HTTPS. Explicit `--header-explorer HTTPS_API_ROOT` opts back in.
`status` inspects a stopped node offline. Only one process should own
the database at a time.

## Learn more

- [Implementation status](docs/implementation-status.md).
- [Operation and embedding guide](docs/guide.md): configuration, bootstrap,
  retrieval, submission, storage and API details.
- [Peer management](docs/peer-management.md): discovery, resource bounds,
  selection policy and cancellation.
- [Security boundaries](SECURITY.md): trusted inputs and remaining limitations.
- [Architecture and implementation status](specs/ARCH-fedimint-light-node.md).
- [Resource exercise](docs/resource-exercise.md): measured workloads, not
  process-memory or disk-size guarantees.
- [Embedding example](examples/embed.rs).

## AI usage disclosure

[I use LLMs when working on my projects.](https://dpc.pw/posts/he-went-to-the-slop-side/)
