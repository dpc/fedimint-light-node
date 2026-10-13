# Readiness evidence and remaining gates

**These bounded experiments found and fixed a real peer interoperability defect.
They do not certify production readiness.** Default-scale storage, composed body
memory pressure, process crashes, actual isolated ENOSPC, allocation failure and
short and four-CPU-hour-per-target coverage-guided fuzz campaigns now have
reproducible evidence. Later campaigns measured real four-peer/RPC pressure and
cancellation, and finite public-network availability with three body failures.
The attempted 24-hour soak stopped at its shared traffic budget, not a duration
PASS. Automatic
guardian commit protection remains absent; the [operations limitations](operations.md)
still apply. Full Bitcoin consensus remains outside this library's contract.

## What changed in peer availability

The earlier mainnet run advertised protocol 70012. Bitcoin Core v30.0 sends its
final legacy alert at negotiated versions at or below 70012
(`src/net_processing.cpp`, final-alert branch). The strict parser rejects `alert`
before allocating its payload. Thus ordinary Core peers could fail exploration,
followed by the pool correctly falling back to its one maintained peer. Candidate
count did not mean eligible or reachable peers. Retry/warmup success was not proof
that the original failures were merely transient.

Advertising **70013** avoids that trigger. BIP133 fee filtering is optional, and
the parser already bounds and ignores those messages. Alert admission, unknown
command rejection, connection cap, lease cleanup, body bounds, useful-service
promotion, source/prefix diversity and backoff did not change. A scripted Core
threshold regression checks the outbound version, fee-filter handling and a
subsequent header response. A second test sends only a huge alert envelope and
checks rejection without waiting for payload.

Primary protocol references:

```text
https://github.com/bitcoin/bitcoin/blob/v30.0/src/net_processing.cpp
https://github.com/bitcoin/bips/blob/master/bip-0133.mediawiki
```

## Host and measurement boundaries

Recorded October 13, 2026 UTC on Linux/NixOS, AMD Ryzen 9 7950X3D, approximately
62 GiB physical RAM; Rust 1.98.1. No 512 MiB/2 GiB/two-CPU deployment envelope was
imposed or demonstrated. Cgroup controller files were unavailable in this
environment. Linux `/proc` and GNU `time` report process observations, not a
library heap limit or page-cache budget. Their RSS readings can differ.

Release CLI SHA-256 used by the live experiments:

```text
7b9c2448022453ea1f1585fb21f86faa2ef10df0e7ed197da1da46bc94b27391
```

Build and experiment artifacts are deliberately not dependencies of normal CI.
The large public header capture is not committed. The small Core body fixtures
and their provenance are committed under `tests/data/core-shared/`.

## Default-scale retained state and body pressure — PASS

The [header manifest](readiness-headers.json) pins 159,696 recent mainnet headers
at heights 810432–970127, captured over certificate-checked TLS Electrum with
25,556,949 application reply bytes. The revised capture completed in 90 seconds
under a separate 300-second process watchdog, with per-reply and aggregate
allocation bounds. The fixture is evidence, never new production anchor authority.
The initial capture preceded the watchdog fix; its identical digest alone is not
evidence that the old tool enforced a deadline against trickling responses.

The replay explicitly trusts context 810432–812447, then contextually validates
157,680 descendants. It fills the 52,560-row window and replaces it completely
twice, with **8 MiB redb cache**, 144-header transactions and all 52,560 context
records retained. Each cycle closes, inspects, reopens and checks pruning/tip.
Wall-clock freshness is disabled for replay; linkage, expected targets, PoW and
the implemented contextual rules are not bypassed.

| Observation | Fill | First replacement | Second replacement |
|---|---:|---:|---:|
| Selected height | 865007 | 917567 | 970127 |
| Header/context rows each | 52560 | 52560 | 52560 |
| Apparent database bytes | 40898560 | 40898560 | 40894464 |
| Allocated filesystem bytes | 28258816 | 30409216 | 30405120 |
| Current RSS KiB, before composed body phase | 25088 | 25696 | 25696 |

Encoded live values were 4,415,040 header bytes plus 8,830,080 context bytes.
Database inspection uses a separate 1 MiB cache after closing the measured store,
as in the older exercise; replay and reopening use 8 MiB. The full capture and
decoded fixture vector contribute to the test-process RSS. Do not quote this as
the minimal resident footprint of a running node.

With that default validated store still open, the test admits two synthetic
witness shapes and holds eight serialized JSON response copies:

| Shape | Body bytes | Weight | Eight response bytes | `/proc` high-water RSS KiB |
|---|---:|---:|---:|---:|
| One large witness item | 3990580 | 3992188 | 63849544 | 123052 |
| 999981 witness items | 3990584 | 3992192 | 63849608 | 133772 |

