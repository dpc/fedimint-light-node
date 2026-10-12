# Comparing Bitcoin backend approaches

**Pruned Bitcoin Core is the full-validation reference; Floresta/Utreexo is a
promising lightweight-validation direction to investigate.** Our current node
instead makes an explicit bounded-state, partial-validation tradeoff.

Sources were checked on **October 12, 2026**. Pinned development snapshots below
are not claims that every inspected change appears in a published release. This
is a focused source review, not a consensus audit, deployment certification,
performance benchmark or decision to change this project's architecture.

## Our current boundary

This project bootstraps mainnet from agreement among configured trusted HTTPS
sources, pins a checkpoint, and checks contextual descendant headers: linkage,
expected difficulty, PoW, timestamps and buried version floors. It selects
greater cumulative observed valid-header work subject to the retained window and
protection floor. Default retention is 52,560 headers, approximately one year at
the target block interval, not a guarantee of one calendar year.

Complete witness-bearing bodies are retrieved on demand and checked for
mechanical integrity. The node does not maintain a UTXO set or validate scripts
and contextual transaction consensus rules. Bodies are not persistently cached.
See the [chain-trust contract](../specs/SPEC-chain-trust.md),
[architecture/status](../specs/ARCH-fedimint-light-node.md) and
[security boundary](../SECURITY.md).

The maintained outbound peer pool discovers candidates, retains useful
connections and rotates an exploratory connection. Multiple peers improve
opportunities to observe competing chains and obtain bodies; they do not
establish universal freshness, independent ownership or immunity to eclipse.
See [peer management](peer-management.md) for its implemented bounds and policy.

These distinctions matter even when independent Fedimint guardians consume
sufficiently old blocks: delay does not prove transaction validity, and federation
agreement cannot manufacture missing Bitcoin validation. Actual guardian
integration remains separate work.

## Pruned Bitcoin Core and AssumeUTXO

**Pruning removes archival block/undo storage after processing; it does not
replace the UTXO database or omit ongoing spend validation.** Core still keeps
chainstate and block-index information. A guardian operator must size retention
for its finality delay, downtime, recovery and reorg needs: knowing a block hash
does not mean the local pruned node can return that body's bytes. This is the
strong conventional baseline, not equivalent to an archival node in disk
requirements.[^C1]

AssumeUTXO is a separate startup option. Core checks an imported snapshot against
a recognized hardcoded hash, validates descendants from that state, and
reconstructs earlier chainstate in the background before checking the result.
Trust while using the unverified snapshot is distinct from the snapshot
supplier's identity: a supplier cannot freely substitute different state under
the recognized commitment. Background work is not eliminated; concurrent
chainstates and downloaded snapshot material can increase transitional
storage.[^C2]

Also distinguish **AssumeValid** from AssumeUTXO. Core's `-assumevalid` may skip
historical scripts under its conditions; `-assumevalid=0` requests verification
of all scripts. An all-history, every-script experiment must state that setting
rather than equate an ordinary default sync with it.[^C3]

For this project, evaluate pruned Core first if production validation is the
objective. Its process/RPC interface is a deployment alternative, not a drop-in
implementation of our caller-owned Rust library. An RPC-shaped compatibility
wrapper around our node would not inherit Core's security properties.

## Compact-filter light clients: Kyoto, Neutrino, Nakamoto

BIP157/158 serves compact block filters so clients can select relevant blocks
locally without submitting their entire script set to a remote indexer. Filter
commitments are **not Bitcoin header consensus commitments**. BIP157 describes
cross-peer comparison and dispute handling, and its security model requires an
honest reachable peer. These mechanisms do not validate every Bitcoin
spend.[^S1]

Kyoto is a Rust/Tokio client with checkpoint starts, block retrieval and
transaction broadcast. Neutrino is a Go light-client library with lazy
filter/block queries, including complete block retrieval. Neither requires that
its consumer be a wallet or inherently prevents guardian block downloads: the
mismatch is filter-oriented synchronization and the absence of full UTXO/script
consensus validation, not inability to request bodies.[^S2][^S3]

