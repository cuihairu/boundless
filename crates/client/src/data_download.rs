//! Resource download segment reassembly (BW-11-03-05).
//!
//! A server-initiated resource download (`addProxyData` / `addProxyFileData`)
//! arrives as a header plus a stream of fragments. `DataDownload`
//! (`lib/connection/data_download.{hpp,cpp}`) is the record that collects a
//! transfer's `DownloadSegment`s (`lib/connection/download_segment.hpp`), and
//! `ServerConnection::resourceHeader` / `resourceFragment`
//! (`lib/connection/server_connection.cpp:2839-2925`) are the two wire
//! messages that drive it.
//!
//! `DataDownload::insert` (`data_download.cpp:27-34`) appends each segment in
//! *arrival order* (a `push_back` on the list — the "sorted fashion" comment
//! notwithstanding, the `seq_` numbers are not used to order the write). A
//! download `complete()`s (`:41-44`) only when *both* the final segment and
//! the description have arrived, in any order. `write()` (`:51-59`)
//! concatenates the segment payloads in insertion order. So the reassembled
//! content and the completion callback order both follow arrival — that is
//! the acceptance.
//!
//! `resourceHeader` (`server_connection.cpp:2839-2868`) reads the id (u16) and
//! the description; a reused id is an "ID collision" — the new header is
//! dropped with an ERROR, keeping the old record. `resourceFragment`
//! (`:2874-2925`) reads `ResourceFragmentArgs` (`client_interface.hpp:162-165`:
//! `rid` u16, `seq` u8, `flags` u8) then the payload, creates the record if a
//! fragment arrived before its header (accepted as an orphan), and on
//! completion erases the record and invokes `onStreamComplete(id, desc, data)`.
//!
//! The acceptance mirrors `lib/network/unit_test/test_data_download.cpp`:
//! segments alone are incomplete, a final segment without a description is
//! incomplete, the description may arrive after the segments, `write()`
//! concatenates in insertion order, and two downloads complete independently.

use std::collections::BTreeMap;

/// One chunk of a `DataDownload` (`DownloadSegment`,
/// `lib/connection/download_segment.hpp:13-33`): a sequence number and the
/// payload bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadSegment {
    /// `seq_` (`download_segment.hpp:29`) — carried but not used for ordering.
    pub seq: u8,
    /// `data_`.
    pub data: Vec<u8>,
}

impl DownloadSegment {
    /// `DownloadSegment(data, len, seq)` (`download_segment.hpp:16-20`).
    pub fn new(data: Vec<u8>, seq: u8) -> Self {
        Self { seq, data }
    }
}

/// The record of one in-flight transfer (`DataDownload`,
/// `lib/connection/data_download.hpp:20-48`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataDownload {
    /// `id_`.
    id: u16,
    /// `pDesc_` (`None` until `setDesc`).
    desc: Option<String>,
    /// `hasLast_` — set when the final segment was inserted.
    has_last: bool,
    /// The segments in arrival (insertion) order (`BW::list<DownloadSegment*>`).
    segments: Vec<DownloadSegment>,
}

impl DataDownload {
    /// `DataDownload(uint16 id)` (`data_download.hpp:23-27`).
    pub fn new(id: u16) -> Self {
        Self {
            id,
            desc: None,
            has_last: false,
            segments: Vec::new(),
        }
    }

    /// `id()` (`data_download.hpp:34`).
    pub fn id(&self) -> u16 {
        self.id
    }

    /// `pDesc()` (`data_download.hpp:35`).
    pub fn desc(&self) -> Option<&str> {
        self.desc.as_deref()
    }

    /// The segment count (the test's `download.size()`).
    pub fn size(&self) -> usize {
        self.segments.len()
    }

    /// `empty()`.
    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    /// `insert(pSegment, isLast)` (`data_download.cpp:27-34`): a final segment
    /// sets `hasLast_`, and the segment is appended in arrival order.
    pub fn insert(&mut self, segment: DownloadSegment, is_last: bool) {
        if is_last {
            self.has_last = true;
        }
        self.segments.push(segment);
    }

