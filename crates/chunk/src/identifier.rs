//! BW-10-01-01 chunk identifiers and their grid coordinates.
//!
//! Three reference sites define the identifier ↔ coordinate mapping:
//!
//! - `lib/chunk/chunk_format.hpp` `ChunkFormat::outsideChunkIdentifier`:
//!   grid coordinates → the `xxxxzzzzo` identifier, optionally prefixed with
//!   the hashed subdirectory levels (`001xx001xx/sep/…`) that keep wide
//!   spaces from putting every `.chunk` file in one directory.
//! - `lib/chunk/geometry_mapping.cpp` `GeometryMapping::gridFromChunkName`:
//!   the loader's parse — it takes the last `xxxxxxxxo` tail of whatever
//!   prefixed identifier the file was opened with.
//! - `lib/chunk/chunk_terrain.cpp` `ChunkTerrain::outsideChunkIDToGrid`:
//!   the terrain cache's parse, which is the exact inverse of the un-prefixed
//!   form and sign-extends to 32-bit. Its `charToHex` helper accepts digits
//!   and lowercase hex only (uppercase letters silently decode as 0), unlike
//!   the loader's `HexLookup` which accepts both cases and reports failure.

/// `Chunk::isOutsideChunk` (`chunk.cpp:532-536`): an outside (grid) chunk
/// identifier always ends in `o`; inside chunk identifiers end in `i`.
pub fn is_outside_chunk(identifier: &str) -> bool {
    identifier.as_bytes().last() == Some(&b'o')
}

/// `charToHex` (`chunk_terrain.cpp:541-553`): digits and lowercase hex only,
/// every other byte decodes as 0 and never fails. Kept separate from
/// [`hex_lookup`] because the two call sites genuinely disagree.
pub fn char_to_hex(c: u8) -> u32 {
    if c.is_ascii_digit() {
        u32::from(c - b'0')
    } else if (b'a'..=b'f').contains(&c) {
        u32::from(c - b'a') + 0xa
    } else {
        0
    }
}

/// `HexLookup` (`geometry_mapping.cpp:357-399`): digits plus both hex cases,
/// `from_hex` reports whether all four bytes were valid.
pub fn hex_lookup(f: [u8; 4]) -> Option<i16> {
    let mut value: i32 = 0;
    for (shift, &byte) in [12, 8, 4, 0].iter().zip(f.iter()) {
        let digit = match byte {
            b'0'..=b'9' => i32::from(byte - b'0'),
            b'a'..=b'f' => i32::from(byte - b'a') + 10,
            b'A'..=b'F' => i32::from(byte - b'A') + 10,
            _ => return None,
        };
        value |= digit << shift;
    }
    Some(value as i16)
}

/// `ChunkTerrain::outsideChunkIDToGrid` (`chunk_terrain.cpp:567-591`): an
/// 8-hex identifier (`xxxxzzzz`, *without* the trailing `o`) becomes signed
/// 32-bit grid coordinates via sign extension of bit 15.
///
/// Returns `None` when the identifier is not exactly 8 characters. Invalid
/// hex digits decode as 0 exactly like the reference (see [`char_to_hex`]).
pub fn outside_chunk_id_to_grid(chunk_id: &str) -> Option<(i32, i32)> {
    let b = chunk_id.as_bytes();
    if b.len() != 8 {
        return None;
    }
    let decode = |chars: &[u8]| -> i32 {
        let mut v = (char_to_hex(chars[0]) << 12)
            | (char_to_hex(chars[1]) << 8)
            | (char_to_hex(chars[2]) << 4)
            | char_to_hex(chars[3]);
        if v & 0x8000 != 0 {
            v |= 0xffff_0000;
        }
        v as i32
    };
    Some((decode(&b[0..4]), decode(&b[4..8])))
}

