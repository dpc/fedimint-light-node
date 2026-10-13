# Security boundaries

This library is not a Bitcoin consensus validator. The governing acceptance
contract and residual trust are in
[SPEC-chain-trust](specs/SPEC-chain-trust.md); concrete bootstrap policy and
limits are in the [operation guide](docs/guide.md#initial-trusted-bootstrap).

## Trusted inputs

The embedding caller controls source operators, HTTPS roots, peer addresses,
local time, resource configuration and database paths. Different host names do
not establish independent ownership. HTTPS certificate validation authenticates
an endpoint, not the truth of its reported height or Bitcoin chain. Source
agreement and timestamp checks cannot exclude collusion, shared failures,
eclipse attacks or consensus-invalid chains. Only initial bootstrap uses those
sources; reopening must use the pinned local checkpoint.

Local database contents are trusted against malicious replacement. Use a
caller-owned directory that untrusted processes cannot rename or modify.
Publication assumes a local filesystem with atomic hard links and durable
directory sync. A crash can leave private staging files; an error after
publication can leave a complete destination. Inspect and reopen, rather than
automatically deleting, replacing or re-anchoring an existing store.

## HTTP transport and cancellation

Bootstrap enforces HTTPS, certificate validation, no proxies, no redirects, no
automatic retries, and no response decompression at runtime, including when an
embedding dependency enables extra HTTP client features. It rejects content
encodings and bounds application reply bytes before copying them into retained
buffers. These limits do not bound HTTP headers, TLS buffers or resolver memory
to that same tiny reply budget.

Application deadlines stop waiting on HTTP; they cannot forcibly stop an
already running OS name-resolution call. The HTTP client's resolver uses
blocking runtime jobs that can outlive cancellation and delay runtime shutdown.
Repeated canceled bootstrap attempts can overlap those jobs. Connection setup
can race multiple resolved addresses. Do not interpret one sequential application
request as a strict one-socket or no-background-work guarantee for HTTPS.
Callers requiring a hard process-wide shutdown or resolver resource bound need
isolation beyond this API.

P2P discovery uses DNS and bounded legacy address samples only as availability
hints. Automatic targets must pass public-unicast and default-port checks;
explicit manual targets can be private. Prefix diversity and connection counts
do not prove independent ownership or exclude eclipse attacks. The bounded
successful-peer cache is disposable, never checkpoint authority, and belongs in
a caller-owned private directory distinct from chain state.

The pool owns at most four sockets, including any pending dial, and bounded idle
drivers on the caller runtime. Canceling an operation closes its leased stream;
other connections remain maintained. Explicit `shutdown().await` closes and joins
drivers; dropping the owner aborts them. Clean-boundary transfer never reuses a
canceled partial frame. One unresolved P2P resolver job is retained per pool even
when its wait times out. OS DNS jobs may outlive cancellation and delay runtime
shutdown; multiple canceled pool instances can overlap them. OS resolver result
allocation is not bounded by the 32 admitted addresses. See
[peer management](docs/peer-management.md) for exact control budgets and limits.

There is no library-created runtime, permanent source oracle or body cache.
Synchronous bounded hashing, validation and durable publication
between awaits cannot be interrupted. A dedicated caller-owned runtime thread
can keep that work off latency-sensitive threads.

## Executable boundary

The optional CLI owns a current-thread Tokio runtime and SIGINT/SIGTERM handling;
none is installed by the library. Signal cancellation cannot interrupt synchronous
database work, block serialization or publication. Executable runtime teardown
waits at most two seconds for resolver jobs; they can still outlive cancellation.
This is not a hard shutdown-time guarantee for synchronous work or the library.

The explicit `serve` command adds a loopback-only Basic-authenticated RPC endpoint.
Basic authentication is not encryption; local users can still connect or deny service.
Use a private trusted parent directory and a mode-0600 regular credentials file;
the pre-open file check is not a hostile-directory sandbox. Credentials are not
printed. No cookie, TLS, proxy trust or public bind is implemented. Do not forward
or expose the listener. The strict one-request HTTP dialect and aggregate bounds
are in the [RPC guide](docs/guardian-rpc.md); excess work is rejected, not detached.
Cheap reads take priority between bounded network operations. A client disconnect
or timeout cancels queued/active work, including a leased P2P stream, but synchronous
validation/database work cannot be interrupted.

`--fake-bitcoind` permits only an enumerated synthetic Core-status schema, never
state recovery, full validation or a bypass of storage readiness. It deliberately
does not prove tip freshness. `--allow-rpc-submission` separately acknowledges
submission-only transaction semantics without Core's implicit fee/burn safeguards.
No standard RPC read advances durable protection; an external guardian commit
cannot be inferred from a block/hash read. This transport is not a production
guardian-finality integration.

Guardian binding is an irreversible local operator assertion, not authentication
of a remote guardian database. Bound startup checks independently supplied current
commitment and needed range. Only explicit hash-checked administration advances
protection; verified older ancestry never lowers it. A manual checkpoint does not
coordinate later commits. Operational metadata rejects unknown versions; older
binaries reject the extra record. Never bypass this with a downgrade or record edit.

Stopped-store backup holds exclusive ownership while exporting logical records.
Restore checks integrity and exact independently current state in private staging,
then publishes without overwrite. Digests prove neither authenticity nor recency;
trust the directory and retain the latest guardian/protection records separately.
An old matching manifest is not safe restore authority. Credentials and guardian
state are not included. See the [operator runbook](docs/operations.md) for failure
boundaries, interrupted publication and explicit recovery limitations.

Block-file export uses private sibling staging and atomic no-overwrite publication.
Existing entries, including aliases of the database, are refused. As with database
publication, the caller must own the parent directory and inspect an output after
a publication error rather than automatically deleting it. Status reports selected
state, never proven freshness, consensus validity or body availability. Protection
is an explicit irreversible rollback-floor choice, not a consensus finality claim.

Transaction submission accepts caller-supplied bytes, not validated spends.
The library bounds serialized size and weight before allocating outbound payloads;
it does not check scripts, input values, fees, standardness or mempool policy.
The CLI reads only bounded regular binary files in a caller-owned trusted directory,
refusing symlinks and special files. Local file I/O is synchronous; parent-directory
ownership, not a race-proof hostile-path sandbox, protects the pre-open check.
Neither interface persists transactions or creates a retry queue.

A completed transaction write is only a local transport observation, not proof of
peer receipt, acceptance, propagation or confirmation. A bounded post-write ping/pong
exchange reduces premature closing with unread control messages but is not an
acceptance acknowledgement or a guarantee against TCP resets. A peer can silently
ignore transactions, including while synchronizing. Cancellation loses the in-flight
report and may occur after delivery to one or more peers. Explicit retries can submit
the same transaction again. Configured peers see the transaction and source address;
this direct submission is not a privacy-preserving broadcast protocol.
