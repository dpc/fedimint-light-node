# Managed outbound peers

The mainnet service uses Bitcoin v1 framing, `rust-bitcoin` encoding and Tokio
sockets on the caller's runtime. Discovery is an availability mechanism, not a
vote, checkpoint source or proof of honest service.

## Selection and resources

By default the pool grows to four connections: three stable slots and one
exploratory slot. One foreground operation leases one connection at a time;
idle drivers keep the others responsive to pings and bounded announcements.
Repeated operations reuse actual connections rather than reconnecting through
an address list. Each operation can add one connection, so startup fills gradually.
There is at most one pending dial, occupying a vacant slot: **four total sockets**,
not four plus replacement attempts. Dials are paced, reserved before awaiting and
bounded by the caller's exchange deadline. This favors simple resource accounting
over fastest possible startup or replacement.
After a failed or canceled exploration attempt, the next header visit uses a
maintained connection when one exists: a large dead-candidate pool cannot starve
useful foreground service. Body retrieval prefers maintained sockets outright.

New and successful candidates have separate limits of 256 and 64, plus at most
16 explicit manual addresses. Admission allows at most 32 new entries per source
group and four per destination group in each population. Selection chooses a
source group uniformly, then an eligible address, using independently seeded
`rand::StdRng` randomness. IPv4 /16 and native IPv6 /32 prefixes provide a modest
diversity heuristic, not ownership or autonomous-system independence. Automatic
connections never relax this diversity rule to fill slots; explicit manual peers
may share a group.

Stable vacancies prefer historically useful candidates, while exploration
prefers new ones. Promotion requires checked canonical header overlap, selected
valid descendants, authenticated bootstrap ancestry or a matching checked body.
Handshake, uptime, pong, advertised height and empty headers do not qualify.
Healthy stable slots remain; an exploratory slot rotates after a randomized
10–20 minutes. Provisional slots also expire, and stable slots expire 24 hours
after their last useful operation. Foreground operations defer rotation until
the lease returns. Failures reserve randomized exponential backoff, initially
30–60 seconds and capped at an hour. Three failures disable a never-useful
automatic candidate until it ages out. Repeated advertisements cannot reset
failures. Explicit manual peers remain configured through failures and use fair
retry order. New entries age out after one day; successful entries after seven days,
including across cache reload.

The pool has **one global foreground header candidate and block payload/decoder**,
not one per connection. Existing aggregate body/submission budgets include new
handshakes. Returned blocks belong to callers. Idle traffic has a separate shared
1 MiB/minute receive allowance, with at most 256 KiB and 64 frames/minute per
connection. Per-connection windows survive leases; shared refunds apply only to
the window that issued the reservation. Idle framing may retain at most one 162,003-byte header payload per
driver, plus at most 2,000 decoded headers while discarding a bounded announcement;
unsolicited bodies fail before payload allocation. There are no body
queues. Each driver has one lease command/result and one coalesced 32-address
sample. Quotas bound application state, not kernel TCP buffers, allocator overhead
or host RSS.

## Discovery and cache

The default seed list contains independently operated mainnet DNS seeds from
Bitcoin Core v30.0: Pieter Wuille, Sjors Provoost and Stephan Oeste. Up to 32 numeric
results per seed enter candidate admission. Seed cycles are at least 30 minutes
apart. Only one resolver job may be unresolved in a pool; a timed-out wait retains
that job rather than starting another. A pool with existing candidates need not
wait for a pending result.

Each connection requests addresses once. Legacy `addr` is streamed: canonical
count (at most 1,000) and exact payload size (at most 30,003 bytes) pass before
allocating a sample of at most 32 endpoints. Stale, far-future, non-serving and
nonpublic entries are discarded. The implementation does **not negotiate BIP155
`addrv2`**; that command is rejected before payload allocation, not decoded by a
general allocating parser. Tor, I2P, CJDNS and BIP324 are not supported.

DNS, gossip and cache entries all use the same conservative public-unicast
IPv4/IPv6 filter, mainnet port 8333 and normalized IPv4-mapped addresses. Private,
loopback, link-local, multicast, documentation, transition and special-use
destinations cannot become automatic dial targets. Explicit manual addresses
may be local or use other ports. `--manual-only` disables DNS, gossip dialing and
cache use; the embedding equivalent is `PeerPoolConfig::manual_only()`.

The optional cache stores at most 64 successful public endpoints in a small,
versioned, mainnet-bound file. It is disposable availability history, not peer
identity or chain state. Corruption is ignored and never changes the checkpoint.
Cache write failures are observable through `HeaderSync::peer_counts()`.
Configure the path in a caller-owned directory, distinct from chain state.
Publication is atomic but intentionally not a durable chain-state transaction.

## Ownership and cancellation

An idle driver waits on a cancellation-safe peek or a lease command. After
starting a frame it completes exactly that frame under a ten-second deadline
before checking the command again. It never cancels `read_exact` and starts a
fresh decoder on the same stream. Idle ping intervals are two minutes, with
30 seconds to answer. Flooding cannot postpone a pending lease indefinitely.

Foreground errors, uncertain writes, timeouts and cancellation close their
connection instead of returning a dirty stream. Other idle connections remain
maintained. Canceled checkout handles stay owned until joined before replacement
dials, including completed-but-unconsumed stream results. `shutdown().await`
closes and joins idle drivers; dropping the owner aborts them. No process signals,
global tracing, runtime or listener are created by the library.

OS DNS resolution cannot be forcibly stopped by dropping a Tokio lookup.
Its internal result allocation is not bounded by the 32 admitted addresses.
Even after an async resolver task is canceled, the OS job may outlive the pool
and delay runtime teardown. Repeatedly creating/dropping separate pools can
overlap such jobs; there is no process-global resolver limit. Applications needing
a hard DNS memory/shutdown guarantee need resolver isolation. HTTPS bootstrap
has its separately documented resolver limits.

## Why this approach

The connectivity study compared extending this checked layer against Kyoto,
Nakamoto, `bitcoin-address-book` and `rust-bitcoin-p2p`. Kyoto's current private
network layer and broad buffering/queues are not a drop-in match for these
resource limits. Nakamoto's state machine is reusable in principle, but adapting
its chain/filter interfaces and older Bitcoin types is a larger change.
`bitcoin-address-book` still leaves lifecycle policy here and lacks the desired
keyed/injectable randomness. Keeping the narrow checked parser avoids a second
chain authority or parallel networking stack. This is not a claim that a small
custom pool matches Bitcoin Core's eclipse defenses.

Primary references: Bitcoin Core v30.0 `src/kernel/chainparams.cpp` and
`src/addrman.h`; BIPs 31, 144, 155, 159 and 324; Kyoto `bip157` 0.7.0,
Nakamoto `nakamoto-p2p` 0.4.0 and `bitcoin-address-book` 0.1.1 source.
