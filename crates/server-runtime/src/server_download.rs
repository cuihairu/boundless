//! BW-07-05-01: stream client resources in chunks.
//!
//! `DataDownloads` / `DataDownload` (`baseapp/data_downloads.cpp`) send a
//! resource the server has queued for a client as a stream of
//! `resourceHeader` + `resourceFragment` messages, at most a bundle's worth
//! (`addToBundle`, `data_downloads.cpp:105`). Each download sends its
//! description header once, then the payload in fragments; the final fragment
//! carries the `done` flag so the client assembles the stream
//! (`data_download.cpp`). A download that is done, or that has failed, is
//! dropped and reported through `IFinishedDownloads::onFinished` — `true` on
//! completion, `false` on failure. IDs are handed out from `freeID_`,
//! auto-assigned when the caller passes `uint16(-1)`, and the lowest freed id
//! is reclaimed on erase (`push_back`, `:149`; `erase`, `:191`). A download
//! onloaded from another BaseApp is inserted already-failed so its first send
//! reports failure (`FailedDataDownload`, `:30`; the stream `operator>>`,
//! `:316`).
//!
//! This module models the server's download queue and the chunking of the
//! string payload (the file variant streams from a loading thread and is not
//! modelled). The chunks it produces feed the client-side collector in
//! [`crate::data_download`], so block order and the completion callback can be
//! verified together.

use crate::data_download::DownloadId;
use std::collections::{BTreeSet, VecDeque};

/// `MAX_CONCURRENT_DOWNLOADS` (`data_downloads.cpp:19`).
const MAX_CONCURRENT_DOWNLOADS: usize = 0xfffe;

/// The wire chunks a download queues for a bundle
/// (`DataDownload::addToBundle`, `data_downloads.cpp:422`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerChunk {
    /// The `resourceHeader` message: the id and the description string.
    Header {
        /// The download id.
        id: DownloadId,
        /// The description, sent once.
        description: Vec<u8>,
    },
    /// The `resourceFragment` message: the id, the sequence, the `done` flag
    /// and the payload segment.
    Fragment {
        /// The download id.
        id: DownloadId,
        /// The segment sequence (`seq_++`).
        seq: u8,
        /// Whether this is the final segment (`uint8(done())`).
        done: bool,
        /// The payload bytes.
        payload: Vec<u8>,
    },
}

/// A download that finished, reported to `IFinishedDownloads::onFinished`
/// (`data_downloads.cpp:121`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DownloadFinish {
    /// The download id.
    pub id: DownloadId,
    /// Whether the download completed (`true`) or failed (`false`).
    pub success: bool,
}

/// What [`ServerDownloads::add_to_bundle`] produced.
#[derive(Debug, Clone, Default)]
pub struct AddResult {
    /// The chunks to put on the bundle, in order.
    pub chunks: Vec<ServerChunk>,
    /// The downloads that finished (or failed) in this call, in order.
    pub finished: Vec<DownloadFinish>,
}

/// One queued download (`DataDownload`, `data_downloads.hpp:76`).
#[derive(Debug, Clone)]
struct Download {
    /// `id_`.
    id: DownloadId,
    /// `seq_`.
    seq: u8,
    /// `pDesc_`, `None` once the header has been sent.
    description: Option<Vec<u8>>,
    /// The payload still to send.
    data: Vec<u8>,
    /// `bytesSent_`.
    sent: usize,
    /// `good_`.
    good: bool,
}

impl Download {
    fn new(id: DownloadId, description: Vec<u8>, data: Vec<u8>) -> Self {
        Download {
            id,
            seq: 0,
            description: Some(description),
            data,
            sent: 0,
            good: true,
        }
    }

    fn failed(id: DownloadId) -> Self {
        Download {
            id,
            seq: 0,
            description: None,
            data: Vec::new(),
            sent: 0,
            good: false,
        }
    }

    /// `available()` (`data_downloads.cpp:103`).
    fn available(&self) -> usize {
        self.data.len() - self.sent
    }

    /// `done()` (`data_downloads.cpp:89`).
    fn done(&self) -> bool {
        self.available() == 0
    }

