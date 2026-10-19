# Bounded transaction submission

`HeaderSync::submit_transaction` fills the outbound transaction operation without
adding a mempool, wallet, fee estimator, queue or guardian adapter. It borrows the
same owner exclusively as header synchronization and body retrieval. One call owns
at most one leased socket and one serialized payload, tries pool peers sequentially,
and never modifies the chain store. Repeating a call explicitly rebroadcasts.
Idle connections remain maintained under the separate bounded pool control budget.

## Observations, not acceptance

An admission error occurs before networking or outbound payload allocation.
Otherwise the report includes transaction identifiers, every selected peer, whether
its payload write started, its write result and an optional post-write observation.
`report.submitted()` counts **completed local frame writes**, even if a later
ping, peer disconnect or deadline fails. An `Ok` report with count zero is not a
successful submission. CLI exit success requires a positive count.

There is no transaction acceptance acknowledgement. A completed write does not
prove receipt, validation, mempool entry, propagation or confirmation. After writing,
the client sends a fresh-nonce ping and waits for the matching pong under at most
two seconds and the existing visit/total deadlines, with at most 32 received frames.
This drains queued control messages to reduce premature closing with unread data;
it cannot guarantee graceful close or transaction processing. Unrelated pongs do
not satisfy the observation. Timeout or another failure never erases a completed
write. Dropping the future closes its current socket and loses the whole report:
earlier or in-progress delivery may already have happened. The caller decides when
to retry; no exactly-once or automatic rebroadcast promise exists.

## Bounds and admission

Defaults allow three visits, 20 seconds per visit, 60 seconds total including the
owner's pacing, 2 MB received and 1,210,000 bytes sent across the entire call.
Every frame reservation includes its 24-byte envelope. Declared inbound payloads
and whole outbound frames are charged before I/O, so failures conservatively
consume reserved bytes, not just observed bytes. The handshake accepts at most
32 frames; ignored messages also consume bytes, message counts and absolute time.
No peer can extend a deadline by trickling data or sending pings. Candidates and
attempts remain finite; dial pacing and retry state are reserved before awaiting,
including cancellation. Maintained sockets return to the back of the service queue.

Canonical serialized bytes and weight are each capped at 400,000, independently
configurable downward. An allocation-free encoding sink rejects excess bytes before
payload allocation, hashing or weight arithmetic. Empty input/output lists fail;
otherwise admission is resource policy, **not** consensus, script, spend, fee or
standardness validation. Raw-file admission first scans canonical transaction
structure without allocation, using the same scanner as block admission. It bounds
decoded inputs, outputs and witness items before constructing collections.

Caller-owned input transactions, decoded return values, vector spare capacity,
allocator overhead and kernel socket buffers are not a process-memory guarantee.
Synchronous bounded serialization, hashing and decoding cannot be interrupted.
The CLI additionally refuses oversized or non-regular files before reading and
caps reads even if the trusted file grows.

## Protocol choice and evidence

This is deliberately direct `tx` submission rather than an inventory relay service.
The shared handshake requires protocol version at least 70012, remote `relay=true`,
`NODE_WITNESS`, and either full or limited network service. Services and relay are
peer-selection hints, not honest-delivery guarantees. Our default `relay=false` asks the
peer not to send transaction announcements to us; it does not forbid sending our
transaction to that peer. No serving capability is advertised.
Explicit `--mempool` instead requests `relay=true` for the separate bounded
[observation view](guardian-rpc.md#approximate-mempool-observation).
That choice does not change submission authorization or imply transaction acceptance.

Primary sources checked against current Bitcoin Core source and the stable v30.0
reference:

- Bitcoin Core `src/net_processing.cpp`, `PeerManagerImpl::ProcessMessage`'s `TX`
  branch, explicitly permits unsolicited transactions and decodes witness data.
  `RejectIncomingTxs` applies the peer's local block-only/feeler policy. Its IBD
  path can silently discard a transaction, demonstrating why pong is not acceptance.
- Bitcoin Core `src/node/txdownloadman_impl.cpp`,
  `TxDownloadManagerImpl::ReceivedTx`, does not require an outstanding request to
  consider a new transaction; already-known/rejected transactions may be ignored.
- BIP 144 defines witness serialization under the same `tx` command and the
  witness service bit. BIP 339 negotiates inventory identifiers; this client sends
  no inventory and therefore need not negotiate witness-ID relay.
- BIP 31 defines matching ping/pong nonces as connection observations, not
  transaction acknowledgements.

Source locations: `bitcoin/bitcoin` on GitHub (`master` and `v30.0`), and
`bitcoin/bips` (`bip-0144.mediawiki`, `bip-0339.mediawiki`,
`bip-0031.mediawiki`). These checks justify interoperability choices, not permanent
guarantees about future peers. Tests use only deterministic local scripted peers,
including exact witness bytes, legacy transactions, replay, refusals, disconnects,
chatter, byte/message/time budgets and cancellation after delivery. They are not a
live Bitcoin Core acceptance or public-network propagation test.
