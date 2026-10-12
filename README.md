# Fedimint light node

An embeddable Rust Bitcoin backend for guardians that retain recent headers
and retrieve complete blocks from peers without full consensus validation.
This is an incremental implementation, **not a working node or a Bitcoin Core substitute**.

The library provides tested retention-window arithmetic and a synchronous,
bounded redb header store with versioned bincode records. Callers supply the
anchor, chain identity, header capacity and database cache budget. Atomic suffix
replacement and pruning preserve the pinned identity across restart. Storage
checks linkage and record consistency, **not Bitcoin header validity or trust**.
There is no networking, validation pipeline, guardian adapter, or executable.
The intended boundaries are described in
[ARCH-fedimint-light-node](specs/ARCH-fedimint-light-node.md).

## Development

Enter the development environment with `nix develop` (or allow `.envrc`).
Run `treefmt`, `nix build -L .#ci.tests`, and `nix develop -c selfci check`.
The test gate runs real workspace tests with nextest; SelfCI also checks
formatting, prose, lockfile freshness, builds, and Clippy.

## Unsettled integration choices

Supported networks, bootstrap quorum and confirmation margin, fee policy,
and a Fedimint Rust adapter versus a small RPC transport remain undecided.
No resource capacities or production defaults have been chosen. The store starts
at a caller-supplied anchor; backfill, separately bounded validation context,
schema migration and capacity resizing are not implemented. A retained suffix
alone does not provide enough context for future consensus checks. Record limits
and an explicit cache budget are not a measured RSS or physical file-size bound.
Actual fallback integration into Fedimint remains outside this implementation.

## AI usage disclosure

[I use LLMs when working on my projects.](https://dpc.pw/posts/he-went-to-the-slop-side/)
