# Fedimint light node

An embeddable Rust Bitcoin backend for guardians that retain recent headers
and retrieve complete blocks from peers without full consensus validation.
This is a project bootstrap, **not a working node or a Bitcoin Core substitute**.

The library currently provides only tested, overflow-safe retention-window
arithmetic. There is no networking, storage, validation, adapter, or executable.
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
No resource capacities or production defaults have been chosen. Persistence
will use redb with bincode encoding; dependencies arrive with that implementation.
Actual fallback integration into Fedimint is outside this bootstrap.

## AI usage disclosure

[I use LLMs when working on my projects.](https://dpc.pw/posts/he-went-to-the-slop-side/)