/// `GeometryMapping::gridFromChunkName` (`geometry_mapping.cpp:404-419`):
/// parse the `xxxxxxxxo` tail of a chunk identifier, whatever subdirectory
/// prefix precedes it. Inside chunk names (trailing `i`) and special names
/// have no grid coordinates.
pub fn grid_from_chunk_name(chunk_name: &str) -> Option<(i16, i16)> {
    let b = chunk_name.as_bytes();
    if b.last() != Some(&b'o') || b.len() < 9 {
        return None;
    }
    let x = hex_lookup(b[b.len() - 9..b.len() - 5].try_into().ok()?)?;
    let z = hex_lookup(b[b.len() - 5..b.len() - 1].try_into().ok()?)?;
    Some((x, z))
}

/// `ChunkFormat::outsideChunkIdentifier` (`chunk_format.hpp:21-54`): grid
/// coordinates → the identifier, including the subdirectory prefix unless
/// `single_dir`. The coordinate wrap is unsigned 16-bit arithmetic: the
/// prefix tests decide on the wrapped value, and the prefix digits are the
/// wrapped value's high bits.
pub fn outside_chunk_identifier(grid_x: i32, grid_z: i32, single_dir: bool) -> String {
    let gridxs = grid_x as u16;
    let gridzs = grid_z as u16;
    let mut id = String::new();
    if !single_dir {
        if gridxs.wrapping_add(4096) >= 8192 || gridzs.wrapping_add(4096) >= 8192 {
            id.push_str(&format!(
                "{:01x}xxx{:01x}xxx/sep/",
                gridxs >> 12,
                gridzs >> 12
            ));
        }
        if gridxs.wrapping_add(256) >= 512 || gridzs.wrapping_add(256) >= 512 {
            id.push_str(&format!(
                "{:02x}xxx{:02x}xxx/sep/",
                gridxs >> 8,
                gridzs >> 8
            ));
        }
        if gridxs.wrapping_add(16) >= 32 || gridzs.wrapping_add(16) >= 32 {
            id.push_str(&format!("{:03x}xx{:03x}xx/sep/", gridxs >> 4, gridzs >> 4));
        }
    }
    id.push_str(&format!("{:04x}{:04x}o", gridxs, gridzs));
    id
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outside_test_is_the_trailing_o() {
        assert!(is_outside_chunk("00000000o"));
        assert!(is_outside_chunk("exxxexxx/sep/efffefffo"));
        assert!(!is_outside_chunk("00000000i"));
        assert!(!is_outside_chunk("heaven"));
        assert!(!is_outside_chunk(""));
    }

    #[test]
    fn grid_parses_the_identifier_tail() {
        assert_eq!(grid_from_chunk_name("00000000o"), Some((0, 0)));
        assert_eq!(grid_from_chunk_name("00100010o"), Some((0x0010, 0x0010)));
        // Subdirectory prefixes are ignored: only the last nine bytes count.
        assert_eq!(
            grid_from_chunk_name("001xx001xx/sep/00100010o"),
            Some((16, 16))
        );
        // 0xe000 as u16 has bit 15 set: the i16 view is negative.
        assert_eq!(grid_from_chunk_name("e000e000o"), Some((-8192, -8192)));
        assert_eq!(grid_from_chunk_name("efffefffo"), Some((-4097, -4097)));
        // Uppercase hex is accepted by the loader's HexLookup.
        assert_eq!(
            grid_from_chunk_name("ABCD1234o"),
            Some((0xabcd_u16 as i16, 0x1234))
        );
    }

    #[test]
    fn grid_parse_rejects_non_outside_names() {
        assert_eq!(grid_from_chunk_name("00000000"), None);
        assert_eq!(grid_from_chunk_name("00000000i"), None);
        assert_eq!(grid_from_chunk_name("heaven"), None);
        assert_eq!(grid_from_chunk_name("g0000000o"), None);
        assert_eq!(grid_from_chunk_name("0000000o"), None);
        assert_eq!(grid_from_chunk_name("o"), None);
    }

    #[test]
    fn terrain_decode_sign_extends() {
        assert_eq!(outside_chunk_id_to_grid("00000000"), Some((0, 0)));
        assert_eq!(outside_chunk_id_to_grid("00100010"), Some((16, 16)));
        assert_eq!(outside_chunk_id_to_grid("e000e000"), Some((-8192, -8192)));
        assert_eq!(outside_chunk_id_to_grid("7fff7fff"), Some((32767, 32767)));
        assert_eq!(outside_chunk_id_to_grid("80008000"), Some((-32768, -32768)));
        // Exactly eight characters, no trailing 'o'.
        assert_eq!(outside_chunk_id_to_grid("00000000o"), None);
        assert_eq!(outside_chunk_id_to_grid("000"), None);
    }

    #[test]
    fn terrain_decode_is_lowercase_only() {
        // `charToHex` ignores uppercase: 'A'..'D' decode as 0, 'E'/'F' too,
        // while "01" still contributes to z.
        assert_eq!(outside_chunk_id_to_grid("ABCDEF01"), Some((0, 1)));
        // Lowercase hex decodes normally, with bit 15 sign-extended.
        assert_eq!(
            outside_chunk_id_to_grid("abcdef01"),
            Some((0xabcd_u16 as i16 as i32, 0xef01_u16 as i16 as i32))
        );
    }

    #[test]
    fn identifiers_in_the_flat_range_have_no_prefix() {
        // -16..=15 keeps u16(coord + 16) < 32 for both axes: no prefix level.
        assert_eq!(outside_chunk_identifier(0, 0, false), "00000000o");
        assert_eq!(outside_chunk_identifier(-16, -16, false), "fff0fff0o");
        assert_eq!(outside_chunk_identifier(15, 15, false), "000f000fo");
        assert_eq!(outside_chunk_identifier(-1, 1, false), "ffff0001o");
        // singleDir skips every prefix level regardless of range.
        assert_eq!(outside_chunk_identifier(4096, 4096, true), "10001000o");
    }

    #[test]
    fn prefixed_identifiers_match_the_reference_formats() {
        // x=16 trips the 4-bit level only: "%03xx%03xx/sep/" with (1, 1).
        assert_eq!(
            outside_chunk_identifier(16, 16, false),
            "001xx001xx/sep/00100010o"
        );
        // One axis outside the flat range is enough for a prefix; the other
        // axis contributes its own wrapped high bits.
        assert_eq!(
            outside_chunk_identifier(17, 0, false),
            "001xx000xx/sep/00110000o"
        );
        assert_eq!(
            outside_chunk_identifier(100, -200, false),
            "006xxff3xx/sep/0064ff38o"
        );
        // |coord| >= 4096 trips all three levels.
        assert_eq!(
            outside_chunk_identifier(4096, 4096, false),
            "1xxx1xxx/sep/10xxx10xxx/sep/100xx100xx/sep/10001000o"
        );
        // x = -4097 wraps to 0xefff: prefixes "e", "ef", "eff".
        assert_eq!(
            outside_chunk_identifier(-4097, -4097, false),
            "exxxexxx/sep/efxxxefxxx/sep/effxxeffxx/sep/efffefffo"
        );
        // -32768 wraps to 0x8000: prefixes "8", "80", "800".
        assert_eq!(
            outside_chunk_identifier(-32768, 32767, false),
            "8xxx7xxx/sep/80xxx7fxxx/sep/800xx7ffxx/sep/80007fffo"
        );
    }

    #[test]
    fn identifier_round_trips_through_the_tail_parse() {
        // Deterministic sweep over the full i16 range on both axes: the
        // formatter and the tail parser must agree everywhere.
        let mut x: i32 = -32768;
        let mut z: i32 = 32767;
        for _ in 0..2048 {
            let id = outside_chunk_identifier(x, z, false);
            assert_eq!(
                grid_from_chunk_name(&id),
                Some((x as i16, z as i16)),
                "round trip failed for ({x}, {z})"
            );
            let tail = &id[id.len() - 9..];
            assert_eq!(
                outside_chunk_id_to_grid(&tail[..8]),
                Some((x, z)),
                "terrain decode failed for ({x}, {z})"
            );
            x = x.wrapping_add(17);
            z = z.wrapping_sub(31);
        }
    }
}
