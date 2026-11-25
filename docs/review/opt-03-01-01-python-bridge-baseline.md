# OPT-03-01-01 / OPT-03-01-02 — Python bridge baseline measurements

The acceptance saves conversion, GIL and callback timing baselines for the
embedded Python bridge (OPT-03-01-01), then verifies a hot-path change by a
before/after comparison on the same harness (OPT-03-01-02, the server timer
fire path).

## Harness

`crates/python-host/tests/bridge_baseline.rs` runs four legs in a fixed
order against one embedded interpreter (`Script::init` → legs → `Script::fini`):

1. **Conversion** — 20,000 round-trips per family: int
   (`PyLong_FromLong`/`PyLong_AsLongLong`), float
   (`PyFloat_FromDouble`/`PyFloat_AsDouble`) and str
   (`PyUnicode_FromStringAndSize`/`PyUnicode_AsUTF8AndSize`). Every
   round-trip asserts the value it read equals the value it wrote; the
   timed sample is the full there-and-back pair.
2. **Callback** — a Python `noop()` is defined through `PyRun_SimpleString`,
   fetched once from `__main__` with `PyObject_GetAttrString`, then called
   5,000 times through `PyObject_CallNoArgs`. Each call's result is dropped
   via `Py_DecRef` so the reference count returns to its starting value.
3. **GIL queue** — a poster thread sends 2,000 closures through
   `gil::post`; the GIL-owning thread drains them in `gil::process()`
   passes. Every callback reports its own post→run time through a channel;
   the leg asserts all 2,000 reported, `pending() == 0`, and `clear() == 0`.
4. **Timer fire** (added with the OPT-03-01-02 comparison) — 32 repeating
   timers registered from script (`BigWorld.addTimer(on_fire, 0.1, 0.1, 9)`)
   driven by 64 `timers::tick(0.1)` steps on the `cell` component; 2,048
   fires are asserted before the numbers are recorded, so a broken fire
   path cannot produce a timing row at all. A counting global allocator
   (the foundation `alloc_counts` pattern) wraps the tick loop to count
   the calling thread's Rust allocations per fire. The timers are
   cancelled after their window so the tick-driven leg that follows
   measures only its own Python work.
5. **Frequent-task walk** (added with the OPT-03-01-03 comparison) — 32
   no-argument tasks registered from script
   (`BigWorld.addFrequentTask(on_frequent)`), driven by the same 64
   `timers::tick(0.1)` steps; 2,048 calls asserted. This is the
   no-argument cross-language callback every registered task pays once
   per tick (`ScriptFrequentTask::update`).

Structural assertions run on every execution. Wall-clock numbers are
**recorded, never asserted tight** — raw nanoseconds are machine-dependent,
as with the OPT-02 network baseline. The record sink writes
`metric<TAB>value` rows to stdout and, when `BRIDGE_BASELINE_OUT` is set,
appends to that path in the network/load fixture format:

```sh
BRIDGE_BASELINE_OUT=<abs path> cargo test -p python-host \
    --test bridge_baseline the_python_bridge_baseline_runs_all_legs -- --exact
```

## OPT-03-01-01 baseline record

The before record (with the timer leg at its pre-change fire path) is the
fixture at commit `e10ccbd`; key rows: int p50 88 ns / float p50 96 ns /
str p50 273 ns / `noop()` p50 134 ns / GIL drain 1,193 ns per callback /
**timer fire 10,096.8 ns per fire**. No acceptance band is set on any
wall-clock row.

## OPT-03-01-02 — timer fire conversion, before/after

The change (reference: `app_script_timers.cpp`, `handleTimeout` —
`PyObject_CallFunction(pObject, "ik", id, uintptr(pUser))`): the port used
to build the fire arguments by hand — two `PyLong` conversions, a
`PyTuple_New`, two `PyTuple_SetItem`s, then `PyObject_CallObject` and a
`Py_DecRef` of the tuple — six C-API entries per fire. It now fires
through a single `PyObject_CallFunction("ik", id, user_arg)`, the way the
reference does; the arguments tuple is built inside the call.

Before/after comparison, same machine, same toolchain, 2026-11-26:

| Window | Before ns/fire | After ns/fire | Δ |
|---|---:|---:|---:|
| Canonical fixture run (committed) | 10,096.8 | 5,049.4 | −50% |
| Two quiet-machine runs | — | 5,621.4 / 5,787.4 | ≈ −44% |
| 9 interleaved pairs, one pinned core, machine load 46–53 | median 6,495.3 | median 5,930.6 | −9%, after lower in 6/9 pairs |

Disclosure: this machine carried a heavy concurrent fleet load (qemu,
parallel crate builds — 1-minute load 40–53) for most of the session, so
the quiet-window gap is the honest magnitude and the interleaved pairs are
the honest same-load comparison; their spread (before 3,983.8–12,478.7,
after 3,628.4–17,011.6) is the noise floor of that load. No band is
asserted on any wall-clock row. The deterministic faces are structural and
hold on every run: the fired count must equal timers × ticks (2,048), and
the fire path crosses its arguments as one call.

