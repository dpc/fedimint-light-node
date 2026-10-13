# Guardian operations and recovery

**Bound mode is a manual safety checkpoint, not automatic guardian-finality
coordination.** Standard Core RPC reads do not acknowledge a durable guardian
commit. The unchanged Fedimint caller can advance past the protected floor and
retries permanent retention errors. Do not treat a successful startup preflight
as permission for unattended production operation.

## Identity and preflight

Use a private directory, an unprivileged service account, local durable storage
and independently retained operator records. Stop the guardian and node before
administrative changes. Generate one random non-secret 32-byte guardian identifier
(64 hex digits) and record its association with that particular guardian database.
Do not reuse it for another guardian, a reset database or a different federation.
This is an operator assertion, not cryptographic attestation of Fedimint state.

Obtain these values from the **guardian's durable database**, not from this node:

* `COMMITTED_HEIGHT`: last irreversibly consumed block **height**, not block count;
* `COMMITTED_HASH`: hash of that block;
* `OLDEST_NEEDED`: oldest block still needed, including rescans. A caught-up
  guardian can require selected tip plus one. It cannot require pruned history.

No guardian database extractor is implemented here. Independently confirm the
network, height-one identity and original anchor against your records using
`status`. On a stopped, previously initialized store:

```sh
# Replace these with independently recorded values; never infer guardian state
# from getblockcount, the backup manifest or the node's selected tip.
NODE=/opt/fedimint-light-node/bin/fedimint-light-node
DB=/var/lib/fedimint-light-node/chain.redb
CAPACITY=52560 # Use this store's creation-time capacity, not a new policy.

# Initial binding only: protect the checked selected commitment, then bind.
# A mismatched hash, unavailable ancestry or conflicting identity is an error.
"$NODE" --db "$DB" --capacity "$CAPACITY" protect \
  --height "$COMMITTED_HEIGHT" --hash "$COMMITTED_HASH"
"$NODE" --db "$DB" --capacity "$CAPACITY" --guardian-id "$GUARDIAN_ID" bind-guardian \
  --committed-height "$COMMITTED_HEIGHT" --committed-hash "$COMMITTED_HASH" \
  --oldest-needed "$OLDEST_NEEDED"

# On later administrative protection, the same identity is mandatory.
"$NODE" --db "$DB" --capacity "$CAPACITY" --guardian-id "$GUARDIAN_ID" protect \
  --height "$COMMITTED_HEIGHT" --hash "$COMMITTED_HASH"

# Bound startup checks identity, already-protected ancestry and needed range
# before opening the listener. Supply persisted capacity on every command.
"$NODE" --db "$DB" --capacity "$CAPACITY" --guardian-id "$GUARDIAN_ID" serve \
  --committed-height "$COMMITTED_HEIGHT" --committed-hash "$COMMITTED_HASH" \
  --oldest-needed "$OLDEST_NEEDED" --rpc-credentials /run/credentials/rpc.auth \
  --fake-bitcoind
```

The floor can safely exceed the guardian watermark after an interrupted
coordination step, at the cost of rejecting more reorganizations. Hash-verified
retained ancestors are recognized as already protected without reducing the
floor. Once pruned, only the exact recorded protected checkpoint can prove a
committed hash; arbitrary older hashes are not accepted just because their
heights are smaller. Range errors are request-local, not global recovery latches.
Ordinary reads never update protection, binding or guardian state.

`serve --standalone` is explicitly unbound smoke/demo operation and refuses
bound state. `serve` without that flag requires all guardian arguments.
`status` remains available offline without knowing the identifier, but reports
it rather than claiming guardian readiness. Binding cannot be removed or changed.

### The missing caller-side transaction boundary

For automatic safety a guardian-aware caller must check selected height/hash and
durably protect that ancestry **before** committing its irreversible consumption
to the guardian database. If protection succeeds but the guardian commit fails,
keep the excess protection and retry safely. If protection fails, do not commit.
It must also stop on permanent history/recovery errors rather than retry forever.
This needs a coordinated caller API and authorization for separate Fedimint work;
adding an HTTP method alone cannot implement that database transaction ordering.

The offline procedure above reconciles an operator checkpoint only. Stopping both
processes, protecting, and restarting does **not** protect every future guardian
commit. Do not bypass this limitation with periodic protection or automatic
read-triggered acknowledgement. There is no deployment certification here.

## Service lifecycle and diagnostics

`misc/fedimint-light-node.service` is an **uninstalled example**, not deployment.
Provision its dedicated user, initialized/bound state, private operator environment
and credential file explicitly. Update guardian evidence while stopped.
Credentials are a mode-0600 regular `username:password` file; systemd's credential
copy keeps the password out of process arguments and logs. Rotate it while both
service and caller are stopped, then restart with matching caller credentials.
No public listener, forwarding or remote administration is supported.

