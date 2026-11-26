# OPT-16-01-03 — Load and install path recheck

The acceptance: **external interfaces and artifacts stay compatible**
(`外部接口与产物兼容`). One fixture pair drives both directions: the
placeholder `server/bw.xml` config is packed through the tool face
(install) and both artifacts — ours and the reference EL7 tool's — are
read back against the XML source (load).

## The two paths

**Install** — `asset_pipeline::res_packer::convert` through a
`BwResource` entry over a scratch root, the exact shape the
`res_packer` tool binary uses. A clean install emits nothing and sets
no error flag.

**Load** — fresh `PackedSection` readers walk both artifacts against
the XML tree: node names, child order, every leaf value.

## The compat contracts

| Contract | Result |
| --- | --- |
| Our artifact ≡ EL7 `bw.packed` | **byte-identical, 177 bytes** |
| Tree read-back (names, order, leaf values) | 7 nodes / 4 leaves, 0 mismatches, both artifacts |
| Re-packing our artifact | byte-identical (idempotent) |
| Typed spot-checks | personality `fantasydemo`, `shouldAcceptUnknownUsers` true, `entityTypeForUnknownUsers` `Avatar`, `archivePeriod` 0 |
| Interface faces | `copy_file` ok, `file_exists` both ways, 2 message-exact failures |
| `remove_trailing_slash` | `/` and `\`, exactly one slash |

The message-exact failures pin the reference strings: the copy failure
(`Error: Cannot open source file <path>.`) and the convert failure
(`ERROR: PackedSection::convert: Failed to open data section <path>`,
no trailing period — that quirk is in the port's format string). The
ERROR line sets `errors_occurred`; the earlier WARNING face clears it.

This re-runs the BW-15-02-05 cross-check (the inline
`an_el7_res_packer_file_reads_back_identical_to_its_xml_source` in
`crates/resources/src/packed.rs`) through the *tool-level* face and
extends it with the re-pack and interface contracts, so the load and
install surfaces the workspace consumes stay pinned in one harness.

## Harness and fixtures

`crates/asset-pipeline/tests/install_path_recheck.rs` reads:

- `docs/review/fixtures/fantasydemo_def/res/server/bw.xml` — the XML
  source.
- `docs/review/fixtures/el7_res_packer/bw.packed` — the reference
  EL7 artifact (recorded by
  `scripts/el7-interop/run-el7-res-packer.sh`).

It gates byte identity, zero tree mismatches, re-pack idempotence and
the exact failure strings; node counts are recorded.

```sh
INSTALL_RECHECK_OUT=<abs path> cargo test -p asset-pipeline \
    --test install_path_recheck -- --nocapture
```

`docs/review/fixtures/install-recheck/install-recheck.tsv` — 11 rows,
captured with the command above.
