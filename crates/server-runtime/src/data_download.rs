//! BW-06-04-05 the client download stream.
//!
//! `ServerConnection::resourceHeader` / `resourceFragment`
//! (`lib/connection/server_connection.cpp`) collect the fragments the server
//! streams for a requested resource and hand the concatenated data to the
//! message handler's `onStreamComplete` once both the header and the final
//! fragment have arrived. `DataDownload` (`lib/connection/data_download.cpp`)
//! stores the segments in arrival order; a header whose id is already in use is
//! dropped (the old record is left to finish), and a fragment for an unknown id
//! creates a record (the reference accepts it silently).
//!
//! The wire layout matches `client_interface.hpp`:
//! * the header is the id then a description — a `readPackedInt` length plus
//!   that many bytes (`binary_stream.hpp`, `0xFF` + 3 little-endian bytes for
//!   lengths of 255 and above);
//! * a fragment is `rid`, `seq`, `flags` (the final segment has `flags == 1`,
//!   which the server sends as its `done()` flag), then the payload.
//!
//! The `seq` field is read but unused: the reference stores it on the segment
//! yet appends in arrival order, so the concatenation ignores ordering.

use std::collections::HashMap;

/// The download id (`ResourceFragmentArgs::rid`).
pub type DownloadId = u16;

/// A completed download handed to `onStreamComplete`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamComplete {
    /// The download id.
    pub id: DownloadId,
    /// The header description (raw bytes, as `BW::string`).
    pub description: Vec<u8>,
    /// The concatenated segment payloads.
    pub data: Vec<u8>,
}

/// The outcome of feeding a header or a fragment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DownloadEvent {
    /// The header id was already in use; the header was dropped.
    Collision { id: DownloadId },
    /// The download completed.
    Complete(StreamComplete),
}

/// The client-side download collector (`dataDownloads_`).
#[derive(Debug, Default)]
pub struct DataDownloads {
    downloads: HashMap<DownloadId, DataDownload>,
}

/// One in-progress download (`DataDownload`).
#[derive(Debug, Default)]
struct DataDownload {
    description: Option<Vec<u8>>,
    segments: Vec<Vec<u8>>,
    has_last: bool,
}

impl DataDownloads {
    /// `resourceHeader`: register the id and read the description.
    pub fn resource_header(&mut self, id: DownloadId, payload: &[u8]) -> Option<DownloadEvent> {
        if self.downloads.contains_key(&id) {
            return Some(DownloadEvent::Collision { id });
        }
        let mut reader = StreamReader::new(payload);
        let description = read_packed_string(&mut reader).unwrap_or_default();
        self.downloads.insert(
            id,
            DataDownload {
                description: Some(description),
                segments: Vec::new(),
                has_last: false,
            },
        );
        self.finish_if_complete(id)
    }

    /// `resourceFragment`: append the payload and mark the last segment.
    ///
    /// The reference reads `seq` but never uses it to order the segments, so
    /// it is accepted and ignored here too.
    pub fn resource_fragment(
        &mut self,
        rid: DownloadId,
        _seq: u8,
        flags: u8,
        payload: &[u8],
    ) -> Option<DownloadEvent> {
        let entry = self.downloads.entry(rid).or_default();
        entry.segments.push(payload.to_vec());
        if flags == 1 {
            entry.has_last = true;
        }
        self.finish_if_complete(rid)
    }

    /// `complete()`: the header and the final segment have both arrived.
    fn finish_if_complete(&mut self, id: DownloadId) -> Option<DownloadEvent> {
        let complete = self
            .downloads
            .get(&id)
            .map(|d| d.has_last && d.description.is_some())
            .unwrap_or(false);
        if !complete {
            return None;
        }
        let download = self.downloads.remove(&id).unwrap();
        Some(DownloadEvent::Complete(StreamComplete {
            id,
            description: download.description.unwrap_or_default(),
            data: download.segments.concat(),
        }))
    }

    /// `clearDownloads`: drop all in-progress records (on disconnect).
    pub fn clear(&mut self) {
        self.downloads.clear();
    }

    /// The number of in-progress downloads.
    pub fn len(&self) -> usize {
        self.downloads.len()
    }

    /// Whether there are no in-progress downloads.
    pub fn is_empty(&self) -> bool {
        self.downloads.is_empty()
    }
}

/// A read cursor over the header or fragment payload.
struct StreamReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> StreamReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn read_u8(&mut self) -> Option<u8> {
        let byte = self.data.get(self.pos).copied();
        if byte.is_some() {
            self.pos += 1;
        }
        byte
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let slice = self.data.get(self.pos..end)?;
        self.pos = end;
        Some(slice)
    }
}

/// `readPackedInt`: one byte, or `0xFF` followed by three little-endian bytes.
fn read_packed_int(reader: &mut StreamReader<'_>) -> Option<i32> {
    let first = reader.read_u8()?;
    if first == 0xff {
        let b0 = reader.read_u8()? as i32;
        let b1 = reader.read_u8()? as i32;
        let b2 = reader.read_u8()? as i32;
        Some(b0 | (b1 << 8) | (b2 << 16))
    } else {
        Some(first as i32)
    }
}

