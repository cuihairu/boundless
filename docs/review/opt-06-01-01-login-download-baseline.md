# OPT-06-01-01 — Login and resource download baseline

Acceptance: **save the peak, the rejection rate and the latency**.

`crates/server-runtime/src/login_baseline.rs` composes the real login faces
— the pre/post-version gates, the IP ban map, the rate limiter, the cached
request queue, the pending-ticket queue, the client login handler and the
session switch — plus the real server/client download faces into one seeded
flood. Every clock is the caller's integer stamp or game tick; no wall-clock
value is sampled, so the whole ledger reproduces from the seed.

## The scenario

`run_login_baseline(seed)` (seed `314_159`): 96 arrivals over 8 virtual
seconds (12 per second, one every `1e9/12` stamps). Arrivals cycle a small
address pool (4 IPs × 16 ports, salt derived from the same index), so every
address recurs every 16 arrivals and the duplicate-in-flight and cached-reply
reattempt shapes are part of the baseline. One of the four IPs draws a 2-second
ban; the last arrival spoofs the empty address. Accepted arrivals enter the
BaseApp's pending ticket queue and complete the real client state machine
(`start_with_base_addr` → `on_login_app_login_success` →
`on_base_app_login_success`) and the `log_on_complete` switch check. Every
accepted arrival also queues a 32–288 byte resource through
`ServerDownloads::stream`, drained by a 64-byte-per-tick bundle into the real
`DataDownloads` collector.

## Recorded ledger (seed 314_159)

| Field | Value |
|---|---|
| arrivals | 96 |
| accepted | 64 |
| rejected | 32 (7 IP ban, 24 rate limited, 1 silent drop) |
| rejection rate | 0.3333 |
| reattempts | 79 arrivals hit a cached address |
| pending peak | 1 (each ticket is answered within its own arrival) |
| cached peak | 17 (16 address-pool entries + the spoofed one) |
| sessions | 64 — exactly the accepted arrivals, in order |
| latency | p50 = 1, p95 = 2, p99 = 2, peak = 2 virtual ticks |
| downloads | 64 streams, 10 288 bytes sent = 10 288 received |
| server queue peak | 64 |
| client in-flight peak | 1 — the server bundle is strictly ordered |

The latency faces: an accepted fresh attempt answers in 1 tick; a duplicate
whose earlier attempt still waits on the DB (`IgnoreInProgress`) answers in 2;
a rejected attempt answers immediately (0).

## Reproducibility

`the_baseline_reproduces_from_its_seed` runs the flood twice and asserts the
entire `LoginBaselineLedger` is equal, including the per-arrival outcomes,
the download records and the byte counts.

## Coverage registration

llvm-cov 0.9.1, filter `-- login_baseline`: `login_baseline.rs` **425/428 =
99.30%** lines, 100% regions. The three missed lines are the post-version
gate's `DropSilently` arm and the closing braces of two covered regions.
The arm is dead by construction: `check_post_version_gates` only ever
returns `Reject` or `Proceed` — the silent drop is a pre-version gate face
(spoofed empty address), which the flood's last arrival exercises.

## Gates

- `cargo test -p server-runtime --lib`: 1708 tests green (16 ignored).
- `cargo fmt -p server-runtime --check`: clean.
- `cargo clippy -p server-runtime --all-targets -- -D warnings`: no new
  lint (only the pre-existing unused-workspace-dependency warning for
  `blowfish`).
