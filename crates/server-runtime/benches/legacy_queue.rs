//! The `d741450` download-queue mechanics, verbatim — the OPT-06-01-03 A/B
//! baseline. One `VecDeque::remove` per finished download, an O(n) tail
//! shift each; the reference drains a `std::list` whose `erase(it)` is O(1)
//! (`data_downloads.cpp:126`), so this shape is the porting debt the
//! one-pass compaction pays off. Included by
//! `benches/download_drain.rs` (duration A/B) and
//! `tests/download_drain_alloc.rs` (allocation A/B) so both harnesses
//! compare against one copy. Uses the crate's public chunk types, so a
//! caller can assert output parity with `ServerDownloads` directly.

use std::collections::{BTreeSet, VecDeque};

use server_runtime::server_download::{AddResult, DownloadFinish, ServerChunk};

const MAX_CONCURRENT_DOWNLOADS: usize = 0xfffe;

struct Download {
    id: u16,
    seq: u8,
    description: Option<Vec<u8>>,
    data: Vec<u8>,
    sent: usize,
    good: bool,
}

impl Download {
    fn new(id: u16, description: Vec<u8>, data: Vec<u8>) -> Self {
        Download {
            id,
            seq: 0,
            description: Some(description),
            data,
            sent: 0,
            good: true,
        }
    }

    fn available(&self) -> usize {
        self.data.len() - self.sent
    }

    fn done(&self) -> bool {
        self.available() == 0
    }

    fn add_to_bundle(&mut self, remaining: &mut i32) -> Vec<ServerChunk> {
        let mut chunks = Vec::new();

        if let Some(description) = self.description.take() {
            let cost = 2 + 1 + description.len() as i32;
            chunks.push(ServerChunk::Header {
                id: self.id,
                description,
            });
            *remaining -= cost;
            if *remaining <= 0 {
                return chunks;
            }
        }

        if self.available() == 0 {
            return chunks;
        }

        *remaining -= 4;
        if *remaining <= 0 {
            return chunks;
        }

        let payload_size = std::cmp::min(*remaining as usize, self.available());
        *remaining -= payload_size as i32;

        let seq = self.seq;
        self.seq = self.seq.wrapping_add(1);
        let take = std::cmp::min(payload_size, self.available());
        let payload = self.data[self.sent..self.sent + take].to_vec();
        self.sent += take;
        let done = self.done();

        chunks.push(ServerChunk::Fragment {
            id: self.id,
            seq,
            done,
            payload,
        });
        chunks
    }
}

#[derive(Default)]
pub struct LegacyServerDownloads {
    downloads: VecDeque<Download>,
    free_id: u16,
    used_ids: BTreeSet<u16>,
}

impl LegacyServerDownloads {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn stream(&mut self, description: Vec<u8>, data: Vec<u8>, requested_id: u16) -> u16 {
        let id = if requested_id == u16::MAX {
            self.free_id
        } else {
            requested_id
        };
        self.downloads
            .push_back(Download::new(id, description, data));
        self.used_ids.insert(id);
        if id == self.free_id {
            self.advance_free_id(id);
        }
        id
    }

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

            result
                .chunks
                .extend(self.downloads[index].add_to_bundle(&mut remaining));

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

    fn remove_at(&mut self, index: usize) {
        let download = self.downloads.remove(index).expect("index in range");
        self.used_ids.remove(&download.id);
        if self.free_id > download.id {
            self.free_id = download.id;
        }
    }

    fn advance_free_id(&mut self, assigned: u16) {
        for i in 1..MAX_CONCURRENT_DOWNLOADS as u16 {
            self.free_id = assigned.wrapping_add(i);
            if !self.used_ids.contains(&self.free_id) {
                break;
            }
        }
    }
}
