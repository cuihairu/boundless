//! BW-10-02-01 the `BWCS` compiled space settings section.
//!
//! A port of `lib/compiled_space/compiled_space_settings_types.hpp` (the
//! `Header` with its `AABB spaceBounds_`, format magic/version) and
//! `lib/compiled_space/compiled_space_settings.cpp` (`CompiledSpaceSettings::read`).
//!
//! Payload layout: `read(Header)` — a u32 typeSize that must equal 24, then
//! the 24-byte AABB as two little-endian `Vector3`s (min, then max).

use foundation::math::{BoundBox, Vector3};

use crate::binary_format::{BinaryFormat, Magic};

/// `CompiledSpaceSettingsTypes::FORMAT_MAGIC`.
pub const FORMAT_MAGIC: Magic = *b"BWCS";

/// `CompiledSpaceSettingsTypes::FORMAT_VERSION`.
pub const FORMAT_VERSION: u32 = 1;

/// `sizeof(CompiledSpaceSettingsTypes::Header)` — the AABB bounds.
pub const HEADER_SIZE: usize = 24;

/// `CompiledSpaceSettings` (`compiled_space_settings.cpp:12-79`): the
/// space-wide bounding box the converter accumulated from every writer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CompiledSpaceSettings {
    space_bounds: BoundBox,
}

impl CompiledSpaceSettings {
    /// `CompiledSpaceSettings::read` (`compiled_space_settings.cpp:28-58`).
    ///
    /// The reference prints a message from `findAndOpenSection` and then its
    /// own on top when the section is missing; the port concatenates the two
    /// lines into one `Err`, and reports a short or mis-sized payload with
    /// the `is malformed` message alone.
    pub fn read(reader: &BinaryFormat) -> Result<Self, String> {
        let mut stream = reader
            .find_and_open_section(FORMAT_MAGIC, FORMAT_VERSION, "CompiledSpaceSettings")
            .map_err(|err| format!("{err}Failed to map compiled space settings into memory.\n"))?;

        // read(Header): the typeSize prefix must equal sizeof(Header) = 24.
        let raw = stream.read_typed(HEADER_SIZE).ok_or_else(|| {
            format!(
                "CompiledSpaceSettings in '{}' is malformed.\n",
                reader.resource_id()
            )
        })?;

        let min = read_vector3(&raw[0..12]);
        let max = read_vector3(&raw[12..24]);

        Ok(Self {
            space_bounds: BoundBox::new(min, max),
        })
    }

    /// `CompiledSpaceSettings::boundingBox` (`compiled_space_settings.cpp:76-79`).
    pub fn bounding_box(&self) -> &BoundBox {
        &self.space_bounds
    }
}

/// Three little-endian f32s, x/y/z (`Vector3::operator<<` stream order).
fn read_vector3(bytes: &[u8]) -> Vector3 {
    Vector3 {
        x: f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        y: f32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
        z: f32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn container(section_magic: Magic, version: u32, payload: &[u8]) -> BinaryFormat {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"BWTB");
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&(48u32 + payload.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&0u64.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&section_magic);
        bytes.extend_from_slice(&version.to_le_bytes());
        bytes.extend_from_slice(&48u64.to_le_bytes());
        bytes.extend_from_slice(&(payload.len() as u64).to_le_bytes());
        bytes.extend_from_slice(payload);
        BinaryFormat::open("test.bin", bytes).unwrap()
    }

    fn settings_payload(min: [f32; 3], max: [f32; 3]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&24u32.to_le_bytes());
        for v in min.iter().chain(max.iter()) {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out
    }

    #[test]
    fn read_parses_the_space_bounds() {
        let reader = container(
            FORMAT_MAGIC,
            FORMAT_VERSION,
            &settings_payload([-50.0, -10.0, -50.0], [50.0, 30.0, 50.0]),
        );
        let settings = CompiledSpaceSettings::read(&reader).unwrap();
        let bounds = settings.bounding_box();
        assert_eq!(
            bounds.min,
            Vector3 {
                x: -50.0,
                y: -10.0,
                z: -50.0
            }
        );
        assert_eq!(
            bounds.max,
            Vector3 {
                x: 50.0,
                y: 30.0,
                z: 50.0
            }
        );
    }

    #[test]
    fn read_reports_a_missing_section_with_both_messages() {
        let reader = container(*b"XXXX", 1, &[0; 28]);
        let err = CompiledSpaceSettings::read(&reader).unwrap_err();
        assert_eq!(
            err,
            "'test.bin' does not contain CompiledSpaceSettings.\n\
             Failed to map compiled space settings into memory.\n"
        );
    }

    #[test]
    fn read_reports_a_wrong_version_with_both_messages() {
        let reader = container(FORMAT_MAGIC, 2, &[0; 28]);
        let err = CompiledSpaceSettings::read(&reader).unwrap_err();
        assert_eq!(
            err,
            "'test.bin' has the wrong version of CompiledSpaceSettings (expected 2, got 1).\n\
             Failed to map compiled space settings into memory.\n"
        );
    }

    #[test]
    fn read_reports_a_malformed_header() {
        // The typeSize prefix says something other than 24.
        let mut payload = Vec::new();
        payload.extend_from_slice(&23u32.to_le_bytes());
        payload.extend_from_slice(&[0u8; 23]);
        let reader = container(FORMAT_MAGIC, FORMAT_VERSION, &payload);
        let err = CompiledSpaceSettings::read(&reader).unwrap_err();
        assert_eq!(err, "CompiledSpaceSettings in 'test.bin' is malformed.\n");
    }

    #[test]
    fn read_reports_a_truncated_header() {
        // The prefix is right but the payload ends early.
        let mut payload = Vec::new();
        payload.extend_from_slice(&24u32.to_le_bytes());
        payload.extend_from_slice(&[0u8; 12]);
        let reader = container(FORMAT_MAGIC, FORMAT_VERSION, &payload);
        let err = CompiledSpaceSettings::read(&reader).unwrap_err();
        assert_eq!(err, "CompiledSpaceSettings in 'test.bin' is malformed.\n");
    }
}