    /// `DataDownload::addToBundle` (`data_downloads.cpp:422`): queue as much of
    /// this download as fits in `remaining`, reducing it by what was queued.
    /// The chunks go straight into the caller's vector: the drained download
    /// needs no staging buffer of its own, so a stampede-sized call costs one
    /// fewer allocation and copy per download (OPT-06-01-03).
    fn add_to_bundle_into(&mut self, chunks: &mut Vec<ServerChunk>, remaining: &mut i32) {
        if let Some(description) = self.description.take() {
            // The header: id plus the length-prefixed description. The
            // reference charges `sizeof(uint16) + 1 + descLen`
            // (`data_downloads.cpp:442`); transport header sizes are not
            // modelled.
            let cost = 2 + 1 + description.len() as i32;
            chunks.push(ServerChunk::Header {
                id: self.id,
                description,
            });
            *remaining -= cost;
            if *remaining <= 0 {
                return;
            }
        }

        if self.available() == 0 {
            return;
        }

        // The fragment: id, seq, done flag, then the payload
        // (`data_downloads.cpp:468`, `:482`).
        *remaining -= 4;
        if *remaining <= 0 {
            return;
        }

        let payload_size = std::cmp::min(*remaining as usize, self.available());
        *remaining -= payload_size as i32;

        let seq = self.seq;
        self.seq = self.seq.wrapping_add(1);
        let payload = self.read(payload_size);
        let done = self.done();

        chunks.push(ServerChunk::Fragment {
            id: self.id,
            seq,
            done,
            payload,
        });
    }

    /// `read` (`data_downloads.cpp:85`): copy `nBytes` out of the payload.
    fn read(&mut self, n_bytes: usize) -> Vec<u8> {
        let take = std::cmp::min(n_bytes, self.available());
        let bytes = self.data[self.sent..self.sent + take].to_vec();
        self.sent += take;
        bytes
    }
}

/// The server's download queue (`DataDownloads`, `data_downloads.cpp:34`).
#[derive(Debug, Clone, Default)]
pub struct ServerDownloads {
    /// `dls_`, the active downloads in arrival order.
    downloads: VecDeque<Download>,
    /// `freeID_`, the next id to hand out.
    free_id: DownloadId,
    /// `usedIDs_`.
    used_ids: BTreeSet<DownloadId>,
}

impl ServerDownloads {
    /// An empty queue.
    pub fn new() -> Self {
        Self::default()
    }

    /// `DataDownloads::contains` (`data_downloads.cpp:216`).
    pub fn contains(&self, id: DownloadId) -> bool {
        id != u16::MAX && self.used_ids.contains(&id)
    }

    /// `DataDownloads::empty` (`data_downloads.cpp:228`).
    pub fn is_empty(&self) -> bool {
        self.used_ids.is_empty()
    }

    /// The number of active downloads.
    pub fn len(&self) -> usize {
        self.downloads.len()
    }

    /// The next id to hand out (`freeID_`).
    pub fn free_id(&self) -> DownloadId {
        self.free_id
    }

    /// The ids currently in use, in order.
    pub fn used_ids(&self) -> &BTreeSet<DownloadId> {
        &self.used_ids
    }

    /// `streamToClient` / `push_back` (`data_downloads.cpp:149`): queue a
    /// string download. `requested_id` of `uint16(-1)` auto-assigns the next
    /// free id. Returns the assigned id, or `None` if the id is in use or the
    /// queue is full.
    pub fn stream(
        &mut self,
        description: Vec<u8>,
        data: Vec<u8>,
        requested_id: DownloadId,
    ) -> Option<DownloadId> {
        if self.downloads.len() >= MAX_CONCURRENT_DOWNLOADS {
            return None;
        }
        let id = if requested_id == u16::MAX {
            self.free_id
        } else {
            requested_id
        };
        if id != u16::MAX && self.used_ids.contains(&id) {
            return None;
        }
        self.downloads
            .push_back(Download::new(id, description, data));
        self.used_ids.insert(id);
        if id == self.free_id {
            self.advance_free_id(id);
        }
        Some(id)
    }

    /// The `operator>>` onload (`data_downloads.cpp:316`): every id streamed in
    /// is inserted already-failed, so its first send reports failure.
    pub fn restore_failed(&mut self, ids: Vec<DownloadId>) {
        for id in ids {
            self.downloads.push_back(Download::failed(id));
            self.used_ids.insert(id);
            if id == self.free_id {
                self.advance_free_id(id);
            }
        }
    }

    /// The ids to stream for an onload (`operator<<`, `data_downloads.cpp:295`).
    pub fn snapshot_ids(&self) -> Vec<DownloadId> {
        self.used_ids.iter().copied().collect()
    }

