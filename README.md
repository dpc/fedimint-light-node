# Fedimint light node

A **research project** exploring the lightest practical Bitcoin node for Fedimint
guardians without ceding security to third-party backends.

The idea is to give each guardian its own small Bitcoin backend: retain recent
headers, follow the greatest cumulative valid-header proof of work observed, and
download complete blocks only when needed. Avoiding a full historical database
and UTXO set makes independently operated guardian backends much less demanding.

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

There is **no full consensus validation, UTXO set, wallet-history service, inbound
listener or Fedimint guardian adapter**. This is not yet a production-ready
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

## Try it

With Nix installed:

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
