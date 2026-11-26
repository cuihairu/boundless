# OPT-12-01-01 — The observation channels' cost baseline

The acceptance: **logging, Watcher and Profiler separated** (`分离日志、
Watcher 和 Profiler`). The three channels a running server pays for
observation live in three crates; the baseline drives each in isolation
so every number belongs to exactly one channel.

## Per-channel per-op cost

| Channel | Operation | Cost | What the op is |
| --- | --- | --- | --- |
| Log (`message_logger`) | one entry written | **198 ns** | `LogEntry::new` + `set_args` + the 48-byte `to_disk_bytes` disk image |
| Log — serialize share | `to_disk_bytes` alone | 193 ns | **97% of the write** — the channel is serialization-bound |
| Watcher (`watcher`) | read through a path | **785 ns** | `get_watcher_string` — path descent + value formatting into a fresh `String` |
| Watcher | write through a path | **821 ns** | `set_watcher_string` — descent + parse + store |
| Watcher | register one leaf | 13.3 µs / 3 leaves | `add_watcher_value` setup (one-off) |
| Profiler (`profiler`) | one event | **164 ns** | `add_entry` — mutex lock + 32-byte name copy into the double buffer |
| Profiler | one frame's `tick` | **2.1 µs** @ 100 entries | buffer swap + inline entry processing |
| Profiler | one profiled frame | **20.4 µs** | 100 events (16.4 µs) + tick (2.1 µs), measured whole |
| Profiler | `init` | 524 µs one-off | buffer pool allocation |

## The separation story

- **The watcher is the most expensive single op** (~0.8 µs): every
  query pays a directory-tree descent plus a `String` allocation. It is
  the tool-facing surface though — query-rate, not tick-rate — so its
  cost lands on tools, not the frame.
- **The log channel is serialization-bound**: 97% of writing an entry
  is producing the fixed 48-byte disk image; building the record is
  noise.
- **The profiler is the high-frequency face**: 164 ns per event is the
  cheapest single op, but it fires per event per frame — a frame that
  records 100 events pays ~20 µs, ~9% of a 60 fps budget at the
  baseline's shape.

Per-channel attribution is what the follow-ups need: OPT-12-01-02's
enable/disable deltas compare against these faces, and the gates it
flips are `profiler::enable`, the watcher event-stats switch and the
logging sink itself.

## Harness

`crates/server-runtime/tests/observation_channels_baseline.rs` — the
three legs driven from outside their own crates (test-only dev-deps:
`watcher`, `profiler`, `message_logger`), one deterministic run. The
value faces are asserted per leg (disk round-trip, live leaf value,
write acceptance and unknown-path rejection).

```sh
OBS_CHANNELS_OUT=<abs path> cargo test -p server-runtime \
    --test observation_channels_baseline -- --nocapture
```

## Fixtures

`docs/review/fixtures/obs-channels/obs-channels.tsv` — 25 rows,
captured with the command above.