The second shape approaches the one-million-element admission limit; both
approach four-million weight units. They pass mechanical body checks with a
real matching witness commitment, but are not mined, script-valid chain blocks.
This is a **composed allocation exercise**, not eight actual blocked RPC clients,
a maintained four-peer pool, or a maximum-RSS proof. Full simultaneous default
state + four peers + adversarial body + actual RPC saturation remains a gap.
GNU `time` reported 132,032 KiB peak, 16.63 seconds wall, 8.57 user and 4.60 system
for the whole replay/composition process.

Reproduce:

```sh
mkdir -p target/readiness
nix develop -c python3 misc/capture-readiness-headers.py \
  target/readiness/recent.raw --start 810432
# Inspect the appended recent.raw.json; compare its SHA-256 with the committed manifest.
nix develop -c python3 misc/resource-exercise.py --headers target/readiness/recent.raw
```

The capture refuses existing output and manifest paths. A timeout can leave a
partial capture without a manifest; inspect and deliberately remove only your
disposable artifact before retrying. The resource runner checks the supplied
manifest digest. That checks integrity, not independent source truth.

## Fault campaigns — bounded PASS, not exhaustive recovery

`tests/readiness_faults.rs` uses disposable stores and real fixture headers:

* **32 seeded SIGKILL schedules:** delays of 1–100 ms plus four longer schedules
  that reach pruning. The child reports an acknowledgment after each successful
  durable selection. Reopening must yield the last reported commit or its single
  in-flight successor, with every retained row matching the known chain and
  identity/protection unchanged. The recorded longer schedules reached tips
  829, 1177, 814 and 992. This is process-kill evidence, not power-loss testing.
* **18 advanced guardian-bound protection SIGKILL schedules:** the normal test
  suite acknowledges public `protect_through` at height600 only after successful
  durable return, then selects through1200, pruning that checkpoint's header row
  (retained689–1200). One control kills before advancement to900, one after its
  acknowledgment, and sixteen schedules race that public call with0–5ms delays.
  Reopen checks exact binding, anchor, retained headers, checkpoint height/hash,
  no floor below any acknowledgment, and bound preflight with positive and
  wrong-identity/wrong-hash cases. Unacknowledged900 may persist coherently as600
  or900; refusal, lost metadata or a torn height/hash fails. A focused run passed
  all18 schedules in3.38seconds and observed both old/new unacknowledged outcomes.
  These are process-crash observations, not guaranteed internal transaction
  interruption points, power-loss guarantees or guardian-database commit coordination.
* **Duplicate writer and read-only file:** ownership rejects a second writer;
  mode-0400 rejects writer open, then normal permissions permit unchanged reopen.
  This test requires an unprivileged account, like the supported service;
  root/CAP_DAC_OVERRIDE is not a valid permission-test environment.
* **Actual byte and inode ENOSPC:** separate unprivileged user/mount namespaces,
  private 8 MiB tmpfs with 64 inodes, no host filesystem exhaustion. Byte filling
  forced a store-update error; inode filling refused a new store. Removing only
  disposable fillers allowed exact old-chain reopen (tip zero) in both runs.
  The test contract also permits explicit reopen refusal, never automatic reset.
  These two successful reopens do not establish universal recovery.
* **Allocation abort:** a child under 64 MiB `RLIMIT_AS` acknowledges one commit,
  then requests 128 MiB and aborts with SIGABRT. Reopen preserves that commit and
  floor. This is allocator/address-space failure, **not a kernel cgroup OOM kill**.

Run the normal permission/ownership tests and selected finite crash campaigns:

```sh
nix develop -c cargo test --release --test readiness_faults
nix develop -c cargo test --release --test readiness_faults -- \
  --ignored --exact seeded_kill_reopen_keeps_committed_chain --nocapture
nix develop -c cargo test --release --test readiness_faults -- \
  --ignored --exact address_limit_abort_preserves_acknowledged_commit --nocapture
```

For ENOSPC, first compile the test executable with `cargo test --release --test
readiness_faults --no-run --message-format=json`; use its `compiler-artifact`
executable path as `TEST_BINARY`. Run **only in a private disposable mount**:

```sh
MOUNT=$(mktemp -d "$PWD/target/readiness/private-tmpfs-XXXXXX")
# MODE=bytes, then repeat with MODE=inodes. Never fill a host filesystem.
MODE=bytes
unshare --user --map-root-user --mount --propagation private sh -eu -c '
  mount -t tmpfs -o size=8m,nr_inodes=64 tmpfs "$1"
  READINESS_FAULT_DIR="$1" READINESS_FAULT_MODE="$2" \
    "$3" --ignored --exact constrained_filesystem_failure --nocapture
' sh "$MOUNT" "$MODE" "$TEST_BINARY"
rmdir "$MOUNT" # The private mount disappeared with its namespace.
```

Each filler itself is capped at 16 MiB or 256 files if an operator forgets the
mount; that is a safety fallback, not permission to run against valuable state.

### Running-owner storage exhaustion