    /// `DataDownloads::addToBundle` (`data_downloads.cpp:105`): queue as much of
    /// every download as fits in the `remaining` budget, reporting the ones
    /// that finish or fail.
    pub fn add_to_bundle(&mut self, mut remaining: i32) -> AddResult {
        let mut result = AddResult::default();
        let mut index = 0;

        while index < self.downloads.len() && remaining > 0 {
            if !self.downloads[index].good {
                let id = self.downloads[index].id;
                result.finished.push(DownloadFinish { id, success: false });
                self.remove_at(index);
                continue;
            }

            self.downloads[index].add_to_bundle_into(&mut result.chunks, &mut remaining);

            if self.downloads[index].done() {
                let id = self.downloads[index].id;
                result.finished.push(DownloadFinish { id, success: true });
                self.remove_at(index);
                continue;
            }

            index += 1;
        }
        result
    }

    /// `DataDownloads::abortDownloads` (`data_downloads.cpp:238`): report every
    /// download as failed and reset to the pristine state.
    pub fn abort(&mut self) -> Vec<DownloadFinish> {
        let finished = self
            .downloads
            .drain(..)
            .map(|download| DownloadFinish {
                id: download.id,
                success: false,
            })
            .collect();
        self.used_ids.clear();
        self.free_id = 0;
        finished
    }

    /// Reclaim an id and drop a download (`erase`, `data_downloads.cpp:191`).
    fn remove_at(&mut self, index: usize) {
        let download = self.downloads.remove(index).expect("index in range");
        self.used_ids.remove(&download.id);
        if self.free_id > download.id {
            self.free_id = download.id;
        }
    }

