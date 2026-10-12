# Security boundaries

This library is not a Bitcoin consensus validator. The governing acceptance
contract and residual trust are in
[SPEC-chain-trust](specs/SPEC-chain-trust.md); concrete bootstrap policy and
limits are in the [README](README.md#initial-trusted-bootstrap).

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

Directly owned P2P work uses caller-resolved addresses and closes on cancellation.
There is no library-created runtime, maintenance task, permanent source oracle
or body cache. Synchronous bounded hashing, validation and durable publication
between awaits cannot be interrupted. A dedicated caller-owned runtime thread
can keep that work off latency-sensitive threads.

## Executable boundary

The optional CLI owns a current-thread Tokio runtime and SIGINT/SIGTERM handling;
none is installed by the library. Signal cancellation cannot interrupt synchronous
database work, block serialization or publication, and default runtime teardown
can wait for HTTPS resolver jobs. This is not a hard shutdown-time guarantee.

Block-file export uses private sibling staging and atomic no-overwrite publication.
Existing entries, including aliases of the database, are refused. As with database
publication, the caller must own the parent directory and inspect an output after
a publication error rather than automatically deleting it. Status reports selected
state, never proven freshness, consensus validity or body availability. Protection
is an explicit irreversible rollback-floor choice, not a consensus finality claim.