Compact filtering is valuable when the application can skip irrelevant bodies.
**If a guardian backend contract requires each consumed complete block, filter
matching cannot eliminate those downloads.** Retained headers, filter headers,
filters, client persistence, queues and runtime memory must be measured
separately rather than compared with our raw header database alone.

Nakamoto provides a modular Rust SPV/compact-filter implementation and a useful
I/O-separated architecture reference. Its inspected repository HEAD is dated
March 1, 2024; its package/type integration requires more adaptation than Kyoto's
Rust/Tokio stack. That describes the inspected source, not proof that the
software is unusable.[^S4]

These projects remain useful P2P/architecture references, not full-validation
upgrades. Implementation adaptation costs are distinct from protocol-level
limitations.

## Electrum- and Esplora-backed clients

Server-backed clients do not all have the same trust model. Electrum's client
verifies confirmed transaction inclusion using SPV and queries multiple servers
for headers. Its documentation acknowledges that a server can omit relevant
transactions and learns the script queries associated with that client;
unconfirmed results have additional trust. Electrum does not simply accept
every server assertion without verification.[^E1]

Esplora exposes headers, raw complete blocks, transaction proofs, indexed
histories and fee data over HTTP. The API makes proofs available but does not
force a client to verify them, authenticate chain selection or check Bitcoin
consensus. Security depends on the consuming client and the assertions it
accepts.[^E2]

Self-hosting a validating node and indexer can avoid handing those decisions to
an outside operator, but moves the full backend/index cost onto the deployment.
A remote service remains able to withhold data. Our ongoing P2P fetches reduce
dependence on a permanent index service, but our HTTPS checkpoint is still an
external bootstrap trust decision.

For guardians needing complete blocks, Esplora's raw block endpoint is relevant.
Electrum's wallet-history protocol is not inherently a complete-block backend;
Electrum support alone does not establish guardian compatibility.

## Utreexo, Floresta and utreexod

Utreexo replaces a validator's large locally materialized UTXO set with
accumulator state. Peers supply spent-output data and membership proofs; a
validating client checks these against its evolving accumulator and uses the
authenticated previous outputs for transaction validation. **This reduces
required validation state rather than deliberately removing UTXO/script
validation.** The cryptographic construction does not require a trusted proof
provider when anchored in correctly established state, but unavailable proofs
can stop progress.[^U1][^F2]

Accumulator roots are only one component. A node still has headers, chain
metadata, caches, networking buffers, decoded blocks/transactions, proof
processing and possibly wallet/filter/mempool state. Genesis validation still
processes history; fast-start modes add explicit assumptions. Proof data adds a
bandwidth dimension absent from ordinary block-only delivery. Bridge/proof-serving
nodes keep more state and perform proof-generation work. Their availability and
operational diversity must be assessed separately from the ordinary Bitcoin
body-serving network. This review did not measure public peer counts.[^U1][^U2]

### Floresta: a Rust embedding lead with configuration-sensitive guarantees

Floresta offers Rust components and a daemon with watch-only wallet/Electrum
interfaces. Its documentation warns that it is experimental. Those components
are a credible research reuse path, not evidence of a tested guardian
adapter.[^F1]

At inspected development snapshot `aaef36dc11f48ab249e1fdbb2c3c17654cb40847`:

- The daemon constructs `assume_utreexo: !no_assume_utreexo` and
  `backfill: !no_backfill`, so fast-start and backfill are enabled by default.
  The reusable `FlorestadConfig::default()` instead sets both fields false;
  its default is not the daemon's fast-start mode. Embedding must make an
  explicit choice.[^F3]
