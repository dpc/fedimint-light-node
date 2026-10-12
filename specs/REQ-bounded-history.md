# REQ-bounded-history: Bounded recent history

## Status

The library implements inclusive retention-range arithmetic and atomic pruning
of persisted canonical headers to an explicit capacity, with an explicit redb
cache budget and bounded record decoding. Storage reports out-of-history
queries separately from future heights and keeps pinned anchor and identity
metadata outside the rolling window. Decoded body checks enforce caller byte
budgets and protocol size/weight limits before allocating scratch trees and sets;
the caller owns block allocations. Wire admission enforces explicit per-call byte
and aggregate element budgets with an allocation-free structural pass before
decoding. It does not yet retain validation context or implement networking or
global body budgets.
Logical record bounds do not establish
physical file-size or process-memory guarantees; resource accounting remains
incomplete.

## Obligation

The requester wants recent headers rather than an archival node; approximately
one year is an example, not a fixed duration or a chosen default. Configuration
must specify a finite header capacity. Retention advances with the selected chain,
not with the slowest consumer. Very old guardians must use another backend.

Keep the queryable rolling window plus only bounded validation context needed
for timestamps, difficulty transitions, and supported reorganizations. Separately
bound peer connections, forks, orphans, queued and concurrent downloads, message
and body bytes, and any body cache. Reject or evict excess untrusted state;
pruning canonical headers alone is not a memory or disk bound.
Download and decode limits apply globally, not merely per peer. Configure
the database cache explicitly within the resource budget. Caller-owned returned
blocks are outside library-owned memory accounting; no host RSS guarantee is
implied without measurement.

Requests below the queryable boundary fail explicitly as outside retained
history; do not silently perform archival recovery or switch backends.
A retained header does not guarantee body availability: suitable peers may
be absent, and this must produce an honest unavailable result with bounded
request time and work.

NODE_NETWORK_LIMITED guarantees only the recent 288 blocks, not the whole
retention window (BIP 159). Older retained bodies require suitable serving peers.
On-demand retrieval saves storage, not normal guardian download bandwidth:
guardians still consume each new full block.

These limits constrain [ARCH-fedimint-light-node](ARCH-fedimint-light-node.md)
and the fork handling in [SPEC-chain-trust](SPEC-chain-trust.md).

Protocol reference: `bitcoin/bips`, `bip-0159.mediawiki`, service-bit specification.