The ignored Linux integration test
`owner_enospc::actual_enospc_stops_owner_closes_clients_and_reopens_safely`
in the `guardian_rpc` test executable composes actual byte ENOSPC with the
running `HeaderSync`/`GuardianRpc` owner. It uses checked-in mainnet headers,
one loopback scripted peer, a new disposable guardian-bound store and an
acknowledged height-one protection checkpoint. Before releasing the peer's
header reply it connects cheap-read, body-read and partial-HTTP clients, then
fills only the explicitly checked private 8 MiB tmpfs (16 MiB hard filler cap).

Finite local execution passed: the real durable selection returned
`Header(Store(Storage(Io(... code: 28 ...))))`; the observer recorded the
terminal latch, both queued reads and zero completed dispatches. The service
returned a recovery-required error, joined its owner task, closed the peer and
HTTP clients and stopped listening within the test deadlines. No successful
JSON result reached the waiting clients. This sync-origin error has no owning
RPC request, so the test does not require a particular RPC error code.
After releasing filler space, reopen retained the exact selected range
`0..=2`, anchor, guardian identity and acknowledged height/hash protection.
The regression permits only coherent atomic selected prefixes with those
safety records intact, or an explicit reopen refusal without deleting or
replacing the store. This is finite failure-propagation evidence, not a
power-loss test, a guardian commit protocol or a production-readiness verdict.

Compile `cargo test --release --test guardian_rpc --no-run --message-format=json`
and use its executable path as `TEST_BINARY`. Run without privilege escalation:

```sh
MOUNT=$(mktemp -d "$PWD/target/readiness/private-tmpfs-XXXXXX")
READINESS_PARENT_MOUNT_NS=$(readlink /proc/self/ns/mnt) \
unshare --user --map-root-user --mount --propagation private sh -eu -c '
  mount -t tmpfs -o size=8m,nr_inodes=64 tmpfs "$1"
  READINESS_FAULT_DIR="$1" "$2" --ignored --exact \
    owner_enospc::actual_enospc_stops_owner_closes_clients_and_reopens_safely \
    --nocapture
' sh "$MOUNT" "$TEST_BINARY"
rmdir "$MOUNT"
```

The test refuses the parent mount namespace and any path that is not the exact
8 MiB tmpfs mount point. If unprivileged namespaces are unavailable, leave this
environment-dependent test ignored; do not substitute a host filesystem or a
privileged mount.

Existing normal tests retain corrupt/truncated metadata, interrupted publication,
network truncation/cancellation and validation-clock coverage. This campaign did
not reproduce every power-loss ordering, every corruption location, real kernel
OOM, or OS clock jumps.

## Injected clock correction — PASS within scope

Offline loopback regressions supply backward and forward wall-clock snapshots to
the existing `HeaderSync::step(now)` and bootstrap APIs. They do not modify the
host clock. This exercise exposed a concrete retry defect: three future-time
rejections disabled a never-useful discovered candidate even after clock
correction. Header synchronization now removes only the current attempt's
provisional failure strike for `FutureTime`, while discarding its connection and
retaining monotonic backoff, prior penalties and exploratory fallback. It neither
promotes the peer nor relaxes header validation.

The owner-level regression rejects the same discovered candidate four times,
checks that an immediate forward correction cannot bypass retry pacing, then
selects its header after simulated monotonic backoff. It reopens the persisted
store, rejects the next header under a backward clock, and accepts it after
correction without a recovery latch or changed protection floor. Bootstrap
alternates future/stale rejection at the same destination before successfully
publishing; offline reopen preserves the pinned anchor after sources disappear.
Additional checks cover prior failure counts (including saturation), absence of
useful credit, and an invalid-PoW header still failing after clock correction.
Selected state continues to report no synchronization/freshness percentage.

Separate loopback visits stall the handshake or header request with supplied
clock values `0`, `u64::MAX`, then `0`; each still expires at its monotonic
exchange deadline. Tokio time is advanced only between completed exchanges to
avoid waiting through peer backoff. Dial pacing, request deadlines, idle budgets,
and rotation use Tokio monotonic instants, not the supplied Unix timestamp;
existing pool tests cover idle and rotation behavior. These are API-level
clock-input regressions, not OS clock-jump, suspend/resume, time synchronization,
power-loss, or global chain-freshness guarantees.

Reproduce with:

```sh
nix develop -c cargo test --lib clock
nix develop -c cargo test --lib time_rejection
```

## Coverage-guided fuzzing and independent shared checks — PASS within scope

The isolated [fuzz workspace](../fuzz/README.md) adds block and transaction
targets plus deterministic fixture seed generation. `cargo-fuzz 0.13.1` with
libFuzzer coverage counters and `--sanitizer none` ran finite 61-second campaigns:

| Target | Executions | Final coverage counters / features | Peak reported RSS |
|---|---:|---:|---:|
| Block, including short headers | 4020620 | 534 / 1073 | 65 MiB |
| Transaction | 5634194 | 357 / 1348 | 65 MiB |