- `floresta-node` enables `floresta-chain`'s `bitcoinkernel` feature. Normal
  daemon script verification is therefore enabled even though direct use of
  `floresta-chain` alone has empty default features. The chain crate
  conditionally compiles its script-engine call; low-level embedders must
  preserve that feature and appropriate validation policy. Script-engine reuse
  is not proof that every surrounding consensus rule is identical to Core.[^F4]
- `--assume-valid 0` disables the ordinary chain's historical script shortcut;
  `--no-assume-utreexo` selects genesis sync rather than trusted fast-start.
  **A fresh daemon using both flags is the stronger supported configuration to
  evaluate** when historical independent validation is the goal. This is a
  source-derived configuration recommendation, not a completed initial block
  download (IBD) test.[^F3][^F4]
- Backfill reconstructs a partial chain from genesis, including accumulator
  and transaction checks, but its construction sets `assume_valid: false`;
  partial-chain validation passes that value directly as `verify_script`.
  The resulting source-based inference is that this backfill does not execute
  every historical script, even with the ordinary chain's CLI assume-valid
  setting disabled. This was independently rechecked in the pinned source,
  not demonstrated by a runtime test. Treat fast-start/backfill and fresh
  script-enabled genesis validation as distinct modes.[^F5]

The last point qualifies the documentation's broad backfill claim; it is not a
claim that ordinary post-bootstrap validation is absent or a comprehensive
vulnerability assessment. Compare Floresta's strongest genesis/script-enabled
configuration fairly with other validators rather than judging the whole
approach only by its fast-start defaults.

Floresta prunes bodies; a guardian still needs a reliable route to complete
delayed/historical blocks. Its wire layer supports block requests, but body
retention, witness encoding, validation status at delivery, reorg behavior,
runtime ownership and cancellation require an integration exercise. A compact
accumulator alone does not settle those questions.[^F1][^F5]

### utreexod: independent comparison and proof infrastructure

utreexod is a Go full-node implementation with Utreexo support. Its README labels
it beta and discourages significant-value mainnet use. The default starts from
hardcoded AssumeUtreexo state and skips IBD; `--noassumeutreexo` requests genesis
IBD. Pruning and wallet support are independent choices. The inspected
documentation did not establish a Core-equivalent background validation
guarantee for the default assumed state; this comparison does not grant
one.[^U2][^U3]

Its validation code skips scripts before the last configured checkpoint. A
historical-script experiment should explicitly evaluate
`--noassumeutreexo --nocheckpoints` on a fresh database; disabling AssumeUtreexo
alone is insufficient to describe every historical script as executed. The
README's `--prune=0` example is not a requirement for genesis validation:
archival retention and validation are separate axes.[^U3]

Its bridge-node mode is useful for studying proof availability and deployment
costs, while its Go daemon is less direct Rust-library reuse than Floresta. Test
actual protocol/version compatibility rather than assuming two Utreexo
implementations automatically interoperate.[^U2]

## Resource and operational comparison

| Dimension | Our current node | Pruned Core | Compact-filter/server clients | Utreexo validator |
|---|---|---|---|---|
| Persistent validation state | Bounded recent headers/context; no UTXO state | UTXO chainstate, index, retained blocks/undo | Headers/filter/client state or indexed-server dependency; varies by client | Accumulator plus headers/metadata and implementation-specific retained state |
| Startup | Trusted recent anchor and bounded ancestry | Historical sync; optional snapshot plus background reconstruction | Header/filter/index retrieval; checkpoint/client dependent | Genesis processing or explicitly assumed state |
| Ongoing body traffic | Complete requested bodies | Processes new blocks | Can skip irrelevant blocks when consumer permits | Validates blocks plus required proof/previous-output data |
| CPU | Header/body integrity, not full spend validation | Consensus and scripts, subject to configured historical shortcuts | Query/filter/inclusion work, not general full validation | Consensus/scripts plus accumulator proof/update work |
| Availability dependencies | Trusted bootstrap sources initially; body/header peers afterward | Ordinary Bitcoin peers; retained-body policy | Filter peers or chosen service operators | Proof-capable peers/bridges as well as block data |
| Whole-process RAM | Not equal to configured DB cache or serialized headers | Not equal to UTXO disk size | Not equal to downloaded filter/header sizes | Not equal to accumulator-root size |

