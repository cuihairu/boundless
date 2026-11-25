# OPT-06-01-04 — Re-run duplicate login, reconnect and old client

Acceptance: **the behaviour is identical**.

`crates/server-runtime/src/login_rerun.rs` drives the three faces once per
seed and records every decision in a comparable ledger:

1. **Duplicate login** — the real DBApp gate (`on_duplicate_login`) against
   a live base: the first attempt starts a re-logon, a second attempt while
   one is pending is rejected with the reference message, the
   `LOG_ON_ATTEMPT_TOOK_CONTROL` reply finishes it, a later reply is stale
   and ignored, the restarted attempt arms wait-for-destroy and times out
   with the reference message, a log-off re-runs the login, and a later
   attempt the base refuses is rejected ("Relogin denied.").
2. **Reconnect** — the real proxy session and pending-ticket queue: the
   client presents the OLD session key and the queue hands over the ticket
   (`AttachClient`, attempts = 2); the other queue faces are recorded too
   (unknown key → `InvalidTicket`, destroyed proxy → `ProxyDestroyed`,
   existing channel → `ChannelCollision`); the switch continues to the
   answering BaseApp, a reply whose BaseApp is not the winner is
   `LoginFailed(1)`, and a reply whose client already logged off stays
   `AlreadyLoggedOff`. A true re-send (same credentials) is answered from
   the cached request (`ResentSuccess`), not re-driven through the DBApp.
3. **Old client** — the replay prefix protocol-version gate: the current
   client's prefix verifies and a copy stamped with a different protocol
   version is rejected at the same boundary. This is the port's only
   runtime-observable old-client face; there is no fleet runner.

## Behaviour identity

`the_rerun_reproduces_from_its_seed` runs the scenario twice at the same
seed and asserts the whole ledger equal, bit for bit.
`the_shape_holds_across_seeds` runs it at two seeds and asserts
`RerunLedger::decisions()` — every outcome kind, message and flag, with the
seed-derived addresses, keys and ids normalised out — is identical across
runs while the presented and regenerated session keys differ. The decisions
are structural: the same inputs produce the same outcomes, which is the
identity the acceptance asks to verify.

## Gates

- `cargo test -p server-runtime --lib`: 1724 tests green (16 ignored).
- `cargo fmt -p server-runtime --check`: clean.
- `cargo clippy -p server-runtime --all-targets -- -D warnings`: no new
  lint (only the pre-existing unused-workspace-dependency warning for
  `blowfish`).
- llvm-cov 0.9.1, filter `-- login_rerun`: **100%** lines.