No crash was found. The block rerun reused its earlier evolved corpus; execution
counts are observations, not a deterministic performance guarantee. An initial
block campaign ran 3,707,074 iterations before the short-header target improvement.
Seeds were 604870434 and 3402604921. Limits were 512 MiB libFuzzer RSS, five seconds
per input, and maximum inputs of 4,000,000/400,000 bytes. These are short campaigns,
not four CPU-hours per target or exhaustive coverage.

A separate block campaign with AddressSanitizer enabled completed 1,425,882
executions in 61 seconds with no crash (837 coverage counters, 1,744 features,
378 MiB peak reported RSS). It used seed 604870434 and the same input, timeout and
RSS limits. This short sanitizer run does not cover the transaction target or
replace the longer campaigns.

Separately, Core v31.1.0 generated a tiny regtest witness block with all P2P
networking disabled. A second fresh Core instance rejected changed Merkle and
witness data with `bad-txnmrklroot` and `bad-witness-merkle-match`, then accepted
the original. The checked-in fixtures let normal CI verify that this library
agrees on those **shared body rules only**. See `tests/data/README.md` and:

```sh
nix develop -c python3 misc/core-shared-fixtures.py \
  --bitcoind /path/to/bitcoind --output target/readiness/new-core-fixtures
nix develop -c cargo test --test core_shared_fixtures
```

The generator has a 120-second owned-process watchdog, uses private regtest state
and loopback RPC, and stops children before deleting their state. Regtest
generation is not a submitted live transaction. This does not provide an
independent synchronized mainnet Core reference.

## Read-only mainnet smoke — mixed outcomes

A fresh **default 52,560-header/8 MiB** store bootstrapped at height 971157 in
9.90 seconds (GNU `time` peak 19,256 KiB). Unlike the replay, bootstrap authenticates
historical rows into its checkpoint; it does not retroactively contextually
validate every retained historical row.

A finite 600-second outbound-only run reached **four maintained connections**,
advanced selected height to 971165, completed ten matching-height external
reference samples and one 1,577,384-byte RPC block, and recovered after one TERM
and one SIGKILL restart. Checkpoint and protection remained 971157. There were no
recorded RPC sample failures. The same namespace's non-loopback RX+TX delta was
56,028,882 bytes; that includes unrelated traffic and possible double counting.
Peak sampled RSS was 20,944 KiB, VmHWM 26,020 KiB, and open FDs 16.

That first smoke preceded the independent-monitor fix. A subsequent 180-second
run of the fixed monitor also passed: four peers, three references, one body,
two restarts, zero sample failures, sampled RSS 22,472 KiB, VmHWM 26,936 KiB, FDs
16 and namespace traffic 103,858,696 bytes (including concurrent build downloads).
These observations do not establish a 24-hour or 72-hour soak.

The next 180-second run, using the added lag and connection-count telemetry,
**failed** its evidence gate: it reached four peers and obtained two matching-height
hashes and one zero-lag observation, but its only scheduled body request returned
a retryable unavailable/deadline error. It completed two successful samples and
two restarts; the failed sample left zero successful bodies. The next body attempt
was scheduled ten minutes later, beyond this run's duration. This is a recorded
availability failure, not a pass inferred from the earlier runs.

A separate **720-second telemetry run passed** with the same binary and safety
limits: twelve successful samples, six matching-height hash references, six
zero-height-lag observations, two 1,479,174-byte body responses, four maintained
peers and two restarts (TERM then SIGKILL). There were no sample failures.
Selected height remained 971166; checkpoint/protection remained 971157, so this
run demonstrates continued service and restart recovery, not new advancement.
The first post-restart samples succeeded approximately 30 and 31 seconds after
launch. Sampled RSS peaked at 21,644 KiB, VmHWM at 26,500 KiB, FDs at 16 and
sockets at nine. Namespace traffic was 63,054,377 bytes, including other host
activity. Builds and a local CI run overlapped part of this experiment.
The longer observation does not erase the preceding failed body sample.

`mempool.space` matching-height hashes are external observations, not an
independent full-validator audit; the operator also participates in default
bootstrap. Native block fetching uses the same library and is not independent
validation either. No live transactions or public listeners were used.

## Soak supervision and still-open gates

The finite supervisor reads a prebootstrapped **disposable, unbound** default
store. Credentials are created exclusively, mode 0600, and removed after normal
completion. It binds RPC to loopback on an ephemeral port, makes at most one
external hash request per minute and one block request per ten minutes, and
alternates TERM/SIGKILL restarts at the configured interval. Source, binary and
script digests, samples, final identity, CPU/RSS/FD/socket/disk observations and
outcome go to local reports. It never submits transactions or reanchors.