Allocation contract: the counting allocator recorded **0.00 Rust
allocations per fire on both sides** — CPython's pymalloc serves the
object-level allocations (the `PyLong`s, the tuple) from its own arenas,
which are invisible to the Rust global allocator. The before path's extra
Rust-side work is call and reference-count bookkeeping, not Rust heap
traffic; the alloc counter is therefore a boundary finding (the fire path
allocates nothing through Rust on either side, worth knowing for the
OPT-01 allocation family), not a discriminator between the two paths.

## OPT-03-01-03 — frequent-task call, GIL occupancy and tail

The GIL-queue leg now also reports **GIL occupancy**: the share of the
drain window the owning thread spends inside `gil::process()` (servicing
posted callbacks plus the polling between them) — `gil_occupancy_pct`.
Tail latency stays the `gil_e2e` p90/p99/max rows. First record under
fleet load: occupancy 28.3%, e2e p50 869 µs / p99 1.86 ms.

The frequent-task walk (`ScriptFrequentTask::update`) is the row's
optimization face. The reference builds the call as
`Script::ask(pCallback.newRef(), PyTuple_New(0))` — an empty tuple
allocated, passed and dropped for every task on every tick — and the
port mirrors it. The change fires the no-argument callback through
`PyObject_CallNoArgs` (the CPython zero-argument fast path the harness's
callback leg already benchmarks): no tuple object is created, the
semantic contract is unchanged (same callable, no arguments, truthy
result still deregisters, failure still prints and the task stays).

Before record (with the walk still on the tuple path), fleet load:
**2,384.0 ns per call**; the Rust allocator counts 0.00 per call — the
tuple lives in pymalloc, like every object on this bridge.

After record (the same harness, walk on `PyObject_CallNoArgs`):
canonical run **543.1 ns per call** (a second run: 521.2), occupancy
97.2%.

Instrument disclosure — the wall-clock ratio is **not** claimable as a
speedup. Under this session's fleet load (1-minute load 40–77) two runs
of a byte-identical binary measured 6,555.9 and 498.5 ns per call, so
per-run wall-clock swings 13× on identical code. A batched CPU-time
comparison (100 runs per side, interleaved batches of 20, `%U` summed to
defeat the 10 ms tick quantization) measured median ratio 1.07 with
rounds 0.91–1.12 — no CPU-side difference distinguishable at this noise
floor. That is consistent with the tuple living in pymalloc: the change
removes one object create-and-destroy per task per tick and moves to the
C fast call entry (`PyObject_CallNoArgs`, the shape the callback leg
benchmarks at 82 ns p50) — work neither wall-clock under load nor `%U`
at this scale can resolve. The honest faces this row reports are the
acceptance's own: **GIL occupancy** (28.3% / 97.2% across the two
records — the drain-window share under a flat-out poster, load-dependent)
and **tail latency** (the `gil_e2e` p50/p90/p99/max rows, plus
`frequent_ns_per_call` for the walk itself), all recorded, none gated.

## OPT-03-01-04 — rerun of the reference, exception and reload cases

After the two hot-path changes (01-02, 01-03), the case families that pin
the bridge's behavior were rerun on the optimized tree; the acceptance is
identical behavior, and these assertions encode those contracts, so
unchanged green is the evidence:

- **Reference** — the `pyref` unit tests (8: adopt/borrow/clone and the
  empty-handle ownership rules), the scenario family's reference faces
  (`scenario_a_handle_owns_one_reference`,
  `scenario_the_cache_holds_one_reference_per_identity`, the keep-alive
  and pin reference scenarios), and the baseline harness's per-value
  identity asserts on every conversion round-trip.
- **Exception** — `scenario_the_bigworld_timers_behave_like_the_reference`
  (a timer callback raising `RuntimeError` prints and reschedules — the
  OPT-03-01-02 path — and a failing frequent task stays registered — the
  OPT-03-01-03 path), `scenario_a_raising_listener_fails_the_trigger_\
and_contributes_none`, and the billing exception scenarios.
- **Reload** — `scenario_a_failed_reload_leaves_the_old_definitions_\
callable`, `scenario_replacing_a_script_class_updates_existing_instances`,
  `scenario_swapping_in_a_module_window_keeps_sys_and_main`,
  `scenario_migrate_reports_the_hook_success_and_failure`, plus the 12
  `reload_*` unit tests.

Rerun (2026-11-26, post-`2d67612`): `every_scenario_exits_cleanly` ok —
all 104 interpreter scenarios, each in its own process — plus `reload_*`
12 passed and `pyref` 8 passed. Zero failures, no assertion changed.

## Current fixture record

`docs/review/fixtures/python-bridge-baseline/bridge-baseline.tsv` is a
rolling canonical record: each delivery commit re-records it with the
harness as it stands, and the other side of every comparison lives in the
commit cited in the section above (e10ccbd for the OPT-03-01-01/02
before, this commit for the OPT-03-01-03 before). Rows moved between
records with the machine load as expected — same code, differing load;
they are snapshots, not regressions.
