# OPT-07-01-03 — Quantify the shared-data propagation message count

Acceptance: **量化消息量变化** (quantify the message-count change).

`crates/server-runtime/src/shared_data_baseline.rs` drives a seeded
96-mutation sequence (four scopes × 8 keys × 4 values, one mutation in
five a delete) through the real propagation chain — the `BaseAppScheduler`
and `CellAppMgr` `set_shared_data` / `del_shared_data` faces, in the hop
order `shared_data.rs` documents — and counts every message the chain
produces. Four wired BaseApps and three registered CellApps; the
CellAppMgr holds the Global authority and the BaseAppMgr the BaseApp
authority, with the forward/relay loop for the global scopes.

`crates/server-runtime/src/shared_data_tuning.rs` runs the same sequence
through the same faces twice — shipped, and with the tuned **no-op
suppression**: a set whose value the authoritative map already holds, or
a del of an absent key, is skipped before it can produce a message. The
load is identical (`same_load`: same mutation count, and the final
authoritative base/cell/global maps are asserted equal), so the message
difference is purely the suppression's effect.

## Recorded ledgers (seed 1 618 033)

| Message kind | Shipped | Tuned |
|---|---|---|
| BaseApp broadcasts (4 apps) | 284 | 216 |
| CellApp broadcasts (3 apps) | 219 | 174 |
| Global-from-BaseApp forwards | 24 | 17 |
| Global/BASE_APP relays | 48 | 37 |
| **Total** | **575** | **444** |
| Suppressed no-op writes | 0 | 21 |

**The change: 131 fewer messages (−22.8%) over an identical load.** One
suppressed mutation removes a whole chain leg at once — a global-set
no-op kills 1 relay + 4 base broadcasts (5), a forwarded no-op kills
forward + relay + 4 base + 3 cell (9), a cell no-op kills 3 — which is
why 21 skips save 131 messages. The final authoritative maps are
identical (7 base keys, 5 cell keys, 9 global keys on both runs); the
suppression changes no state, only the traffic.

`the_suppression_holds_across_seeds` repeats the comparison at three
seeds and asserts the same shape every time (no suppression shipped,
positive suppression tuned, strictly fewer messages, same load).

## Reproducibility

`the_comparison_reproduces_from_its_seed` runs the whole comparison
twice and asserts both ledgers equal bit for bit. No wall-clock value is
sampled.

## Coverage registration

llvm-cov 0.9.1, filter `-- shared_data`: `shared_data_tuning` **100%**
lines and regions; `shared_data_baseline` **98.58%** lines, 100%
regions. The three missed lines are dead by construction inside the
harness: the false arms of the forward and relay `is_some` checks (the
wired managers always have their peer registered, so both always fire)
and the defensive `_` match arm the seeded sequence can never draw.

## Gates

- `cargo test -p server-runtime --lib`: 1741 tests green (16 ignored).
- `cargo fmt -p server-runtime --check`: clean.
- `cargo clippy -p server-runtime --all-targets -- -D warnings`: no new
  lint (only the pre-existing unused-workspace-dependency warning for
  `blowfish`).
