# Fedimint light node

A **research project** exploring the lightest practical Bitcoin node for Fedimint
guardians without ceding security to third-party backends.

The idea is to give each guardian its own small Bitcoin backend: retain recent
headers, follow the greatest cumulative valid-header proof of work observed, and
download complete blocks only when needed. Avoiding a full historical database
and UTXO set makes independently operated guardian backends much less demanding.

## Try it

```sh
cargo run
```

No setup command or credentials file is needed. Normal startup creates persistent
mainnet state on first use, then synchronizes headers and serves **read-only,
unauthenticated RPC on `127.0.0.1:18443`**. Later runs reopen that state; they never
reset or bootstrap over an existing file. State lives at
`$XDG_STATE_HOME/fedimint-light-node/chain.redb`, or
`~/.local/state/fedimint-light-node/chain.redb` when XDG state home is unset.
Startup prints the path, synchronization progress and ready-to-copy queries.

After the first bootstrap finishes, query from another terminal:

```sh
bitcoin-cli -rpcconnect=127.0.0.1 -rpcport=18443 -rpcuser=demo -rpcpassword=demo getblockcount
```

The dummy credentials work with clients that insist on supplying credentials;
the server does not check them. No Bitcoin Core daemon is needed. Any local
process can query this endpoint; **do not forward its port**. Ctrl-C stops the
node and preserves state. Initial bootstrap trusts configured HTTPS source
agreement and needs Internet HTTPS, DNS and outbound Bitcoin P2P access; it can
take minutes or fail when sources or peers are unavailable. This is not full
Bitcoin validation or an automatic guardian integration.

Optional overrides: `cargo run -- --db /trusted/path/chain.redb`,
`cargo run -- --rpc-bind 127.0.0.1:0` for an available port, or
`cargo run -- --rpc-credentials /private/rpc.auth` for authentication.
`cargo run -- --fake-bitcoind` additionally enables the explicitly synthetic
Core-status compatibility calls; they remain off by default. See the
[RPC guide](docs/guardian-rpc.md) for curl queries, authentication, resource
limits and explicit operational commands. With Nix, `nix run .` starts the same
normal service, or use `nix develop -c cargo run`.

## Why this fits Fedimint

Fedimint already delays consumption of Bitcoin blocks behind the observed tip.
That finality delay gives a guardian room to compare header chains before
consuming blocks, while keeping a bounded recent window. External references
establish an initial checkpoint; subsequent contextual header validation and
cumulative work select its descendants. The goal is particularly useful in a
BFT federation where each guardian runs its own backend and participates in
federation consensus.

The research goal is **not a claim of trust-free bootstrap or full Bitcoin
validation**. The current bootstrap trusts agreement between HTTPS sources.
Finality delay and BFT do not prove script or UTXO validity or eliminate
correlated source failures and eclipse risk. Existing guardians agree block
counts and independently fetch bodies, not an authenticated hash-chain vote.

## What works today

The experimental Rust library and CLI support mainnet bootstrap, contextual
header synchronization, a bounded persistent header store, on-demand complete
witness-bearing blocks, and explicit transaction submission. A managed outbound
pool discovers public peers, retains useful connections and rotates an
exploratory connection. Manually configured peers and a manual-only mode remain
available. Discovery supplies availability hints, never chain authority.

The loopback [guardian RPC profile](docs/guardian-rpc.md), with optional authentication,
supports the narrow Fedimint client profile. `--fake-bitcoind` explicitly enables
synthetic Core status; it does not establish freshness or validation.
There is **no full consensus validation, UTXO set, wallet-history service, inbound
P2P listener or automatic guardian finality binding**. This is not yet a production-ready
guardian backend or a substitute for Bitcoin Core's validation.

## How does this compare?

The main difference is **what gets validated**, not just how much state is kept:

| Approach | Validation and tradeoff |
|---|---|
| **This project** | Trusted checkpoint, contextual descendant headers and complete-block integrity; bounded recent state, but no UTXO or script validation. |
| **Pruned Bitcoin Core** | Full-node validation with old bodies discarded; retains chainstate and processes history. AssumeUTXO can make it usable earlier while historical validation runs in the background. |
| **Kyoto / Neutrino** | Compact-filter light clients: header and inclusion checks, not full spend validation. Filters help skip irrelevant blocks, not blocks a guardian must consume in full. |
| **Electrum / Esplora clients** | Indexed server queries, with proofs available depending on the protocol and client; availability and completeness still depend on the backend. Self-hosting moves that backend cost to the operator. |
| **Floresta / utreexod** | Utreexo validators use authenticated UTXO proofs instead of a full local UTXO database. Validation work, proof availability and bootstrap configuration still matter. |

**Pruned Core is the full-validation baseline. Floresta is a promising research
alternative** because its Rust components aim to reduce validation state rather
than omit spend validation. We have not adopted it or established guardian
compatibility. Finality delays and federation consensus do not supply the
validation missing from our current node.

See the [detailed comparison](docs/alternative-approaches.md) for primary sources
checked on October 12, 2026, configuration caveats and resource tradeoffs. This
is not a measured performance ranking.

## Explicit operations

The quickstart above runs bootstrap, synchronization and read-only RPC for you.
For manual control, you can run the individual operations instead. For example,
with Nix installed:

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

The default network mode discovers peers; no hand-picked public IP is required.
To use only your own node, add `--manual-only --peer 127.0.0.1:8333` to network
commands. `status` inspects a stopped node offline. Only one process should own
the database at a time.

Selected height is not proof of freshness or consensus validity. Inspect the
trust assumptions before relying on any result, and do not broadcast real
transactions merely to test connectivity.

## Learn more

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