    /// `complete()` (`data_download.cpp:41-44`): both the final segment and the
    /// description arrived.
    pub fn complete(&self) -> bool {
        self.has_last && self.desc.is_some()
    }

    /// `setDesc` (`data_download.cpp:65-69`): store the description. It may
    /// arrive before or after the segments.
    pub fn set_desc(&mut self, desc: String) {
        self.desc = Some(desc);
    }

    /// `write(os)` (`data_download.cpp:51-59`): concatenate the segment
    /// payloads in insertion order. The reference `MF_ASSERT_DEV(complete())`
    /// is a `debug_assert` here.
    pub fn write(&self) -> Vec<u8> {
        debug_assert!(self.complete(), "write() on an incomplete download");
        let mut out = Vec::new();
        for segment in &self.segments {
            out.extend_from_slice(&segment.data);
        }
        out
    }
}

/// Why a resource-download wire message was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadError {
    /// The message ended before its fixed fields could be read.
    ShortStream,
    /// `resourceHeader`: "Collision between new and existing download IDs" —
    /// the id is already in use, so the new header is dropped
    /// (`server_connection.cpp:2860-2863`).
    Collision(u16),
}

/// A completed download, as handed to `onStreamComplete`
/// (`server_connection.cpp:2915-2921`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamComplete {
    /// The transfer id.
    pub id: u16,
    /// The description.
    pub desc: String,
    /// The reassembled payload in arrival order.
    pub data: Vec<u8>,
}

/// The `dataDownloads_` side of `ServerConnection`
/// (`server_connection.cpp:2839-2925`): the in-flight record map plus the
/// completed-download callback log. The record map is keyed by id, so a second
/// download of a distinct id completes independently of the first.
#[derive(Debug, Clone, Default)]
pub struct ResourceDownloader {
    /// `dataDownloads_`.
    downloads: BTreeMap<u16, DataDownload>,
    /// The `onStreamComplete` calls, in completion order.
    completed: Vec<StreamComplete>,
}

impl ResourceDownloader {
    pub fn new() -> Self {
        Self::default()
    }

    /// `ServerConnection::resourceHeader` (`server_connection.cpp:2839-2868`):
    /// reads `[id u16][desc packed-string]`; a reused id is a collision (the
    /// header is dropped, the old record kept).
    pub fn resource_header(&mut self, stream: &[u8]) -> Result<(), DownloadError> {
        let mut c = Cursor::new(stream);
        let id = c.u16().ok_or(DownloadError::ShortStream)?;
        let desc = c.string().ok_or(DownloadError::ShortStream)?;
        if self.downloads.contains_key(&id) {
            return Err(DownloadError::Collision(id));
        }
        let mut download = DataDownload::new(id);
        download.set_desc(desc);
        self.downloads.insert(id, download);
        Ok(())
    }

    /// `ServerConnection::resourceFragment` (`server_connection.cpp:2874-2925`):
    /// reads `[rid u16][seq u8][flags u8][data…]`; a fragment before its header
    /// creates an orphan record; on completion the record is erased and the
    /// download handed to `onStreamComplete`.
    pub fn resource_fragment(&mut self, stream: &[u8]) -> Result<(), DownloadError> {
        let mut c = Cursor::new(stream);
        let rid = c.u16().ok_or(DownloadError::ShortStream)?;
        let seq = c.u8().ok_or(DownloadError::ShortStream)?;
        let flags = c.u8().ok_or(DownloadError::ShortStream)?;
        let data = c.rest().to_vec();

        {
            let download = self
                .downloads
                .entry(rid)
                .or_insert_with(|| DataDownload::new(rid));
            // `args.flags == 1` marks the final segment (`:2907`).
            download.insert(DownloadSegment::new(data, seq), flags == 1);
        }

        if self.downloads.get(&rid).is_some_and(DataDownload::complete) {
            let download = self.downloads.remove(&rid).expect("just checked");
            self.completed.push(StreamComplete {
                id: download.id(),
                desc: download.desc().unwrap_or_default().to_string(),
                data: download.write(),
            });
        }
        Ok(())
    }