```sh
# D is a NEW private directory with its own newly bootstrapped chain.redb.
nix develop -c python3 misc/readiness-soak.py \
  --binary target/release/fedimint-light-node --directory "$D" \
  --seconds 86400 --byte-stop 2147483648 --restart-seconds 21600
```

Use a new report directory/store for each run; do not delete existing reports to
hide earlier failures. A 72-hour command uses `--seconds 259200` and an explicitly
chosen total byte stop (at most 6 GiB); the script does not enforce a rolling
per-day quota. Run it as a foreground, harness-managed command, never detached.

The separate monitor samples every 250 ms and kills only the owned node on
traffic, RSS (1 GiB), FD (256), disk (2 GiB), queue or monitoring errors. It remains
active if an external HTTP response stalls. Namespace counters are conservative
and can stop a run because of other programs. These are **sampled stop thresholds
with overshoot**, not hard packet/memory/disk quotas; scheduler delays and in-flight
traffic can exceed a threshold before termination. The outer watchdog kills the
owned process group at duration plus 60 seconds, including blocked resolver work.
It can leave an incomplete report or credential file; such a run is NOT PASS.
Healthy sync logs are sampled once per minute, errors retained, with a 16 MiB cap.

## Adversarial witness exchange — finite regression PASS

The offline, non-ignored unit regression
`block_download::tests::adversarial_witness_transport_boundaries_and_fresh_exchange_recovery`
extends the earlier composed-allocation evidence to real loopback TCP block
exchanges. It runs the production `request_block`, frame admission, structural
preflight and mechanical body checks. No production acceptance path changes.

| Synthetic body | Body bytes | Weight | Aggregate elements |
|---|---:|---:|---:|
| One large non-coinbase witness item | 3990580 | 3992188 | 9 |
| 999991 empty items plus one large item | 3990584 | 3992192 | 1000000 |

Both include a separate coinbase reserved witness value and matching commitment.
The second body reaches the configured one-million-element ceiling exactly,
rather than merely approaching it. Each shape rejects a one-element-short
decode budget, a one-byte-short payload budget, a one-byte-short aggregate wire
budget, and a corrupt frame checksum with the expected error. For payload and
wire-budget rejection, the peer sends **only the envelope and remains connected**:
the client must reject without waiting for or receiving the body. Received-byte
reservation is checked at each boundary.

After each failure the test closes the rejected connection and performs a fresh
successful exchange at exact byte/element budgets, checking the complete
witness-preserving serialized response. This is fresh-exchange recovery, **not
pool retry or service-state recovery**. The focused debug run passed all eight
rejection/recovery pairs in 3.76 seconds. A separate release-binary run recorded
0.19 seconds wall time and 31,508 KiB process high-water RSS with GNU `time -v`;
the log is `target/readiness/adversarial-witness/release.log`, and the measured
binary SHA256 is
`e3aeba857ca564851f3285b48e7d1de658c2db4d68991b4ef95fa81b689088c4`.
This is one sequential run of both shapes, not isolated per-shape memory usage.

These synthetic headers are unmined. The test deliberately enters below
`HeaderSync`'s retained-mainnet-header/PoW boundary; it does not inject them into
the validated store or RPC service, relax header acceptance, or claim
script/consensus validity. This closes a lower-layer transport regression gap,
not the remaining combined default-store/four-peer/eight-HTTP extreme-shape
measurement gap. It supplies no RSS ceiling, allocation count, or deployment
guarantee. Fixture creation, peer buffers and returned bodies coexist in the
test process.

Reproduce without external fixtures or network access beyond loopback:

```sh
nix develop -c cargo test --lib adversarial_witness_transport -- --nocapture
```

## Actual transport pressure — PASS within reachable phases

The ignored Linux library test
`readiness_transport_tests::default_store_real_pool_and_rpc_pressure` uses the
same validated 52,560-row replay and configured 8 MiB cache. It transfers that
store into the real `HeaderSync` and `GuardianRpc`, with four loopback Bitcoin
peers and unmodified RPC deadlines and acceptance checks. Fixture P2P request
and handshake deadlines are explicitly one second, with long peer rotation.
It does not
inject RPC results, weaken PoW, or substitute a synthetic chain.

The [mined body manifest](readiness-block.json) pins height 934996: 3,833,822 bytes,
3,993,659 weight units, 347 transactions and 1,272 witness items. Remaining slack
is 166,178 payload bytes and 6,341 weight units. This is a large real body,
**not every worst-case admitted shape or the million-element limit**.

The October 13 recovery run at `target/readiness/transport-recovery` passed in
17.12 seconds, including replay; transport phases took 1,087 ms:

| Natural observation | Accepted HTTP tasks | Blocked writers | Cheap / network queued | P2P sockets | RSS KiB |
|---|---:|---:|---:|---:|---:|
| Warmed owner | 0 | 0 | 0 / 0 | 4 | 41516 |
| Eight successful body writers | 8 | 8 | 0 / 0 | 4 | 174892 |
| Active body decoded and checked | 8 | 2 | 4 / 1 | 4 | 71696 |
| All occupied clients canceled, cheap read recovered | 0 | 0 | 0 / 0 | 3 | 41820 |

