# OPT-13-01-04 — Asset hashes and formats rechecked

The acceptance: **reproducible and original-readable**
(`可复现且原版可读`). The recheck pins both sides of the asset
pipeline: the hash keys the cache and the editor use, and the file
formats the original engine reads.

## The hash side

| Face | Digest (upper 64 bits, hex) | Reproducibility |
| --- | --- | --- |
| 4096-byte content | `9856B01F993513A9` | 8/8 identical |
| `quality=medium;mips=4` string | `F4FC0185664CD4B1` | equals `compute_bytes(text.as_bytes())` |
| `i64(-42)` | `EEB07D2CE8108904` | fixed vector |
| `u64(42)` | `FF3ADD6B3789DAEF` | fixed vector |
| FNV fold `[7,11,13]` | `4098973E757DA3B7` | repeat-identical, order-sensitive (`[13,11,7]` differs) |
| 48-asset cache buckets | 45 distinct low-byte buckets | re-derived identical |

The digest vectors are the regression anchors: any change to the
FNV-1a basis, the byte order or the string encoding breaks them.
`combine` is order-sensitive by contract — the dependency list's
hash depends on input order, so a permutation must move the cache
key.

## The format side

A real 24bpp uncompressed-truecolor TGA (the original's decoder
shape) converts and reads back:

| Check | Result |
| --- | --- |
| TGA source digest | `3D4B54C0EF3067DC` |
| Decoded RGBA digest | `37DE14EFCBE006B1` |
| DDS header fields checked | **10/10** — magic `DDS `, 124-byte size, `DDSD_PIXELFORMAT`, height, width, pitch=`width*4`, 32bpp, R/G/B/A masks |
| DDS byte digest | `8206C7737D5B330B` — stable across runs |
| DDS pixel read-back | pixel-exact |
| `TgaDdsTool::convert` output | byte-identical to `encode_dds` |
| Zip archive members | 3/3 byte-exact through the original `ZipFileSystem` reader |

**Honest caveat on the archive container**: the zip's *overall*
digest changes between captures (the writer embeds creation
timestamps), while each member's bytes and its read-back are
exact. Reproducibility lives in the content hashes and the decoded
pixels — not in the zip container's byte layout. The cache keys on
content hashes, so the timestamps never affect cache identity;
anything comparing whole archives byte-for-byte would be wrong to
do so.

## Harness

`crates/asset-pipeline/tests/asset_format_recheck.rs` — fixed
bytes, fixed pixels, fixed zip members, one temp root. The
resource-backed conversion goes through `BwResource::init_paths`
and `TgaDdsTool::convert`, the same seam the converter uses.

```sh
ASSET_FORMAT_OUT=<abs path> cargo test -p asset-pipeline \
    --test asset_format_recheck -- --nocapture
```

## Fixtures

`docs/review/fixtures/asset-format-recheck/asset-format-recheck.tsv`
— 18 rows, captured with the command above.
