//! OPT-06-01-03: compare download transfer duration and memory.
//!
//! The acceptance compares transfer duration and memory. The harness runs
//! one deterministic resource corpus through the real server/client download
//! faces twice — the shipped per-tick bundle budget and a tuned one — and
//! records both ledgers whole. The duration is the number of bundle ticks
//! until the last stream completes; the memory is the bytes the two sides
//! hold, not an allocator counter (allocation counts across the script bridge
//! are always zero in this port, so held-bytes is the reproducible face).
//! No wall-clock value is sampled: the comparison reproduces from the seed.

use std::collections::BTreeMap;

use foundation::stream::BinaryStream;

use crate::data_download::{DataDownloads, DownloadEvent};
use crate::server_download::{ServerChunk, ServerDownloads};

/// The shipped corpus size: the number of resources the comparison streams.
pub const CORPUS_STREAMS: usize = 64;
/// The shipped per-tick bundle budget — the drain shape OPT-06-01-01 used.
pub const SHIPPED_BUNDLE_BUDGET: i32 = 64;
/// A guard so a pathological budget cannot spin the harness forever.
pub const TRANSFER_TICK_LIMIT: u32 = 100_000;

/// The drain seam the comparison varies. `Default` reproduces the shipped
/// transfer shape exactly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DownloadTuning {
    /// The bytes the server may put on one bundle tick.
    pub bundle_budget: i32,
}

impl Default for DownloadTuning {
    fn default() -> Self {
        Self {
            bundle_budget: SHIPPED_BUNDLE_BUDGET,
        }
    }
}

/// One transfer run's duration and memory snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DownloadLedger {
    /// The streams queued and completed.
    pub streams: usize,
    /// Bundle ticks until the last stream completed.
    pub ticks: u32,
    /// Payload bytes transferred (sent equals received; asserted in the run).
    pub bytes: usize,
    /// The queued-stream peak on the server side.
    pub server_queue_peak: usize,
    /// The largest number of partial streams the client held at once.
    pub client_in_flight_peak: usize,
    /// The largest payload bytes the client held uncompleted at once —
    /// bytes sent to it that had not yet completed a stream.
    pub client_partial_bytes_peak: usize,
    /// The largest bundle one tick put on the wire (chunk bytes), the
    /// per-tick transfer buffer the budget buys.
    pub bundle_bytes_peak: usize,
}

/// The deterministic transfer corpus: `CORPUS_STREAMS` payloads of 32–287
/// bytes drawn from the seed's LCG.
pub fn comparison_corpus(seed: u64) -> Vec<Vec<u8>> {
    let mut rng = seed;
    (0..CORPUS_STREAMS)
        .map(|index| {
            rng = rng.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            let size = 32 + ((rng >> 33) as usize % 256);
            (0..size)
                .map(|i| (i as u8).wrapping_add(index as u8))
                .collect()
        })
        .collect()
}

/// The real header encoding a server chunk feeds into the client collector.
fn packed_string(bytes: &[u8]) -> Vec<u8> {
    let mut stream = BinaryStream::new();
    let text = String::from_utf8_lossy(bytes);
    stream
        .write_string(&text)
        .expect("description fits a packed string");
    stream.as_bytes().to_vec()
}

/// Queue the corpus on the server and drain it at the tuning's budget while
/// a real client collector reassembles the bytes. Every stream must complete
/// byte-identical or the run panics.
pub fn run_download_transfer(corpus: &[Vec<u8>], tuning: DownloadTuning) -> DownloadLedger {
    let mut server = ServerDownloads::new();
    let mut client = DataDownloads::default();
    let mut payloads: BTreeMap<u32, Vec<u8>> = BTreeMap::new();
    for (index, payload) in corpus.iter().enumerate() {
        let description = format!("res-{index}.bin");
        let id = server
            .stream(description.into_bytes(), payload.clone(), u16::MAX)
            .expect("the corpus fits the concurrent-download bound");
        payloads.insert(u32::from(id), payload.clone());
    }
    let mut ledger = DownloadLedger {
        streams: corpus.len(),
        ticks: 0,
        bytes: 0,
        server_queue_peak: server.len(),
        client_in_flight_peak: 0,
        client_partial_bytes_peak: 0,
        bundle_bytes_peak: 0,
    };

    let mut sent = 0usize;
    let mut received = 0usize;
    while !server.is_empty() {
        ledger.ticks += 1;
        assert!(ledger.ticks <= TRANSFER_TICK_LIMIT, "the drain stalled");
        let result = server.add_to_bundle(tuning.bundle_budget);
        let mut bundle_bytes = 0usize;
        for chunk in &result.chunks {
            match chunk {
                ServerChunk::Fragment { payload, .. } => {
                    bundle_bytes += payload.len();
                    sent += payload.len();
                }
                ServerChunk::Header { description, .. } => {
                    bundle_bytes += description.len();
                }
            }
        }
        ledger.bundle_bytes_peak = ledger.bundle_bytes_peak.max(bundle_bytes);
        for chunk in &result.chunks {
            let event = match chunk {
                ServerChunk::Header { id, description } => {
                    client.resource_header(*id, &packed_string(description))
                }
                ServerChunk::Fragment {
                    id,
                    seq,
                    done,
                    payload,
                } => client.resource_fragment(*id, *seq, u8::from(*done), payload),
            };
            if let Some(DownloadEvent::Complete(stream)) = event {
                assert_eq!(
                    stream.data,
                    payloads[&u32::from(stream.id)],
                    "the client reassembled different bytes"
                );
                received += stream.data.len();
            }
        }
        for done in &result.finished {
            assert!(done.success, "a queued download must not fail");
        }
        ledger.client_in_flight_peak = ledger.client_in_flight_peak.max(client.len());
        ledger.client_partial_bytes_peak = ledger
            .client_partial_bytes_peak
            .max(sent.saturating_sub(received));
    }
    ledger.bytes = received;
    assert_eq!(sent, received, "every sent byte must complete");
    assert_eq!(ledger.bytes, corpus.iter().map(|p| p.len()).sum::<usize>());
    ledger
}