The active-body phase includes three idle sockets and one leased socket, not a
fifth connection. Eight writers and full queues cannot coexist under the
eight-task cap; these are separate reachable phases. Test-only observations
record actual socket writes returning `Pending`, not merely unread clients.
Both full queues reject surplus admitted requests with `-32014`. Successful
responses drain to their exact expected bytes. Canceling eight occupied clients
released tasks, queues and writers in 2 ms; the closed active lease explains the
three remaining sockets. Shutdown joins owned tasks, then checks retained
header/context row counts and reopens the unchanged selected hash and protection floor.

Measurements include in-process fixture peers, one shared raw body, one expected
JSON response, replay setup, test runtime and observation overhead. They are not
library-only heap figures. The largest instrumented RSS sample was 174,892 KiB;
the independent sampler observed 159,912 KiB RSS and 174,728 KiB high-water RSS.
These separate Linux observations need not agree exactly. Peak sampled disk
was 40,899,667 bytes; phase observations counted 40 descriptors including the
observation directory, versus 39 in the external sampler. The runner pins two
CPUs and uses sampled resource stops, not a hard deployment quota.

The earlier `transport-first.log` failed an incorrect expected-height assertion,
which was fixed. `transport-second` passed transport checks but its top-level-only
disk sampler missed the nested temporary store. Its disk figure is invalid.
The recovery runner enables recursive disk sampling, with a sparse-file safety
regression; it does not overwrite either earlier result.

To reproduce, acquire the external header/body fixtures into private bounded
files, verify their manifests, build the release library test executable with
`cargo test --release --lib --no-run --message-format=json`, then run:

```sh
nix develop -c python3 misc/readiness-transport.py \
  --binary "$LIBTEST" --headers "$HEADERS" --block "$BLOCK" --directory "$NEW_REPORT"
```

## Managed body-failure recovery — finite offline regression

The normal test
`peer_pool::tests::hostile_body_leases_close_and_recover_within_attempt_policy`
now exercises the actual managed download path with checksum-valid witness
corruption, a truncated frame and a stalled partial frame. Each failure runs
with both one and two allowed attempts. One attempt returns the precise failure;
a following request succeeds through the other peer. Two attempts recover within
the original request. Failed sockets reach EOF, never earn useful-service credit
or enter an idle slot, and successful recovery clears lease/checkout ownership.
The configured pool cap is one; shutdown joins its remaining idle driver.
This small checked-in Bitcoin test network body deliberately exercises retrieval
**below mainnet header admission**, not a selected mainnet chain.

The ignored Linux test
`readiness_transport_tests::body_recovery::selected_body_failures_recover_through_managed_pool_and_rpc`
closes that admission gap using the same pinned mainnet header replay and
height-934996 body as the pressure test. It warms four real loopback peers,
then exercises all three failures through actual authenticated HTTP, GuardianRpc,
HeaderSync and the managed pool. Witness corruption changes only the coinbase
witness reserved value: the selected mined header, transaction Merkle root and
wire checksum still match, but the witness commitment does not.

With unmodified RPC budgets, one hostile response recovers on the second peer;
two hostile responses exhaust the two-attempt policy and return `-32012` without
body bytes. Cheap reads and a following body request then succeed, with the
original bytes supplied by a different eligible peer. The test observes hostile
socket EOF, a peak of four accepted P2P sockets, zero remaining HTTP tasks,
writers, queues or fixture sockets after joined shutdown, and unchanged selected
tip, retained bounds and an explicitly advanced protection checkpoint on reopen.
No retry, admission, timeout or protection policy changes are involved.

The October 13, 2026 finite run passed all three modes in 24.23 seconds including
replay (`target/readiness/managed-body-recovery-concurrent.log`). Fixture listeners
accept concurrent connections, so duplicate client sockets cannot hide behind
an existing connection in a serial accept loop. The first fixture
run exposed a test-observer reset omission between modes; that test-only issue was
fixed before the passing run. An earlier invocation used a nonexistent header
path and did not execute a scenario. These are regression observations, not a
long availability campaign, an Internet-latency result or a hard process-wide
resource guarantee. The near-limit million-element synthetic body remains
covered only below mainnet admission by the separate direct exchange tests.

To reproduce without a network connection:

```sh
READINESS_HEADERS="$HEADERS" READINESS_BLOCK="$BLOCK" \
  nix develop -c cargo test --release --lib \
  selected_body_failures_recover_through_managed_pool_and_rpc -- --ignored --nocapture
```

Both external fixture digests are checked before use. This regression is ignored
in ordinary CI because the large public-ledger fixtures are not checked in; the
small managed-pool regression runs in ordinary CI.

## Extended campaigns — terminal results within scope