These are architecture-level dimensions, **not numerical performance rankings**.
Useful comparisons must separate fresh bootstrap, time until first usable
result, completed historical validation, steady-state operation, catch-up after
downtime and reorg/failure workloads.

## Follow-up research

No alternative has been adopted, benchmarked or integrated by this comparison.
The most useful next investigations are:

1. **Establish the pruned Core baseline.** Match guardian delay, required block
   coverage, restart/downtime and fee/broadcast needs. Report AssumeValid and
   AssumeUTXO settings.
2. **Evaluate Floresta feasibility.** Use a pinned release and separately pinned
   development snapshot, script-enabled daemon, fresh genesis/no-assumption
   configuration and an explicitly labeled fast-start configuration. Clarify
   backfill script policy and assumed-state reconciliation with upstream before
   relying on them.
3. **Measure full workloads.** Track peak/RSS memory, all disk files, wire bytes
   including proofs, CPU time, startup latency and historical-validation
   completion independently. Accumulator size and header-store microbenchmarks
   are not node-footprint comparisons.
4. **Exercise guardian block semantics.** Check full witness bodies after the
   application delay, missed blocks after downtime, selected-hash consistency
   through reorgs, unavailable peers/proofs and transaction-submission/fee-policy
   behavior. An Electrum server or a `getblock` method name does not prove
   compatibility.
5. **Study utreexod as proof infrastructure.** Determine interoperability,
   proof-source diversity and the operational cost of federation-owned bridges.
   Federation consensus does not replace missing Bitcoin validation.

## Primary sources

All sources were accessed **October 12, 2026** during the research underlying
this document. Immutable URLs identify inspected code; moving documentation
URLs record documentation observations, not permanent implementation claims.

