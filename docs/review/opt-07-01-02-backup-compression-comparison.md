# OPT-07-01-02 — Backup serialization and archive compression

The acceptance compares **queues and recovery at the same load**
(`按同负载比较队列与恢复`). The OPT-07-01-01 baseline fixed the write
path's three windows at the shipped (uncompressed) payload; this row
lands the reference's compression transport and compares the same
96-entity load against it.

## The landed face

`Base::writeBackupData` (`base.cpp:2322-2327`) writes the plaintext
header (`id`, `type id`, `templateID`, `databaseID`) and then the body
through a `CompressionOStream` — the type byte first and, for a zip
type, a packed uncompressed length, a packed compressed length and the
zlib payload (`zip_stream.cpp:261-286`); the read side
(`base.cpp:2521`) unwraps the frame. The compression type is the
entity type's `internalNetworkCompressionType`
(`entity_description.cpp:147`, default `NONE`; the `DEFAULT_*`
sentinels resolve to `NONE`, `compression_stream.cpp:53-58`; zip
levels `1..9` map `compressionType - ZIP_1 + Z_BEST_SPEED`,
`compression_stream.cpp:85-89`).

The port now ships that transport beside the shipped payload:

- `write_backup_data_wrapped` / `read_backup_data_wrapped`
  (`base_backup.rs`) — the header stays plaintext (so
  `peek_type_name` and the type check read it before any
  decompression, as the reference does), the body goes through the
  frame (`replay_recording::compress_frame`/`decompress_frame`, the
  BW-07-05-03 `ZipOStream`/`ZipIStream` pair).
- `BackupReceiver::restore_wrapped` — the takeover path for stored
  wrapped backups.
- `archiver::encode_with_compression` / `restore_archived_with_compression`
  — the archive byte stays a backup byte under the same choice.
- `replay_recording::decompress_frame` is now public (it was the
  BW-07-05-03 test probe); its declared-length bound mirrors
  `uncompress`'s `destLen`.

Byte fidelity, pinned by unit tests: the wrapped `NONE` frame is the
shipped payload plus exactly the type byte (header bytes identical,
body bytes identical); every zip level inflates back to the shipped
body; the sentinels resolve to the `NONE` frame.

## Harness

`crates/server-runtime/tests/backup_tuning.rs` — the OPT-07-01-01
load (96 entities, 4 BaseApps, alternating `Account`/`Mailbox`,
`backupPeriodInTicks = 10`, `archivePeriodInTicks = 8`), re-run under
four variants: **raw** (shipped), **none** (framed `NONE`), **zip1**
(`Z_BEST_SPEED`), **zip9** (`Z_BEST_COMPRESSION`). The sender's cycle
schedule slices entity counts, not bytes, so every variant runs the
identical `BackupSender::tick` arithmetic; a fifth leg walks one
entity's body up a size ladder to place the crossover.

```sh
BACKUP_TUNING_OUT=<abs path> cargo test -p server-runtime \
    --test backup_tuning -- --nocapture
```

## Bandwidth and queues at the fleet load

| Variant | Window bytes | Ratio | Held (receiver) | Archive store | Peak depth |
| --- | --- | --- | --- | --- | --- |
| raw | 9600 | 1.000 | 9600 | 9600 | 96 |
| none | 9696 | 1.010 | 9696 | 9696 | 96 |
| zip1 | 10119 | 1.054 | 10119 | 10119 | 96 |
| zip9 | 9976 | 1.039 | 9976 | 9976 | 96 |

**Verdict: at this load every wrapped arm pays, and the zip arms pay
most.** The bodies are 84–116 bytes and mostly binary (`i64`/`f64`/
`f32` props with one short string each); deflate cannot recover the
zlib frame's fixed cost (~6 bytes plus stored-block overhead) from a
body that small — the smallest payloads *grow* (85 → 91), the
`Account` medians barely move (117 → 107 at zip1), and the aggregate
lands above the framed NONE. The schedule is serialization-blind and
the harness pins it: same 96 messages, same per-tick entity split,
same peak depth, and the receiver holds exactly the sent bytes in
every variant.

## Recovery at the fleet load

| Variant | Takeover entities | Consumed bytes | Archive restore |
| --- | --- | --- | --- |
| raw | 24 | 2784 | equal |
| none | 24 | 2808 | equal |
| zip1 | 24 | 2805 | equal |
| zip9 | 24 | 2749 | equal |

The takeover restores every entity the dead app owned, and every
variant's restored property set compares equal to the live entities —
the transport may not lose state, and none of the four arms does.
Wall ms recorded (`recover_*_wall_ms`), never asserted.

## The serialization ladder — where the knob pays

One entity whose `log` prop grows by 64-char lines; payload bytes per
variant:

| Lines | raw | none | zip1 | zip9 |
| --- | --- | --- | --- | --- |
| 1 | 114 | 115 | 113 | 113 |
| 8 | 558 | 559 | 143 | 141 |
| 64 | 4086 | 4087 | 280 | 277 |
| 512 | 32310 | 32311 | 1500 | 1408 |
| 4096 | 258102 | 258103 | 11620 | 10458 |

**Verdict: the crossover sits under ~500 bytes for text-heavy
bodies.** At one line the zip arms are already level with the framed
NONE (113 vs 115); at eight lines zip9 is 3.96× smaller than raw; at
4096 lines it is 24.7×. The framing itself costs exactly one byte at
every rung (the `NONE` row minus the raw row). So the operator's rule
the reference's per-type knob encodes: leave `NONE` for the small
binary-heavy entities (the whole fleet load above), turn the zip
ladder on for types that carry large text/blob state — archive and
backup shrink together because an archive byte is a backup byte.

## Fixtures

`docs/review/fixtures/backup-tuning/backup-tuning.tsv` — 153 rows,
captured with the command above.