/// Both transfer ledgers, run from the same seed and corpus.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DownloadComparison {
    /// The shipped budget — the OPT-06-01-01 drain shape.
    pub shipped: DownloadLedger,
    /// The tuned budget.
    pub tuned: DownloadLedger,
}

/// The tuned drain: a bundle four times the shipped budget, so the server
/// fronts more bytes per tick and the client holds more partial streams —
/// the duration-versus-memory trade the acceptance asks to compare.
pub fn tuned_download_drain() -> DownloadTuning {
    DownloadTuning {
        bundle_budget: SHIPPED_BUNDLE_BUDGET * 4,
    }
}

/// Run the seeded comparison once.
pub fn run_download_comparison(seed: u64) -> DownloadComparison {
    let corpus = comparison_corpus(seed);
    DownloadComparison {
        shipped: run_download_transfer(&corpus, DownloadTuning::default()),
        tuned: run_download_transfer(&corpus, tuned_download_drain()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEED: u64 = crate::login_baseline::SCENARIO_SEED;

    #[test]
    fn the_comparison_reproduces_from_its_seed() {
        assert_eq!(run_download_comparison(SEED), run_download_comparison(SEED));
        assert_eq!(comparison_corpus(SEED), comparison_corpus(SEED));
    }

    #[test]
    fn the_default_budget_reproduces_the_shipped_ledger() {
        let corpus = comparison_corpus(SEED);
        assert_eq!(
            run_download_transfer(&corpus, DownloadTuning::default()),
            run_download_transfer(
                &corpus,
                DownloadTuning {
                    bundle_budget: SHIPPED_BUNDLE_BUDGET
                }
            )
        );
    }

    #[test]
    fn the_tuned_budget_cuts_the_transfer() {
        let comparison = run_download_comparison(SEED);
        assert!(comparison.tuned.ticks < comparison.shipped.ticks);
        assert_eq!(comparison.tuned.bytes, comparison.shipped.bytes);
        assert_eq!(comparison.tuned.streams, comparison.shipped.streams);
    }

    #[test]
    fn the_memory_trade_is_on_the_record() {
        let comparison = run_download_comparison(SEED);
        // The tuned drain buys its shorter transfer with a larger per-tick
        // bundle; the server's queue peak is the same because the corpus is
        // queued up front in both runs. The client's partial backlog is
        // recorded but not direction-asserted: which side holds more
        // depends on the corpus shape, and both numbers are on the ledger.
        assert!(comparison.shipped.bundle_bytes_peak > 0);
        assert!(comparison.tuned.bundle_bytes_peak > comparison.shipped.bundle_bytes_peak);
        assert!(comparison.shipped.client_partial_bytes_peak > 0);
        assert!(comparison.tuned.client_partial_bytes_peak > 0);
        assert_eq!(
            comparison.tuned.server_queue_peak,
            comparison.shipped.server_queue_peak
        );
    }

    #[test]
    fn an_aborted_transfer_shrinks_the_queue() {
        let corpus = comparison_corpus(SEED);
        let mut server = ServerDownloads::new();
        for (index, payload) in corpus.iter().enumerate().take(4) {
            server
                .stream(
                    format!("res-{index}.bin").into_bytes(),
                    payload.clone(),
                    u16::MAX,
                )
                .expect("the corpus fits the concurrent-download bound");
        }
        assert_eq!(server.len(), 4);
        let finished = server.abort();
        assert_eq!(finished.len(), 4);
        assert!(server.is_empty());
    }
}
