# OPT-03-01-01 — Python bridge baseline measurements

The acceptance saves conversion, GIL and callback timing baselines for the
embedded Python bridge. This row establishes the measurement harness; it
does not change any hot path.

## Harness

`crates/python-host/tests/bridge_baseline.rs` runs three legs in a fixed
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

Structural assertions run on every execution. Wall-clock numbers are
**recorded, never asserted tight** — raw nanoseconds are machine-dependent,
as with the OPT-02 network baseline. The record sink writes
`metric<TAB>value` rows to stdout and, when `BRIDGE_BASELINE_OUT` is set,
appends to that path in the network/load fixture format:

```sh
BRIDGE_BASELINE_OUT=<abs path> cargo test -p python-host \
    --test bridge_baseline the_python_bridge_baseline_runs_all_legs -- --exact
```

## Record

`docs/review/fixtures/python-bridge-baseline/bridge-baseline.tsv` (29 rows,
`cargo test -p python-host --test bridge_baseline`, 2026-11-26 toolchain):

| Family | p50 | p90 | p99 |
|---|---:|---:|---:|
| int conversion round-trip | 88 ns | 120 ns | 594 ns |
| float conversion round-trip | 96 ns | 127 ns | 525 ns |
| str conversion round-trip | 273 ns | 334 ns | 3.9 µs |
| `noop()` callback | 134 ns | 182 ns | 197 ns |
| GIL post→run end-to-end | 822 µs | 1.15 ms | 1.17 ms |
| GIL drain throughput | — | — | 1,193 ns/callback |

The GIL end-to-end figures are from a producer running flat-out against a
single drain loop, so they measure queueing under saturation rather than
the scheduling latency of an idle host. They are recorded with the rest of
the table so the OPT-03-01-02/03 comparison legs have a same-machine
starting point; no acceptance band is set on them.

Two consecutive runs of the runner leg produced identical metric names and
identical structural results; the nanosecond values are a snapshot, not a
contract.
