# Block fixture

`testnet-segwit.raw` is the 4,319-byte Bitcoin test network block
`000000000000045e0b1660b6445b5e5c5ab63c9a4f956be7e1e69be04fa4497b`,
copied unchanged from the official `rust-bitcoin/rust-bitcoin` repository at tag
`bitcoin-0.32.102`, path
`bitcoin/tests/data/testnet_block_000000000000045e0b1660b6445b5e5c5ab63c9a4f956be7e1e69be04fa4497b.raw`.
Its SHA-256 is
`469b9daa241d3dafe495d2e63ccc553b3b465c0ea20f7150e7dfe7f20269bed5`.
This public block data provides an independent witness-commitment vector,
rather than generating every successful commitment with the checked dependency.

## Mainnet headers

`mainnet-headers-0-2035.raw` contains the 2,036 consecutive 80-byte Bitcoin mainnet
headers from heights 0 through 2,035 (162,880 bytes). Retrieved from the public
`electrum.blockstream.info:50002` node using `blockchain.block.headers` at heights
0 and 2,016. This is public ledger data, not a source of production trust.
Tests check linkage, PoW and contextual difficulty from genesis through the first
retarget, and exercise bootstrap and restart with bounded timestamp context.
The fixture includes a full difficulty period because an anchor immediately
before a retarget requires its authenticated period-start header.
Its SHA-256 is
`f5865ced1b5f13290d029e49f69f9ca57ec89eca71376c85e1a056e564d3f081`.

## Divergent header branches

`mainnet-headers-477792-478562.raw` contains mainnet heights 477,792 through
478,562 from `electrum.blockstream.info:50002`. `bch-headers-478559-478562.raw`
contains the first four Bitcoin Cash fork headers from
`bch.imaginary.cash:50002`. Both use `blockchain.block.headers`. They share the
mainnet parent at height 478,558, and these initial fork headers still satisfy
mainnet header difficulty rules. They provide actual divergent mined branches
for testing work selection, suffix replacement, pruning and protection, without
a test-only PoW bypass. Their acceptance is **not** Bitcoin Cash network support
or validation of their block bodies: a header-only checker cannot distinguish
consensus-invalid bodies merely from valid linkage, work and timestamps.
Their SHA-256 values, in the same order, are
`23c2cc5f0feed363a28906c3555378956d4710e5d7ada07a7e6fb6733271be86`
and `419278117f953998d807df78055ffb7072905f012013f242f69578c6e7bb33f3`.

## Core shared-check vectors

`core-shared/` contains a 227-byte Core-generated regtest block, a changed
coinbase locktime (bad transaction Merkle root), and a changed coinbase witness
reserved value (bad witness commitment). On October 13, 2026,
`misc/core-shared-fixtures.py` generated the block in one isolated Core v31.1.0
instance and submitted both mutations followed by the valid block to a second
fresh instance. Both had networking and P2P listening disabled; RPC bound only
loopback. No live transactions were submitted. The manifest records exact
Core decisions, binary and fixture digests, and the block hash.

The library matches those three **shared mechanical body checks** with active
witness rules. This is not a mainnet header test, an independent mainnet node,
or full-consensus equivalence. Regenerating produces a new timestamp/hash and
requires updating the manifest and reviewing the captured decisions.