    /// Pick the next unused id from `assigned` (`push_back`,
    /// `data_downloads.cpp:170`).
    fn advance_free_id(&mut self, assigned: DownloadId) {
        for i in 1..MAX_CONCURRENT_DOWNLOADS as u16 {
            self.free_id = assigned.wrapping_add(i);
            if !self.used_ids.contains(&self.free_id) {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data_download::{DataDownloads, DownloadEvent, StreamComplete};

    /// The `BW::string` prefix the header payload starts with.
    fn packed_string(bytes: &[u8]) -> Vec<u8> {
        let mut payload = Vec::new();
        if bytes.len() < 255 {
            payload.push(bytes.len() as u8);
        } else {
            payload.push(0xff);
            payload.push((bytes.len() & 0xff) as u8);
            payload.push(((bytes.len() >> 8) & 0xff) as u8);
            payload.push(((bytes.len() >> 16) & 0xff) as u8);
        }
        payload.extend_from_slice(bytes);
        payload
    }

    /// Feed a chunk batch into the client-side collector.
    fn feed(chunks: &[ServerChunk], client: &mut DataDownloads) -> Vec<DownloadEvent> {
        let mut events = Vec::new();
        for chunk in chunks {
            match chunk {
                ServerChunk::Header { id, description } => {
                    if let Some(event) = client.resource_header(*id, &packed_string(description)) {
                        events.push(event);
                    }
                }
                ServerChunk::Fragment {
                    id,
                    seq,
                    done,
                    payload,
                } => {
                    if let Some(event) =
                        client.resource_fragment(*id, *seq, u8::from(*done), payload)
                    {
                        events.push(event);
                    }
                }
            }
        }
        events
    }

    /// The chunks stream in order and the final one carries the `done` flag,
    /// so the client collector reassembles the original data.
    #[test]
    fn chunks_reassemble_to_the_original_data() {
        let data = (0..100u8).collect::<Vec<_>>();
        let mut server = ServerDownloads::new();
        let id = server
            .stream(b"file.bin".to_vec(), data.clone(), u16::MAX)
            .unwrap();
        let mut client = DataDownloads::default();
        let mut completed = Vec::new();

        let budget = 16;
        let mut guard = 0;
        loop {
            let result = server.add_to_bundle(budget);
            completed.extend(feed(&result.chunks, &mut client));
            let finished = result.finished.iter().any(|f| f.id == id && f.success);
            guard += 1;
            assert!(guard < 100, "the download must finish");
            if finished {
                break;
            }
        }

        assert_eq!(
            completed,
            vec![DownloadEvent::Complete(StreamComplete {
                id,
                description: b"file.bin".to_vec(),
                data,
            })]
        );
        assert!(server.is_empty());
    }

    /// The fragments carry increasing sequence numbers, the `done` flag only on
    /// the final segment, and the payloads concatenate to the original data.
    #[test]
    fn fragments_stream_in_sequence() {
        let data = vec![1, 2, 3, 4, 5];
        let mut server = ServerDownloads::new();
        let id = server.stream(Vec::new(), data.clone(), u16::MAX).unwrap();

        let mut fragments = Vec::new();
        let mut guard = 0;
        loop {
            let result = server.add_to_bundle(6);
            for chunk in &result.chunks {
                if let ServerChunk::Fragment {
                    seq, done, payload, ..
                } = chunk
                {
                    fragments.push((*seq, *done, payload.clone()));
                }
            }
            let finished = result.finished.iter().any(|f| f.success && f.id == id);
            guard += 1;
            assert!(guard < 100, "the download must finish");
            if finished {
                break;
            }
        }

        assert_eq!(
            fragments
                .iter()
                .map(|(_, done, _)| *done)
                .collect::<Vec<_>>(),
            vec![false, false, true]
        );
        assert_eq!(
            fragments.iter().map(|(seq, _, _)| *seq).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert_eq!(
            fragments
                .iter()
                .flat_map(|f| f.2.clone())
                .collect::<Vec<_>>(),
            data
        );
    }

    /// The download reports completion exactly once, with its id.
    #[test]
    fn completion_reports_the_id_once() {
        let mut server = ServerDownloads::new();
        let id = server.stream(Vec::new(), vec![1, 2], 7).unwrap();
        assert_eq!(id, 7);

        let result = server.add_to_bundle(64);
        assert_eq!(
            result.finished,
            vec![DownloadFinish {
                id: 7,
                success: true
            }]
        );
        assert!(server.is_empty());
    }

    /// An onloaded download is already failed and reports failure on its first
    /// send.
    #[test]
    fn an_onloaded_download_reports_failure() {
        let mut server = ServerDownloads::new();
        server.restore_failed(vec![3, 4]);
        assert!(server.contains(3));
        assert!(server.contains(4));

        let result = server.add_to_bundle(64);
        assert_eq!(
            result.finished,
            vec![
                DownloadFinish {
                    id: 3,
                    success: false
                },
                DownloadFinish {
                    id: 4,
                    success: false
                }
            ]
        );
        assert!(server.is_empty());
    }

    /// IDs are auto-assigned from the free id and the lowest freed id is
    /// reclaimed.
    #[test]
    fn ids_are_auto_assigned_and_reclaimed() {
        let big = vec![9u8; 10000];
        let mut server = ServerDownloads::new();
        let a = server.stream(Vec::new(), vec![1], u16::MAX).unwrap();
        let b = server.stream(Vec::new(), big.clone(), u16::MAX).unwrap();
        let c = server.stream(Vec::new(), big.clone(), u16::MAX).unwrap();
        assert_eq!((a, b, c), (0, 1, 2));
        assert_eq!(server.free_id(), 3);

        // The first download is small enough to finish on this budget; the
        // others still have data left, so id 0 is the lowest freed id.
        let result = server.add_to_bundle(64);
        assert_eq!(
            result.finished,
            vec![DownloadFinish {
                id: 0,
                success: true
            }]
        );
        assert_eq!(server.free_id(), 0);
        assert_eq!(server.len(), 2);

        // The reclaimed id is handed out again.
        assert_eq!(server.stream(Vec::new(), vec![1], u16::MAX), Some(0));
    }

    /// A stampede that finishes every download in one call reports the ids in
    /// queue order and reclaims the lowest one for the next stream.
    #[test]
    fn a_stampede_finishes_in_queue_order_and_reclaims_the_lowest_id() {
        let mut server = ServerDownloads::new();
        let count = 64;
        for _ in 0..count {
            server
                .stream(b"f".to_vec(), vec![7u8; 16], u16::MAX)
                .unwrap();
        }
        assert_eq!(server.len(), count);

        // Header (2 + 1 + 1) plus fragment overhead 4 and payload 16: 24
        // bytes per download.
        let result = server.add_to_bundle((count * 24) as i32);
        assert_eq!(result.finished.len(), count);
        assert_eq!(
            result
                .finished
                .iter()
                .map(|finish| finish.id)
                .collect::<Vec<_>>(),
            (0..count as u16).collect::<Vec<_>>()
        );
        assert!(result.finished.iter().all(|finish| finish.success));
        assert_eq!(result.chunks.len(), count * 2);
        assert!(server.is_empty());
        assert_eq!(server.free_id(), 0);
        assert_eq!(server.stream(Vec::new(), vec![1], u16::MAX), Some(0));
    }

    /// A budget that runs out mid-queue leaves the later downloads untouched:
    /// their headers are not consumed, so the next bundle streams them first.
    #[test]
    fn an_exhausted_budget_leaves_later_headers_unsent() {
        let mut server = ServerDownloads::new();
        let a = server.stream(b"a".to_vec(), vec![1, 2], u16::MAX).unwrap();
        let b = server.stream(b"a".to_vec(), vec![3, 4], u16::MAX).unwrap();
        let c = server.stream(b"a".to_vec(), vec![5, 6], u16::MAX).unwrap();

        // The first download takes exactly 10 bytes: header 4 + fragment
        // overhead 4 + payload 2. The second download's header does not fit.
        let result = server.add_to_bundle(10);
        assert_eq!(
            result.finished,
            vec![DownloadFinish {
                id: a,
                success: true
            }]
        );
        assert_eq!(server.len(), 2);
        assert_eq!(server.free_id(), a);

        // The next bundle streams the second download from its header on.
        let result = server.add_to_bundle(64);
        assert_eq!(
            result.chunks,
            vec![
                ServerChunk::Header {
                    id: b,
                    description: b"a".to_vec()
                },
                ServerChunk::Fragment {
                    id: b,
                    seq: 0,
                    done: true,
                    payload: vec![3, 4]
                },
                ServerChunk::Header {
                    id: c,
                    description: b"a".to_vec()
                },
                ServerChunk::Fragment {
                    id: c,
                    seq: 0,
                    done: true,
                    payload: vec![5, 6]
                },
            ]
        );
        assert_eq!(
            result.finished,
            vec![
                DownloadFinish {
                    id: b,
                    success: true
                },
                DownloadFinish {
                    id: c,
                    success: true
                }
            ]
        );
        assert!(server.is_empty());
    }

    /// A requested id already in use is rejected.
    #[test]
    fn a_requested_id_in_use_is_rejected() {
        let mut server = ServerDownloads::new();
        assert_eq!(server.stream(Vec::new(), vec![1], 5), Some(5));
        assert_eq!(server.stream(Vec::new(), vec![1], 5), None);
        assert_eq!(server.stream(Vec::new(), vec![1], u16::MAX), Some(0));
    }

    /// A tiny budget still sends the header, then waits for room.
    #[test]
    fn a_tiny_budget_sends_only_the_header() {
        let mut server = ServerDownloads::new();
        let id = server
            .stream(b"x".to_vec(), vec![1, 2, 3], u16::MAX)
            .unwrap();

        let result = server.add_to_bundle(1);
        assert_eq!(
            result.chunks,
            vec![ServerChunk::Header {
                id,
                description: b"x".to_vec()
            }]
        );
        // The download is not done and has not been dropped.
        assert_eq!(server.len(), 1);

        // With room for a fragment it proceeds.
        let result = server.add_to_bundle(64);
        assert!(
            result
                .chunks
                .iter()
                .any(|chunk| matches!(chunk, ServerChunk::Fragment { .. }))
        );
    }

    /// `abort` reports every download as failed and resets the queue.
    #[test]
    fn abort_reports_every_download_as_failed() {
        let mut server = ServerDownloads::new();
        let a = server.stream(Vec::new(), vec![1], 1).unwrap();
        let b = server.stream(Vec::new(), vec![1], 2).unwrap();

        let finished = server.abort();
        assert_eq!(
            finished,
            vec![
                DownloadFinish {
                    id: a,
                    success: false
                },
                DownloadFinish {
                    id: b,
                    success: false
                }
            ]
        );
        assert!(server.is_empty());
        assert_eq!(server.free_id(), 0);
        assert_eq!(server.snapshot_ids(), Vec::<u16>::new());
    }

    /// The onload snapshot carries the ids in use.
    #[test]
    fn snapshot_lists_the_ids_in_use() {
        let mut server = ServerDownloads::new();
        server.stream(Vec::new(), vec![1], 9).unwrap();
        server.stream(Vec::new(), vec![1], 2).unwrap();
        assert_eq!(server.snapshot_ids(), vec![2, 9]);
    }
}
