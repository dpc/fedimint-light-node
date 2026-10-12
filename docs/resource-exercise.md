# Local release resource and failure exercise

This is a reproducible **library/test-process measurement**, not a production
memory limit or a complete guardian-backend sizing result. It exercises the
implemented bounded-history backend without public services. In particular,
**52,560 raw rows omit validation context**: do not quote their disk or RSS
numbers as the cost of a validated node with a year of history.

The new workload changes no production behavior. The existing scripted-peer
tests already cover failure handling; the runner reuses them instead of building
a second peer simulator. The four larger measurement tests are explicitly
ignored in normal CI and run by this command.

The maintained-pool regression now runs in normal CI: `cargo test --lib peer_pool`.
It fills all four socket slots, holds a foreground lease while servicing the other
connections, cycles leases through the 64-frame idle ceiling, and checks aggregate
budget rollover, bounded candidate populations, rotation and joined shutdown.
These are resource-accounting regressions, not new RSS measurements for the tables
below.

## Reproduce

From the repository root on Linux, with Python 3 and GNU `time` on `PATH`:

```console
nix develop -c python3 misc/resource-exercise.py 2>&1 | tee resource-exercise.log
```

Use `set -o pipefail` if the pipeline's exit status is used as a gate. If these
host utilities are absent, enter `nix shell nixpkgs#python3 nixpkgs#time` first.
The runner builds the selected integration tests with `cargo test --release
--locked --no-run`, reads Cargo's executable paths, then measures the executables
directly. Compilation, Cargo, Python and Nix are outside the measured processes.
Dependencies/toolchains may need fetching during setup; the **workloads** use
only checked-in fixtures, generated inputs, temporary files and loopback peers.

Each measurement test runs in a fresh process. Other suites run serially within
one fresh process per suite (`--test-threads=1`). GNU `time` reports wall/user/
system seconds and peak RSS in KiB. The four measurement tests additionally
print Linux `/proc/self/status` high-water RSS and current RSS before teardown.
These kernel interfaces can report different RSS values; retain both rather
than treating either as exact allocation accounting. All numbers include the
test harness, fixture construction, allocator overhead and database inspection.
They exclude the host page cache and other processes.

Database measurements occur after closing the library store and before
reopening it. `file_bytes` is apparent length; `allocated_bytes` is Unix
`st_blocks * 512`, not logical live bytes or guaranteed exclusive device usage.
Read-only inspection checks actual table counts and sums encoded value lengths;
it does not count keys, tree pages, metadata or free pages as record bytes.
Inspection also uses a 1 MiB redb cache. No compaction is requested.
Temporary directories are removed after each test.

## Workloads and interpretation