/// The `BW::string` operator: a packed-int length, then that many bytes.
fn read_packed_string(reader: &mut StreamReader<'_>) -> Option<Vec<u8>> {
    let length = read_packed_int(reader)?;
    if length < 0 {
        return Some(Vec::new());
    }
    reader.take(length as usize).map(|bytes| bytes.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header_payload(description: &[u8]) -> Vec<u8> {
        let mut payload = vec![description.len() as u8];
        payload.extend_from_slice(description);
        payload
    }

    #[test]
    fn fragments_accumulate_until_the_last_segment() {
        let mut downloads = DataDownloads::default();
        assert!(downloads.is_empty());

        // The header registers the download but it is not complete yet.
        assert_eq!(
            downloads.resource_header(5, &header_payload(b"file.txt")),
            None
        );
        assert_eq!(downloads.len(), 1);

        // Two non-final fragments, then the final one.
        assert_eq!(downloads.resource_fragment(5, 0, 0, &[1, 2, 3]), None);
        assert_eq!(downloads.resource_fragment(5, 1, 0, &[4, 5]), None);
        assert_eq!(
            downloads.resource_fragment(5, 2, 1, &[6]),
            Some(DownloadEvent::Complete(StreamComplete {
                id: 5,
                description: b"file.txt".to_vec(),
                data: vec![1, 2, 3, 4, 5, 6],
            }))
        );
        assert!(downloads.is_empty());
    }

    #[test]
    fn a_fragment_before_the_header_is_accepted_silently() {
        let mut downloads = DataDownloads::default();
        // The reference accepts data for an unknown id, creating the record.
        assert_eq!(downloads.resource_fragment(9, 0, 0, &[7]), None);
        assert_eq!(downloads.len(), 1);
        // The late header collides with the record the fragment created, so it
        // is dropped. The download can never complete without a description and
        // just holds its data until clearDownloads (the reference's wart).
        assert_eq!(
            downloads.resource_header(9, &header_payload(b"late")),
            Some(DownloadEvent::Collision { id: 9 })
        );
        assert_eq!(downloads.resource_fragment(9, 1, 1, &[8]), None);
        assert_eq!(downloads.len(), 1);
        downloads.clear();
        assert!(downloads.is_empty());
    }

    #[test]
    fn a_colliding_header_is_dropped() {
        let mut downloads = DataDownloads::default();
        downloads.resource_header(5, &header_payload(b"first"));
        // A second header with the same id is dropped; the first continues.
        assert_eq!(
            downloads.resource_header(5, &header_payload(b"second")),
            Some(DownloadEvent::Collision { id: 5 })
        );
        assert_eq!(
            downloads.resource_fragment(5, 0, 1, &[1, 2]),
            Some(DownloadEvent::Complete(StreamComplete {
                id: 5,
                description: b"first".to_vec(),
                data: vec![1, 2],
            }))
        );
    }

    #[test]
    fn a_last_segment_before_the_header_does_not_complete() {
        let mut downloads = DataDownloads::default();
        downloads.resource_fragment(3, 0, 1, &[9]);
        assert_eq!(downloads.len(), 1);
        // The late header collides with the record the fragment created, so it
        // is dropped and the download stays incomplete.
        assert_eq!(
            downloads.resource_header(3, &header_payload(b"tail")),
            Some(DownloadEvent::Collision { id: 3 })
        );
        assert_eq!(downloads.len(), 1);
    }

    #[test]
    fn clear_drops_in_progress_downloads() {
        let mut downloads = DataDownloads::default();
        downloads.resource_header(1, &header_payload(b"a"));
        downloads.resource_fragment(1, 0, 0, &[1]);
        downloads.clear();
        assert!(downloads.is_empty());
    }

    #[test]
    fn a_long_description_uses_the_packed_three_byte_length() {
        let description = vec![b'x'; 300];
        // 0xFF marks a 3-byte little-endian length (300 = 0x2c, 0x01, 0x00).
        let mut payload = vec![0xff, 0x2c, 0x01, 0x00];
        payload.extend_from_slice(&description);

        let mut downloads = DataDownloads::default();
        assert_eq!(downloads.resource_header(7, &payload), None);
        assert_eq!(
            downloads.resource_fragment(7, 0, 1, &[0]),
            Some(DownloadEvent::Complete(StreamComplete {
                id: 7,
                description,
                data: vec![0],
            }))
        );
    }

    #[test]
    fn read_packed_int_decodes_both_forms() {
        let mut reader = StreamReader::new(&[0x2a]);
        assert_eq!(read_packed_int(&mut reader), Some(42));

        let mut reader = StreamReader::new(&[0xff, 0x2c, 0x01, 0x00]);
        assert_eq!(read_packed_int(&mut reader), Some(300));
    }
}