A 24-hour soak began October 13, 2026 at 01:12:50 UTC, but was deliberately
interrupted after about 1,055 seconds to release an orchestration lock that
blocked unrelated source work. Preserve `target/readiness/soak-24h-SmrOTI`;
this was not a completed soak or a node-availability verdict.

That interruption exposed a supervisor defect: killing the outer managed command
could leave its separately grouped worker and node running. The owned group was
explicitly stopped. The corrected Linux wrapper arms parent-death termination
before ordinary executable replacement and checks the original spawning parent.
Every soak process edge uses it. Tests cover nested supervisor death, the
pre-arm parent mismatch, and actual managed-tool cancellation. This guarantee
requires long-lived spawning threads and trusted ordinary executables without
credential changes; it is not a general descendant-process sandbox.

The replacement started **October 13, 2026 at 01:36:18 UTC**. Its private external directory is
`/tmp/public/cg3s-soak-24h-tsxSc`, also recorded by
`target/readiness/soak-24h-restarted-path`. It uses the unchanged release binary
above, corrected pinned scripts, and a fresh copy of the stopped 720-second
store, not a new bootstrap. Its manifest records all digests. The existing
2 GiB namespace traffic stop, sampled RSS/FD/disk limits and six-hour alternating
restart policy remain unchanged. There is no five-minute disconnect injection.
The managed command used shell `exec` from an independent directory, so the
campaign did not hold the source repository's update lock.

Its terminal `soak.jsonl` record is **FAIL: namespace traffic counter/budget
safety stop**, after **19,780.376 seconds (5h29m40s)**. Sampled non-loopback RX+TX
reached 2,176,256,252 bytes against the 2,147,483,648-byte stop. This shared
namespace counter includes unrelated traffic; it is neither node-only usage nor
a hard quota. The stop does not demonstrate node resource exhaustion, but three
real body RPC failures prevent describing the observation as measurement-only.
No 24-hour/72-hour PASS or planned restart coverage was achieved: **zero
restarts**, before the first six-hour restart.

The finite observation recorded 325 successful samples, 164 matching-height
hash comparisons and 164 external tip observations (163 lag zero, one lag -1).
These asynchronous explorer checks are not independent full-consensus validation
or continuous freshness proof. Selected height advanced 971,166 → 971,205.
The monitor killed the child at the traffic stop; a separate stopped reopen
verified 52,560 rows, retained range 918,646..971,205, unchanged checkpoint and
protection at 971,157, and unbound guardian identity. That is reopen evidence,
not completion of the planned graceful/abrupt restart schedule.

Body retrieval succeeded in **30/33 scheduled attempts**, not 325/328. Failures at
elapsed 2,511.911, 3,113.281 and 7,942.498 seconds returned retryable RPC -32012,
unavailable/deadline. Later body observations succeeded without restart at
3,715.679 and 8,542.781 seconds. The script scheduled attempts 600 seconds apart
before making each call and followed the current tip; it did not promptly retry
the same failed hash. These results show service recovery, not measured recovery
latency, a production SLA or proof that every failed hash later became available.
The retained telemetry does not identify a unique root cause.

Peak sampled RSS was 27,948 KiB (VmHWM 32,308 KiB), with 16 descriptors, nine
sockets, 25,433,055 disposable-directory bytes and at most four managed
connections. No node resource-limit stop or retained corruption/fatal/store error
was reported; samples are not universal bounds. The binary remains source
**e0e4918e**, not the later clock-rejection fix or the final local-hardening source.
Further duration acceptance needs a feasible isolation/budget plan or an explicit
scoped acceptance decision; this result does not authorize a rerun or larger cap.

The new `misc/readiness-fuzz.py` runner executes block then transaction targets
sequentially. The threshold is 14,400 measured child CPU seconds each,
with 300-second wall-time chunks and a six-hour wall limit per target. CPU
accounting includes startup, corpus loading, sanitizer and system work; it is
not pure mutation time. A wall/resource failure is not a CPU-hours pass.
Corpora persist between chunks; each target uses one CPU at nice level 10,
512 MiB libFuzzer RSS, five seconds per input and the existing input-size limits.
An independent monitor and owned outer deadline bound the campaign.

The runner's short actual AddressSanitizer check passed both targets at a
one-CPU-second threshold (two-second chunks): block 67,058 executions and
3.059 child CPU seconds; transaction 90,837 executions and 3.042 child CPU
seconds. Both pinned binaries contain `__asan_init`. This verifies the runner,
not the planned long coverage. The long sequential campaign started October 13,
2026 at about 01:43 UTC in `/tmp/public/cg3s-fuzz-4cpuh-47Mgc`, recorded by
`target/readiness/fuzz-4cpuh-path`; its pinned snapshot includes binary, script
and initial corpus digests. That attempt **failed** after six block chunks:
each chunk exited successfully, but the monitor reported permission denied for
`/proc/3731266/fd`. The child used 1,799.104159 CPU seconds; five chunks contributed
24,562,329 accepted executions before the sixth chunk's 4,443,190 executions were
excluded by the failed monitoring gate. No crash artifacts were recorded.
The transaction target did not run. This is not four CPU hours of successful
coverage, and the failed attempt remains preserved in its original directory.