* **Raw rollover:** 52,560-row capacity (the CLI's default retention), explicit
  **1 MiB cache** (not the CLI's 8 MiB default), 144 linked but unmined headers
  per durable transaction. Four rounds of 365 transactions append 210,240
  headers: fill once, then replace the full window three times. Each round
  checks the expired query, physical row count and selected tip, closes and
  reopens the database. This is synthetic raw-store sizing only. It measures
  batched updates, not 210,240 individual durable commits.
* **Validated rollover:** actual mainnet headers 0–2,035, including the first
  retarget; 512-row capacity, 1 MiB cache, batches of at most 144 descendants.
  All 15 batches close, inspect and reopen the database; both header and context
  row counts are checked. Genesis is explicitly trusted and wall-clock freshness
  is disabled with `u64::MAX`; descendants undergo the real contextual validator.
  The final live values are 84 bytes/header plus 168 bytes/context. This small,
  early-chain fixture is **not** a measured 52,560-context database; record widths,
  tree overhead and work encodings must not be extrapolated as a disk/RSS bound.
* **Byte-heavy body:** generate one mechanically acceptable, unmined legacy
  body with a 990,000-byte output script, serialize it, drop the original,
  decode/check/drop it 100 times. Payload: 990,222 bytes. This stresses large
  byte copies near the stripped-size limit, not script validity or PoW.
* **Element-heavy body:** likewise generate 100,000 zero-value, empty-script
  outputs in one coinbase; payload 900,213 bytes. This contrasts decoded object
  overhead with wire size. It is an extreme synthetic body, not observed
  traffic. Both body exercises use a 4,000,000-byte/200,000-element admission
  budget, retain the wire payload and at most one returned block, and have no
  transport or database open. Neither is a worst-case 4 MB witness body,
  transaction/input/witness allocation study, or an end-to-end large P2P fetch.

## Recorded run

Measured October 12, 2026, on the implementation following `9bac4e65`.
Environment: x86-64 NixOS, Linux 7.1.8, Ryzen 9 7950X3D (32 logical CPUs),
about 62 GiB host RAM, bcachefs for both checkout and `/tmp`.
Rust 1.98.1 (from `nix develop`); repository `Cargo.lock` and `flake.lock`; default Cargo release
profile. This was a shared, already-warm development machine, not a controlled
cold-cache benchmark or constrained-memory VM. Swap was enabled; no process
memory limit was imposed. Run-to-run timings are not service latency guarantees.

<!-- Measured tables below are deliberately workload-specific. -->

### Persistence

| Raw round | Tip | Rows | Apparent bytes | Allocated bytes | Round ms | Reopen ms |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Fill | 52,560 | 52,560 | 10,465,280 | 9,998,848 | 1,629 | 16 |
| Rollover 1 | 105,120 | 52,560 | 20,873,216 | 12,481,024 | 3,690 | 13 |
| Rollover 2 | 157,680 | 52,560 | 20,869,120 | 12,476,928 | 3,813 | 13 |
| Rollover 3 | 210,240 | 52,560 | 20,865,024 | 12,472,832 | 3,879 | 15 |

Raw encoded header values remained 4,415,040 bytes at each snapshot, with no
context values. Physical allocation rose after the first window despite a fixed
row count. The later snapshots leveled off in this run; four snapshots cannot
establish a universal physical-file ceiling or absence of long-term growth.

For the 512-row validated exercise, the final live values totaled 43,008 header
bytes and 86,016 context bytes. Final apparent/allocated sizes were
1,568,768/1,565,184 bytes. The largest sampled apparent/allocated sizes were
3,133,440/1,688,064 bytes. Reopen took 1–3 ms. These are snapshot maxima, not
continuous measurements of transient files/pages during transactions.

### Process resources

The runner's complete wall time includes creation, checking and cleanup;
in-test timers exclude some setup/teardown. Values will vary with scheduling,
filesystem and allocator state. RSS does not add linearly across these workloads.

| Isolated workload | Wall seconds | GNU peak RSS KiB | In-test VmHWM KiB |
| --- | ---: | ---: | ---: |
| Raw 52,560-row rollover | 13.06 | 4,892 | 5,696 |
| Validated 512-row rollover | 0.32 | 4,636 | 5,248 |
| 100 byte-heavy decodes | 0.05 | 5,660 | 5,720 |
| 100 element-heavy decodes | 0.30 | 9,188 | 9,188 |

The decode-only timers were 47 ms and 297 ms respectively. Small wire size
does not imply equal decoded memory: the element-heavy case used more RSS than
the larger byte-heavy payload. These observations are not a minimum supported
RAM specification.

## Failure evidence and remaining gaps

The runner also runs the existing release suites:

* `header_sync`: real-header progress/pruning/restart, withholding-peer timeout
  followed by progress from another peer, hostile frames/invalid headers with
  failover, cancellation during handshake/request with socket closure, global
  byte accounting, and actual divergent mined branches/protection.
* `block_download`: retained-body delivery, wrong-hash failover, unavailable
  bodies, required full-history/witness service bits, absolute deadlines,
  cancellation releasing the download slot, malformed/partial envelopes across
  peer failover, failed-peer aggregate accounting and permanent range errors.
* `block_decode`: real legacy and witness fixtures, exact byte/element budgets,
  malformed/truncated encodings and integrity failures.
* `block_decode_allocations`: malformed preflight paths allocate nothing under
  the existing counting allocator (not a whole-process allocation guarantee).
* `validated_header_store`: real divergent fork selection, protection, repeated
  capacity-one pruning/restart through a retarget, incompatible modes and damaged
  persisted state. These are logical-corruption tests, not power-loss simulation.
* `cli`: existing subprocess tests exercise the release executable, bootstrap
  overwrite refusal, local sync/body export, offline status/protection and
  process signals.

| Release suite | Passed tests | Wall seconds | GNU peak RSS KiB |
| --- | ---: | ---: | ---: |
| `header_sync` | 8 | 2.05 | 5,696 |
| `block_download` | 10 | 1.11 | 5,536 |
| `block_decode` | 8 | 0.00 | 3,104 |
| `block_decode_allocations` | 1 | 0.00 | 3,104 |
| `validated_header_store` | 6 | 1.00 | 4,688 |
| `cli` | 9 | 5.48 | 6,628 |

Zero seconds here means below GNU `time`'s displayed precision. The CLI row
includes its test driver and child-process accounting, not an isolated steady
node. Suite peaks cannot be attributed to a particular individual test.

All these assertions passed in the recorded release exercise. They test explicit
local outcomes, not the behavior of unknown Internet peers. There is no combined
default-retention validated-node plus maximum-body RSS measurement, modern
52,560-header fixture, HTTPS resolver/TLS memory measurement, disk-full test,
power-cut campaign, long-duration soak, constrained-RAM/OOM test, or guardian
integration here. Local scripted timeouts and signal tests do not establish
hard shutdown deadlines for synchronous work or OS DNS resolution; see
[SECURITY.md](../SECURITY.md).

The observed rows remained bounded and the exercised failures preserved the
existing contracts. No production resource-limit fix was required by this run.
The report does not change the incomplete resource-accounting status in
[REQ-bounded-history](../specs/REQ-bounded-history.md), the trust boundary in
[SPEC-chain-trust](../specs/SPEC-chain-trust.md), or the unimplemented guardian
adapter in [REQ-guardian-backend](../specs/REQ-guardian-backend.md).
