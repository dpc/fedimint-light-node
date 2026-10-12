# Fedimint light node

An embeddable Rust Bitcoin backend for guardians that retain recent headers
and retrieve complete blocks from peers without full consensus validation.
This is an incremental implementation, **not a working node or a Bitcoin Core substitute**.

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
Neither API authenticates the requested hash or establishes consensus acceptance.
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

## Integrity boundary

Body checks borrow an already decoded block. An allocation-free counting sink
enforces the caller's byte budget (capped at 4 MB) before scratch allocation and
hashing. Scratch trees and sets scale with accepted transaction/input counts;
the caller owns existing block allocations. The `block_decode` entry point first
scans complete payloads without allocation, enforcing explicit byte and aggregate
element budgets and rejecting malformed lengths, encodings that are not canonical,
truncation and trailing bytes. It then uses `rust-bitcoin` decoding and the same
integrity checks. Structural budgets bound decoded storage growth, not exact
allocator bytes or RSS. Global download limits and process memory accounting
remain unimplemented. A body rejection must not poison the
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