    /// The completed downloads, in completion order.
    pub fn completed(&self) -> &[StreamComplete] {
        &self.completed
    }

    /// The in-flight records, keyed by id.
    pub fn active(&self) -> &BTreeMap<u16, DataDownload> {
        &self.downloads
    }
}

/// A cursor with the wire read helpers the two handlers use. The packed string
/// is `BinaryIStream::operator>>(BW::string&)`: a `readPackedInt` length
/// prefix (`binary_stream.hpp:287-296`, 1 byte below 255 else `0xFF` + 3
/// little-endian bytes) then the bytes.
struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.data.len() - self.pos < n {
            return None;
        }
        let out = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Some(out)
    }
    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }
    fn u16(&mut self) -> Option<u16> {
        self.take(2).map(|b| u16::from_le_bytes([b[0], b[1]]))
    }
    fn rest(&mut self) -> &'a [u8] {
        let out = &self.data[self.pos..];
        self.pos = self.data.len();
        out
    }
    fn packed_len(&mut self) -> Option<usize> {
        let first = self.u8()?;
        if first == 0xFF {
            let bytes = self.take(3)?;
            Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], 0]) as usize)
        } else {
            Some(first as usize)
        }
    }
    fn string(&mut self) -> Option<String> {
        let len = self.packed_len()?;
        let bytes = self.take(len)?;
        Some(String::from_utf8_lossy(bytes).into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialise a `BW::string` with a packed length prefix.
    fn packed_string(text: &str) -> Vec<u8> {
        let mut b = Vec::new();
        assert!(text.len() < 255, "test string small");
        b.push(text.len() as u8);
        b.extend_from_slice(text.as_bytes());
        b
    }

    /// A `resourceHeader` stream: `[id u16][desc packed-string]`.
    fn header_stream(id: u16, desc: &str) -> Vec<u8> {
        let mut b = id.to_le_bytes().to_vec();
        b.extend_from_slice(&packed_string(desc));
        b
    }

    /// A `resourceFragment` stream: `[rid u16][seq u8][flags u8][data…]`.
    fn fragment_stream(rid: u16, seq: u8, is_last: bool, data: &[u8]) -> Vec<u8> {
        let mut b = rid.to_le_bytes().to_vec();
        b.push(seq);
        b.push(if is_last { 1 } else { 0 });
        b.extend_from_slice(data);
        b
    }

    /// The acceptance: a download completes only when both the description and
    /// the final segment have arrived, in any order
    /// (`test_data_download.cpp` `DataDownload_segmentsAndCompletion`).
    #[test]
    fn a_download_completes_only_with_desc_and_last_segment() {
        let mut download = DataDownload::new(7);
        assert_eq!(download.id(), 7);
        assert!(download.is_empty());
        assert_eq!(download.desc(), None);
        assert!(!download.complete());

        // Segments alone are not enough.
        download.insert(DownloadSegment::new(b"abc".to_vec(), 0), false);
        assert!(!download.complete());

        // The final segment without a description is not enough either.
        download.insert(DownloadSegment::new(b"de".to_vec(), 1), true);
        assert!(!download.complete());

        // The description completes it — it may arrive after the segments.
        download.set_desc("my download".to_string());
        assert!(download.complete());
        assert_eq!(download.desc(), Some("my download"));
        assert_eq!(download.size(), 2);
    }

    /// `write()` concatenates the payloads in *insertion order*, not `seq`
    /// order (`DataDownload_writeOrderIsInsertionOrder`).
    #[test]
    fn write_is_in_insertion_order_not_seq_order() {
        let mut download = DataDownload::new(1);
        download.insert(DownloadSegment::new(b"second".to_vec(), 1), false);
        download.insert(DownloadSegment::new(b"first...".to_vec(), 0), false);
        download.insert(DownloadSegment::new(b"!".to_vec(), 2), true);
        download.set_desc("desc".to_string());
        assert!(download.complete());
        assert_eq!(download.write(), b"secondfirst...!".to_vec());

        // A second download completes the same way.
        let mut other = DataDownload::new(2);
        other.insert(DownloadSegment::new(b"x".to_vec(), 0), true);
        other.set_desc("d2".to_string());
        assert!(other.complete());
        assert_eq!(other.write(), b"x".to_vec());
    }

    /// Two downloads of distinct ids complete independently.
    #[test]
    fn two_downloads_complete_independently() {
        let mut a = DataDownload::new(10);
        let mut b = DataDownload::new(11);
        a.insert(DownloadSegment::new(b"aaa".to_vec(), 0), true);
        a.set_desc("a".to_string());
        assert!(a.complete());
        assert!(!b.complete());
        b.insert(DownloadSegment::new(b"bbb".to_vec(), 0), true);
        b.set_desc("b".to_string());
        assert!(b.complete());
    }

    /// The full wire path: a header then the fragments reassembles the content
    /// in fragment arrival order and fires one completion
    /// (`server_connection.cpp:2874-2925`).
    #[test]
    fn header_then_fragments_produce_one_completion() {
        let mut dl = ResourceDownloader::new();
        dl.resource_header(&header_stream(7, "my download"))
            .expect("header");
        dl.resource_fragment(&fragment_stream(7, 0, false, b"abc"))
            .expect("frag 0");
        assert!(dl.completed().is_empty(), "not complete until the last");
        dl.resource_fragment(&fragment_stream(7, 1, true, b"de"))
            .expect("frag 1");
        assert_eq!(
            dl.completed(),
            &[StreamComplete {
                id: 7,
                desc: "my download".to_string(),
                data: b"abcde".to_vec(),
            }],
            "the reassembled content is in fragment order"
        );
        assert!(dl.active().is_empty(), "the record is erased on completion");
    }

    /// A fragment before its header creates an orphan record; the later header
    /// collides and is dropped, so it never completes
    /// (`server_connection.cpp:2888-2899`, `:2860-2863`).
    #[test]
    fn a_fragment_before_the_header_leaves_an_orphan() {
        let mut dl = ResourceDownloader::new();
        dl.resource_fragment(&fragment_stream(7, 0, true, b"abc"))
            .expect("orphan fragment");
        assert!(dl.active().contains_key(&7));
        assert!(dl.completed().is_empty());

        // The late header collides with the orphan and is dropped.
        assert_eq!(
            dl.resource_header(&header_stream(7, "late")),
            Err(DownloadError::Collision(7))
        );
        assert!(dl.completed().is_empty(), "no completion without a desc");
        assert_eq!(dl.active().get(&7).unwrap().desc(), None);
    }

    /// A reused id after a completed transfer starts a fresh download.
    #[test]
    fn an_id_is_reusable_after_completion() {
        let mut dl = ResourceDownloader::new();
        dl.resource_header(&header_stream(7, "first")).expect("h1");
        dl.resource_fragment(&fragment_stream(7, 0, true, b"x"))
            .expect("f1");
        assert_eq!(dl.completed().len(), 1);

        dl.resource_header(&header_stream(7, "second")).expect("h2");
        dl.resource_fragment(&fragment_stream(7, 0, true, b"y"))
            .expect("f2");
        assert_eq!(dl.completed().len(), 2);
        assert_eq!(dl.completed()[1].data, b"y".to_vec());
    }

    /// A truncated header or fragment fails cleanly.
    #[test]
    fn a_short_stream_is_rejected() {
        let mut dl = ResourceDownloader::new();
        assert_eq!(
            dl.resource_header(&[0x07, 0x00]), // no desc
            Err(DownloadError::ShortStream)
        );
        assert_eq!(
            dl.resource_fragment(&[0x07, 0x00, 0x00]), // no flags
            Err(DownloadError::ShortStream)
        );
    }
}