[^C1]: [Pruning release/design explanation](https://bitcoincore.org/en/releases/0.11.0/#block-file-pruning).
    Historical introduction, not a claim about the newest release.
[^C2]: [Snapshot usage](https://github.com/bitcoin/bitcoin/blob/60afa0b8d71ff0959eac27fe73fadafe72b70cce/doc/assumeutxo.md)
    and [design](https://github.com/bitcoin/bitcoin/blob/60afa0b8d71ff0959eac27fe73fadafe72b70cce/doc/design/assumeutxo.md).
[^C3]: [AssumeValid option](https://github.com/bitcoin/bitcoin/blob/60afa0b8d71ff0959eac27fe73fadafe72b70cce/src/init.cpp).
    Snapshot dated October 12, 2026; not a release-version assertion.

[^S1]: [BIP157](https://github.com/bitcoin/bips/blob/master/bip-0157.mediawiki),
    particularly Client Operation and Security Considerations.
[^S2]: [Kyoto scope/behavior](https://github.com/2140-dev/kyoto/blob/0169dc3514c8238147302535a1123d5c37468de6/doc/DETAILS.md)
    and [manifest](https://github.com/2140-dev/kyoto/blob/0169dc3514c8238147302535a1123d5c37468de6/Cargo.toml).
    Snapshot dated September 25, 2026; manifest `bip157` version 0.7.0.
[^S3]: [Neutrino README](https://github.com/lightninglabs/neutrino/blob/46b0845266c40a4fb6474f6979a6eaa2ca3d0871/README.md).
    Snapshot dated October 7, 2026.
[^S4]: [Nakamoto source](https://github.com/cloudhead/nakamoto/tree/76ab7a3b6207373399cd15a90037294bb08beeb5).
    Snapshot dated March 1, 2024; manifest version 0.4.0.

[^E1]: [Electrum's trust explanation](https://electrum.readthedocs.io/en/latest/faq.html#does-electrum-trust-servers).
[^E2]: [Esplora API](https://github.com/Blockstream/esplora/blob/master/API.md),
    especially raw block/header, Merkle-proof and self-hosting sections.

[^U1]: [Original Utreexo paper](https://eprint.iacr.org/2019/611).
    Cryptographic construction and resource tradeoff, not a current benchmark.
[^U2]: [utreexod README](https://github.com/utreexo/utreexod/blob/4122e36cb62feefb53f8d35e989b5e22346df4f1/README.md),
    including beta warning, default startup and bridge-node mode. Snapshot dated
    October 11, 2026; implementation claims use this snapshot, not a release binary.
[^U3]: [utreexod configuration](https://github.com/utreexo/utreexod/blob/4122e36cb62feefb53f8d35e989b5e22346df4f1/config.go)
    and [script/checkpoint validation](https://github.com/utreexo/utreexod/blob/4122e36cb62feefb53f8d35e989b5e22346df4f1/blockchain/validate.go).
[^F1]: [Floresta README](https://github.com/getfloresta/Floresta/blob/aaef36dc11f48ab249e1fdbb2c3c17654cb40847/README.md)
    and [experimental warning in its book](https://getfloresta.github.io/floresta-docs/).
    Development snapshot dated October 12, 2026; not a claim that all inspected
    behavior has shipped in a release.
[^F2]: [Authenticated proof/chain validation](https://github.com/getfloresta/Floresta/blob/aaef36dc11f48ab249e1fdbb2c3c17654cb40847/crates/floresta-chain/src/pruned_utreexo/chain_state.rs),
    `validate_block`, `connect_block`, `verify_script`.
[^F3]: [Daemon arguments](https://github.com/getfloresta/Floresta/blob/aaef36dc11f48ab249e1fdbb2c3c17654cb40847/bin/florestad/src/cli.rs),
    [CLI-to-config wiring](https://github.com/getfloresta/Floresta/blob/aaef36dc11f48ab249e1fdbb2c3c17654cb40847/bin/florestad/src/main.rs),
    [library configuration](https://github.com/getfloresta/Floresta/blob/aaef36dc11f48ab249e1fdbb2c3c17654cb40847/crates/floresta-node/src/florestad.rs)
    and [run documentation](https://github.com/getfloresta/Floresta/blob/aaef36dc11f48ab249e1fdbb2c3c17654cb40847/doc/run.md).
[^F4]: [Node script dependency](https://github.com/getfloresta/Floresta/blob/aaef36dc11f48ab249e1fdbb2c3c17654cb40847/crates/floresta-node/Cargo.toml),
    [chain feature defaults](https://github.com/getfloresta/Floresta/blob/aaef36dc11f48ab249e1fdbb2c3c17654cb40847/crates/floresta-chain/Cargo.toml)
    and [conditional script call](https://github.com/getfloresta/Floresta/blob/aaef36dc11f48ab249e1fdbb2c3c17654cb40847/crates/floresta-chain/src/pruned_utreexo/consensus.rs),
    `verify_transaction` / `verify_input_scripts`.
[^F5]: [Backfill construction/completion](https://github.com/getfloresta/Floresta/blob/aaef36dc11f48ab249e1fdbb2c3c17654cb40847/crates/floresta-wire/src/p2p_wire/node/running_ctx.rs),
    `backfill`; [partial-chain construction](https://github.com/getfloresta/Floresta/blob/aaef36dc11f48ab249e1fdbb2c3c17654cb40847/crates/floresta-chain/src/pruned_utreexo/chain_state.rs),
    `get_partial_chain`; [partial-chain script boolean](https://github.com/getfloresta/Floresta/blob/aaef36dc11f48ab249e1fdbb2c3c17654cb40847/crates/floresta-chain/src/pruned_utreexo/partial_chain.rs),
    `validate_block`.
