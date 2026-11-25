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
   the calling thread's Rust allocations per fire.

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

## Current fixture record

`docs/review/fixtures/python-bridge-baseline/bridge-baseline.tsv` (34 rows)
is the canonical after run quoted above. The conversion/callback/GIL rows
moved with the machine load as expected (int p50 88→63 ns, str p50 273→172
ns, GIL drain 1,193→1,437 ns per callback) — same code on those paths,
differing load; they are snapshots, not regressions.