The service never bootstraps missing state, retries permanent startup failures or
automatically restarts after a fatal local failure. It leaves state intact for
inspection. SIGTERM cancels asynchronous work and joins owned connections;
runtime teardown waits at most two seconds for resolver jobs. Synchronous
validation/database work still cannot be interrupted. The example's stop timeout
can kill the process as an external backstop; inspect/reopen after forced exit.
The embedding library retains caller-owned runtime and OS DNS limitations.

Native `sync:` logs include observation time, selected height/hash, process-local
generation, live connections, candidate count, queue depths, completed/canceled
RPC totals, last RPC outcome code/time, last successful visit time and last
selection-change time. These counters reset on restart, saturate instead of
wrapping and use bounded fields without transaction bodies or arbitrary method
labels. RPC totals cover only requests observed by the chain owner: completed
dispatches and canceled queued/active work. Authentication, parsing and queue
admission failures are excluded; these are not all HTTP traffic totals.
They are not a durable audit log. A successful empty visit does not prove
freshness. Low connection count and repeated parser/transport rejections deserve
investigation; do not relax peer parsing to hide them. `stopped=true` or a fatal
storage error requires operator intervention. Synthetic Core progress is never
health evidence.

## Stopped-store backup

Stop the guardian, record its current durable watermark, and stop the node.
Reconcile binding, anchor, floor and range first. Keep all paths private and on
trusted local filesystems. Use the original capacity on every invocation.

```sh
umask 077
"$NODE" --db "$DB" --capacity "$CAPACITY" --guardian-id "$GUARDIAN_ID" checkpoint \
  --output /safe/current-state.json
"$NODE" --db "$DB" --capacity "$CAPACITY" --guardian-id "$GUARDIAN_ID" backup \
  --output /backup/chain.snapshot.redb --manifest /backup/chain.manifest.json
```

`checkpoint` exports the exact local safety state, not guardian state. Retain it
independently, associated with the guardian's current durable watermark and
monotonic protection record. Replace your independent record through your
controlled process after every state/protection change; these commands never
overwrite files. A stale record is not evidence that an old backup is safe.

Backup requires exclusive database ownership and copies logical records from one
quiescent transaction into private staging, not naive live redb file bytes.
It reopens and verifies the result, syncs it, publishes without overwrite and
publishes the SHA-256 manifest last. The manifest contains format versions,
capacity, network identity, anchor, retained range, selected tip, protected
height/hash and binding. It excludes credentials, peer cache, runtime fee policy
and guardian database contents. Back up deployment configuration and guardian
state separately under their own safe procedures.

An interrupted operation can leave private staging or a complete snapshot
without its manifest. Never treat that as a complete backup. Publication or
directory-sync errors can leave complete destinations: inspect; do not delete
or retry over them automatically. A digest detects damaged bytes, not malicious
replacement, authenticity or recency.

## Conservative restore and upgrades

Keep the damaged/original store untouched. Restore into a **new** path:

```sh
"$NODE" --db /var/lib/fedimint-light-node/restored.redb --capacity "$CAPACITY" \
  --guardian-id "$GUARDIAN_ID" restore \
  --backup /backup/chain.snapshot.redb --manifest /backup/chain.manifest.json \
  --expected-state /independent/current-state.json
```

The expected state must be independently current, reconciled with the guardian
and monotonic protection record, **not copied from the candidate backup**.
Restore demands exact safety-state equality, including tip/range as well as
anchor, capacity, binding and protected hash. This deliberately refuses many
older backups even where a more elaborate recovery might be possible. It never
reduces the floor, deletes state, reanchors or performs archival recovery.
It verifies digest before opening a private copy, checks schema/config and all
recorded state, then publishes without overwrite. Original backup bytes remain
unchanged. Disk-space budgeting and trusted-directory ownership remain operator
responsibilities.

Before selecting the restored path, run `status`, perform bound startup
preflight against the guardian's **current** evidence, and obtain operator
approval. If state is incompatible, stop and coordinate recovery using another
backend or separately designed recovery procedure. Do not edit the manifest,
lower the floor, reset the guardian identifier or bootstrap over missing history.

The base store format remains schema 2. Binding or explicit protection adds an
independently versioned operational record; prior binaries reject the extra
metadata rather than silently dropping binding. Unknown versions are rejected.
There is no downgrade, resizing or automatic migration. Before upgrading, stop,
checkpoint and back up; test the new binary against a verified private restore
with identical configuration. A legacy non-anchor floor already pruned before
hash persistence cannot be invented during upgrade: reconcile using independent
evidence and explicit supported protection, or stop for recovery.