A local reproduction showed that Linux denies access to a zombie's descriptor
directory while Python's `poll()` can still return `None` when another waiter
holds its internal lock. This explains a possible exit-time race, not a proven
diagnosis of the original PID, whose live state was not captured. The monitor now
allows at most 100 ms to confirm termination of the exact sampled child after
specific process-file access errors. A live or uncertain unobservable child still
fails closed; disk and resource errors remain fatal even if the child exits.
Regressions exercise real zombie permissions with waiter contention, a live
non-dumpable child, and an independent disk error coinciding with exit.
Any replacement campaign requires a new pinned snapshot and fresh CPU accounting;
the failed attempt is never silently combined into a continuous pass.

The fresh retry started **October 13, 2026 at 02:26:31 UTC**, in
`/tmp/public/cg3s-fuzz-4cpuh-retry-iBGtgO`. Its durable `campaign.jsonl` ends with
**PASS, completed_targets=2**:

| Target | Successful executions | Successful chunks | User CPU seconds | System CPU seconds | Wall seconds |
| --- | ---: | ---: | ---: | ---: | ---: |
| block | 205,534,427 | 49 | 13,209.192928 | 1,481.661184 | 14,755.192 |
| transaction | 384,926,978 | 49 | 13,912.996369 | 771.828113 | 14,755.136 |

Each target exceeded four measured child CPU hours (about 4.08 hours). These are
49 restarted chunks per target, not continuous runs or four hours of pure mutation.
The retry copied the failed attempt's evolved corpus but carried over **no CPU
time or execution credit** from that attempt or the separate smoke check. The
first failed attempt above remains a failure.

`snapshot.txt`, `previous-binary-provenance.txt`, `campaign.jsonl` and all 98 chunk
logs pin the inputs and measurement. The reused AddressSanitizer binaries were built from
**87217c8e7ae11b43bc91c4cf70c9bfe48f619389**, not the latest tree;
the replacement script snapshot is from **27d30312**. Binary SHA-256 values:

```text
block       33f8646e54a824b2b89ad7053b2be9a37bbac7507809b5e86a189daeb4543050
transaction 61b195f99fd65ddee0bae13029225646331ba6635cabfaaf1da8da0d55b0cafd
```

An independent read-only assessment summed the chunk logs, checked the recorded
binary/script digests and AddressSanitizer symbols, and found no crash artifacts or sanitizer
diagnostic markers. This closes the specified duration gate for these pinned
targets, not a guarantee that sanitizer-detectable defects are absent or that
current-source networking, persistence and guardian integration were fuzzed.
The verdict relies on the durably flushed terminal records and chunk evidence, not an
independently consumed managed-shell exit status.

For reproduction only, prepare a **new** private directory with the pinned
`block`/`transaction` binaries, `corpus/{block,transaction}`, and the three scripts
listed in the snapshot. Never reuse or modify a completed campaign directory.
The recorded invocation was:

```sh
python3 "$NEW_DIRECTORY/readiness-fuzz.py" --directory "$NEW_DIRECTORY" \
  --cpu-seconds 14400 --chunk-seconds 300 --wall-seconds 21600
```

CPU affinity was `[0]`, nice level 10. Seed and input-size limits are in each
chunk's recorded command; the manifest and evolved corpus matter for reproducing
this scope. No new campaign is implied or authorized by this documentation.

Finite local hardening at **abdc8b12** is independently reviewed within the
intentional partial-validation contract. These terminal assessments do not
identify another finite local source task. Deployment/integration acceptance
remains pending, especially guardian approval and actual consumer commit
integration, sustained body availability, and supported deployment/reference
evidence.

**Still not completed:**

* Actual 24-hour and 72-hour completed soaks with sustained recovery/lag evidence.
* Independent, already synchronized mainnet Core comparison.
* Every worst admitted body shape under combined transport pressure; the measured
  real-body phases above cover a specific large fixture and reachable occupancy.
* Kernel/cgroup OOM, exhaustive storage/power-loss faults, OS clock-jump campaigns,
  broader sanitizer targets/current-source coverage and broader property schedules.
* Restricted-hardware deployment envelope and automatic guardian commit safety.

Report these as NOT RUN, incomplete or pending as appropriate, not implied passes
from the completed finite evidence.
The independent reviewer found and required fixes for trickle-resistant capture
deadlines and safety monitoring blocked behind reference I/O; both tools now have
offline regressions. Reviews and tests reduce risk, not the residual trust in
[SPEC-chain-trust](../specs/SPEC-chain-trust.md).
