# OPT-09-01-01 — MySQL and PostgreSQL baselines over one dataset

Acceptance: **使用同一数据集** (both baselines use the same dataset).

The dataset is defined once and collected on both backends' real servers:

1. **The SQL leg** — `tests/store/common/baseline_dataset.sql`, a *common*
   store case: `ci/store.sh` runs the very same statements against an
   isolated MariaDB (private socket, private data directory) and an
   isolated PostgreSQL (private run directory). 24 entity rows in the
   reference `tbl_<Type>`/`sm_<prop>` layout (row i: id 1000+i, name
   `ent-<id>`, health 100+i, speed `(i%4)*0.5` — exact in binary floating
   point so the sum compares exactly — alive when `i%3=0`) plus the
   account mapping's four rows (alpha..delta onto the first four ids).
   The assertions are the statements themselves: every guard row
   violates its `CHECK (v = 0)` unless the count or sum it pins is
   exact — 24 rows, 24 distinct ids, health sum 2676, 8 alive, speed sum
   18.0, 4 unique log-on names onto 4 distinct entity ids, an even-id
   update moving the sum by exactly 12 (2676→2688), a delete leaving 23
   rows at sum 2565.

2. **The decoded-row leg** —
   `crates/db-postgres/tests/baseline_dataset.rs` expresses the same
   24+4 dataset through the storage faces: `import_entities` writes all
   24 decoded `MySqlEntityRow`s (the MySQL row contract), `import_accounts`
   writes the four mapping rows, every row reads back equal through
   `get_entity`, the same pinned statistics hold over the read-back rows,
   and a rerun skips all 28 rows (written=0) — the baseline is
   re-collectable without duplicating anything.

## Collected baselines (this tree, this runner)

| Backend | Provisioning | Cases | `baseline_dataset` |
|---|---|---|---|
| MariaDB 11.8 | throwaway server, private socket | 4/4 ok | ok |
| PostgreSQL 18.6 | throwaway server, private run dir | 2/2 ok | ok |

Both engines answered with identical affected counts and identical
re-read sums on every statement — the same dataset lands, updates,
deletes and reads back the same on both backends. A wrong hand-laid
pattern (alive period 4 instead of `i%3`) was caught by the fixture's
own guard on **both** engines before the green run, which is the CHECK
idiom working as designed.

The Rust leg runs against the local PostgreSQL server (peer auth); it
skips with a notice when no server is reachable, the crate's established
gate. `cargo test -p db-postgres`: 133 green, nothing skipped here.

## Gates

- `cargo test -p db-postgres`: 133 passed, 0 failed.
- `bash ci/store.sh`: no failed lane (mysql 4/4, postgres 2/2, sqlite
  4/4, xml 1/1 — all green, no residue).
- `cargo fmt -p db-postgres --check`: clean.
- `cargo clippy -p db-postgres --all-targets -- -D warnings`: no new
  lint.
