# Bounded decoder fuzz campaigns

This separate workspace keeps `libfuzzer-sys` out of production dependencies.
Targets exercise canonical block/transaction admission and round-trip accepted
bytes. The block target derives the requested hash from the input header so
mutations reach body checks, exercises both witness policies, and sends short
headers through rejection too. Neither target validates spends or proves full
consensus equivalence.

Generate small seed corpora from existing checked-in public fixtures:

```sh
nix develop -c cargo run --locked --manifest-path fuzz/Cargo.toml \
  --example seed_corpus -- fuzz/corpus
```

With `cargo-fuzz` installed and an appropriate nightly compiler:

```sh
cargo fuzz run block --sanitizer none -- \
  -seed=604870434 -max_total_time=60 -rss_limit_mb=512 -max_len=4000000 -timeout=5
cargo fuzz run transaction --sanitizer none -- \
  -seed=3402604921 -max_total_time=60 -rss_limit_mb=512 -max_len=400000 -timeout=5
```

Run from the repository root. Use an outer process timeout for an unattended
campaign as well. Runtime flags limit one campaign, not compilation. Corpora,
artifacts and target output are ignored; preserve a crashing input as an explicit
regression rather than committing a large mutation corpus. Minimize and inspect
findings before changing admission rules.

The original recorded experiments used `cargo-fuzz 0.13.1`, the project's Rust 1.98.1 with
`RUSTC_BOOTSTRAP=1`, and `--sanitizer none`. That environment override was confined
to the fuzz command; it is not a production build setting. Coverage counters were
present, so these were coverage-guided campaigns, not random unit loops. There was
**no AddressSanitizer coverage in those runs**; libFuzzer warned that sanitizer crash hooks were
absent. A separate 61-second block campaign subsequently used `--sanitizer address`.
A short successful campaign does not establish memory safety, exhaustive
coverage, or the proposed four CPU-hours per target.

See [readiness evidence](../docs/readiness-evidence.md) for exact results and gaps.
