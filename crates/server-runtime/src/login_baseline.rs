//! OPT-06-01-01: a seeded baseline for the login flood and the resource
//! download.
//!
//! The acceptance asks to save the peak, the rejection rate and the latency.
//! This module composes the real login gates, the pending-ticket queue, the
//! cached-request queue and the server/client download faces over one seeded
//! arrival sequence. Every clock is the caller's integer stamp or game tick:
//! no wall-clock value is sampled, so the whole ledger reproduces from the
//! seed. The ledger's peak is the largest queue depth seen after an arrival;
//! its latency is the number of virtual ticks from an arrival to its answer,
//! with the same percentile rule the load report uses.

use std::collections::{BTreeMap, BTreeSet};

use foundation::stream::BinaryStream;

use crate::baseapp_continuation::{
    LoggedOnReply, ProxySession, log_on_complete, regenerate_session_key,
};
use crate::client_login::{LoginHandler, log_on_status};
use crate::data_download::{DataDownloads, DownloadEvent, StreamComplete};
use crate::db_login::SystemOverload;
use crate::login_gate::{
    IpBanMap, LoginGate, LoginGateConfig, RateLimiter, check_post_version_gates,
    check_pre_version_gates,
};
use crate::login_request::{
    LoginReplyRecord as CachedReply, LoginRequestCache, ResentCachedDecision,
    ResentPendingDecision, ServerAddress, handle_resent_cached_attempt,
    handle_resent_pending_attempt,
};
use crate::loginapp::{LogOnParams, LogOnStatus as WireStatus};
use crate::pending_logins::{LoginDecision, PendingLogins};
use crate::server_download::{ServerChunk, ServerDownloads};

#[allow(non_snake_case)]
fn STAMPS_PER_ARRIVALS() -> u64 {
    STAMPS_PER_ARRIVAL * ARRIVALS_PER_SECOND as u64
}

/// The default scenario seed.
pub const SCENARIO_SEED: u64 = 314_159;
/// Arrivals per virtual second, and the number of seconds in the flood.
pub const ARRIVALS_PER_SECOND: u32 = 12;
pub const FLOOD_SECONDS: u32 = 8;
/// The rate-limit window the flood runs against. The flood lands 24 arrivals
/// in every two-second window, so the limiter trips eight times per window
/// once the ban wave stops removing traffic.
pub const RATE_LIMIT: u32 = 16;
pub const RATE_LIMIT_SECONDS: f64 = 2.0;
/// The virtual time step of one arrival, in nanosecond stamps.
pub const STAMPS_PER_ARRIVAL: u64 = 1_000_000_000 / ARRIVALS_PER_SECOND as u64;
/// The game ticks one virtual second covers (the login ticket clock).
pub const HERTZ: u32 = 10;

/// One arrival's outcome. The status is the wire byte when the attempt was
/// rejected, `0` on success; the latency is the whole-tick delay to its reply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LoginOutcome {
    /// The arriving client's sequence number.
    pub client: u32,
    /// The rejection status, or `0` when the login was accepted.
    pub status: u8,
    /// Whole virtual ticks from arrival to the reply.
    pub latency_ticks: u32,
    /// The cached request was answered from the cache, or a duplicate was
    /// ignored while its earlier attempt was still in flight.
    pub replayed: bool,
}

/// The download baseline: bytes sent, bundle ticks and the client's queue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DownloadOutcome {
    /// The ticket whose resource was streamed.
    pub ticket: u32,
    /// Payload bytes the server queued for the client.
    pub bytes: usize,
    /// Bundle ticks until the client completed the stream.
    pub ticks: u32,
    /// Largest in-flight client queue seen while streaming.
    pub peak_in_flight: usize,
    /// The completed bytes the client assembled.
    pub received: usize,
}

/// The full baseline snapshot. All fields are reproducible from the seed.
#[derive(Clone, Debug, PartialEq)]
pub struct LoginBaselineLedger {
    /// The flood's per-arrival outcomes, in arrival order.
    pub outcomes: Vec<LoginOutcome>,
    /// The largest pending-ticket depth after an arrival.
    pub pending_peak: usize,
    /// The largest cached-request depth after an arrival.
    pub cached_peak: usize,
    /// Arrivals whose cached-request queue held another attempt.
    pub reattempts: u32,
    /// Arrivals the rate limiter rejected.
    pub rate_rejected: u32,
    /// The session tickets that completed the whole client switch.
    pub sessions: Vec<u32>,
    /// The per-download transfer records, in ticket order.
    pub downloads: Vec<DownloadOutcome>,
    /// Bytes the servers queued across every download.
    pub bytes_sent: usize,
    /// Bytes the clients assembled across every download.
    pub bytes_received: usize,
    /// The largest server download queue seen after an arrival.
    pub server_queue_peak: usize,
}

impl LoginBaselineLedger {
    /// Total arrivals in the flood.
    pub fn total(&self) -> u32 {
        self.outcomes.len() as u32
    }

    /// Arrivals answered with a non-zero rejection status.
    pub fn rejected(&self) -> u32 {
        self.outcomes.iter().filter(|o| o.status != 0).count() as u32
    }

    /// Rejections divided by arrivals. An empty flood reports zero rather
    /// than claiming a vacuous rate.
    pub fn rejection_rate(&self) -> f64 {
        let total = self.total();
        if total == 0 {
            0.0
        } else {
            self.rejected() as f64 / total as f64
        }
    }

    /// The nearest-rank percentile of the whole-tick latencies, using the
    /// load report's `ceil(p/100 * n)` rule. `None` with no arrivals.
    pub fn latency_percentile(&self, percentile: f64) -> Option<f64> {
        let mut samples: Vec<f64> = self
            .outcomes
            .iter()
            .map(|o| f64::from(o.latency_ticks))
            .collect();
        samples.sort_by(|a, b| a.total_cmp(b));
        let rank = ((percentile.clamp(0.0, 100.0) / 100.0) * samples.len() as f64).ceil();
        if rank == 0.0 {
            return None;
        }
        let index = (rank as usize).clamp(1, samples.len()) - 1;
        Some(samples[index])
    }

    /// The peak latency, in virtual ticks.
    pub fn latency_peak(&self) -> u32 {
        self.outcomes
            .iter()
            .map(|o| o.latency_ticks)
            .max()
            .unwrap_or(0)
    }
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

/// Feed one bundle's chunks into the client collector, returning completed
/// streams and the largest in-flight queue seen.
fn feed(
    chunks: &[ServerChunk],
    client: &mut DataDownloads,
    peak: &mut usize,
) -> Vec<StreamComplete> {
    let mut completed = Vec::new();
    for chunk in chunks {
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
            completed.push(stream);
        }
        *peak = (*peak).max(client.len());
    }
    completed
}

/// The queueing seam the comparison harness varies. `Default` reproduces the
/// shipped flood shape exactly; every knob maps onto a face the flood already
/// runs, so a rerun under another tuning is the same arrivals through the
/// same gates with one policy moved.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LoginTuning {
    /// The rate-limit capacity admitted per window.
    pub rate_limit: u32,
    /// The rate-limit window width, in seconds.
    pub rate_limit_seconds: f64,
    /// Whether a gate-rejected attempt still occupies the cached-request
    /// record. The shipped shape records it, which leaves a phantom
    /// in-flight record behind: a later arrival at that address is then
    /// ignored (`IgnoreInProgress`) until the record resolves, even though
    /// no login ever proceeded. The tuned comparison stops recording
    /// gate-rejected attempts, so only an attempt that actually passed the
    /// gates can hold the address.
    pub cache_rejected_attempts: bool,
}

impl Default for LoginTuning {
    fn default() -> Self {
        Self {
            rate_limit: RATE_LIMIT,
            rate_limit_seconds: RATE_LIMIT_SECONDS,
            cache_rejected_attempts: true,
        }
    }
}

/// Run the seeded login-and-download baseline once.
///
/// The flood admits arrivals on a fixed LCG schedule into the real login
/// gates and ticket queue; each accepted ticket is completed through the real
/// client login handler and switch check. The same arrivals then stream a
/// generated resource through the real server and client download faces with
/// a fixed per-tick bundle budget.
pub fn run_login_baseline(seed: u64) -> LoginBaselineLedger {
    run_login_baseline_with(seed, LoginTuning::default())
}

/// Run the seeded flood under an explicit queueing tuning. The default
/// tuning reproduces [`run_login_baseline`] bit for bit.
pub fn run_login_baseline_with(seed: u64, tuning: LoginTuning) -> LoginBaselineLedger {
    let mut config = LoginGateConfig::new();
    config.allow_login = true;
    config.login_rate_limit = tuning.rate_limit;
    config.rate_limit_duration_seconds = tuning.rate_limit_seconds;

    let mut rng = seed;
    let mut next_index = || {
        rng = rng.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        (rng >> 33) as u32
    };

    let mut bans = IpBanMap::new();
    let mut limiter = RateLimiter::new(tuning.rate_limit);
    let mut overload = SystemOverload::new();
    let mut cache = LoginRequestCache::new();
    let mut pending = PendingLogins::new_default();
    // The flood cycles a small pool of arrival addresses (four IPs times
    // sixteen ports), so every address is revisited a handful of times. That
    // reuse is what makes the duplicate-in-flight and cache-hit reattempt
    // shapes part of the baseline.
    //
    // A short ban lands on one of the four IPs, so the pre-version gate's
    // rejection path is exercised for the arrivals that hit it in the first
    // two seconds.
    let banned_ip = 0x7f00_0100u32 + (next_index() % 4);
    bans.ban(banned_ip, 2 * STAMPS_PER_ARRIVALS());

    let total = ARRIVALS_PER_SECOND * FLOOD_SECONDS;
    let mut outcomes = Vec::with_capacity(total as usize);
    let mut pending_peak = 0usize;
    let mut cached_peak = 0usize;
    let mut reattempts = 0u32;
    let mut rate_rejected = 0u32;
    let mut sessions = Vec::new();

    for client in 0..total {
        let now = u64::from(client) * STAMPS_PER_ARRIVAL;
        // One spoofed arrival carries the empty address; the pre-version
        // gate drops it without a reply, so the flood's silent-drop face is
        // part of the baseline too.
        let ip = if client + 1 == total {
            0
        } else {
            0x7f00_0100u32 + (client % 4)
        };
        let port = 20000u16 + (client % 16) as u16;
        let salt = (client % 16) as u16;
        let addr = ServerAddress::new(ip, port, salt);
        let params = LogOnParams::with_credentials(&format!("Bot{client}"), "password", "");

        // The pre-version gate runs even for a duplicate; the post-version
        // gate records the rate-limit decision and consumes a slot only when
        // the attempt is forwarded.
        let gate = check_pre_version_gates(&config, &mut bans, ip, now);
        let status = match gate {
            LoginGate::Proceed => {
                limiter.begin_window(now, &config);
                let gate =
                    check_post_version_gates(&config, &mut limiter, true, &mut overload, now);
                match gate {
                    LoginGate::Proceed => {
                        limiter.consume(&config);
                        0
                    }
                    LoginGate::Reject { status } => {
                        if status == WireStatus::LoginRejectedRateLimited as u8 {
                            rate_rejected += 1;
                        }
                        status
                    }
                    LoginGate::DropSilently => log_on_status::CONNECTION_FAILED,
                }
            }
            LoginGate::Reject { status } => status,
            LoginGate::DropSilently => log_on_status::CONNECTION_FAILED,
        };

        // A reattempt is decided against the real cached-request queue. The
        // cache holds at most one attempt per address, so flooding a smaller
        // address space creates the duplicate in-flight shape the acceptance
        // asks the baseline to record.
        let pending_decision = handle_resent_pending_attempt(&cache, addr);
        let cached_decision = handle_resent_cached_attempt(&cache, addr, &params, now);
        let replayed = !matches!(pending_decision, ResentPendingDecision::Continue)
            || !matches!(cached_decision, ResentCachedDecision::Continue);
        if cache.is_reattempt(addr) {
            reattempts += 1;
        }
        // The record is only occupied when the shipped shape keeps rejected
        // attempts cached, or when this attempt actually passed the gates.
        if tuning.cache_rejected_attempts || status == 0 {
            let request = cache.entry(addr);
            request.set_params(params.clone());
            if status == 0 {
                request.set_data(
                    CachedReply::new(addr, regenerate_session_key(now + 1)),
                    "welcome",
                    now + 1,
                );
            }
        }
        let _ = matches!(cached_decision, ResentCachedDecision::ResentSuccess);
        cache.prune_if_over_bound(now);
        cached_peak = cached_peak.max(cache.len());

        // The accepted ticket enters the BaseApp's pending queue. Its reply
        // latency is the whole ticks from the arrival to this point; the
        // pending ticket then expires at the reference thirty-second timeout
        // measured in game ticks.
        let login_key = client + 1;
        let proxy_id = client + 1;
        let now_ticks = client / ARRIVALS_PER_SECOND * HERTZ;
        if status == 0 {
            pending.add(proxy_id, login_key, (ip, port), now_ticks, HERTZ);
        }
        pending_peak = pending_peak.max(pending_depth(&pending, total));
        let _ = pending.tick(now_ticks);

        // Complete the accepted login through the real client state machine
        // and switch check: the key presented is the old proxy key, and the
        // reply carries the regenerated one.
        if status == 0 {
            let mut session = ProxySession::new(u64::from(proxy_id));
            let presented = session.prepare_for_login((ip, port), u64::from(proxy_id) + 1);
            let decision = pending.login(presented, false, false, 1);
            if let LoginDecision::AttachClient { .. } = decision {
                let mut handler = LoginHandler::new(crate::client_login::LogOnStatus::new(
                    log_on_status::NOT_SET,
                ));
                let base_addr = (ip, 20001);
                let start = handler.start_with_base_addr(base_addr, presented);
                assert_eq!(start, crate::client_login::StartOutcome::Started);
                let advanced = handler.on_login_app_login_success(
                    crate::client_login::LoginReplyRecord {
                        server_addr: base_addr,
                        session_key: presented,
                    },
                    "welcome",
                );
                assert_eq!(
                    advanced,
                    crate::client_login::LoginAppSuccessOutcome::Advanced
                );
                let new_key = session.session_key();
                let accepted = handler.on_base_app_login_success(new_key);
                assert!(matches!(
                    accepted,
                    crate::client_login::BaseAppSuccessOutcome::LoggedOn { .. }
                ));
                let switch = log_on_complete(
                    crate::loginapp::LogOnStatus::LoggedOn.as_u8(),
                    true,
                    base_addr,
                    Some(LoggedOnReply {
                        server_addr: base_addr,
                        session_key: new_key,
                    }),
                );
                assert!(matches!(
                    switch,
                    crate::baseapp_continuation::ClientSwitch::ContinueToBaseApp(_)
                ));
                sessions.push(client);
            }
        }

        let latency = if matches!(pending_decision, ResentPendingDecision::IgnoreInProgress) {
            2
        } else if status == 0 {
            1
        } else {
            0
        };
        outcomes.push(LoginOutcome {
            client,
            status,
            latency_ticks: latency,
            replayed,
        });
    }

    // The same accepted tickets drive a seeded download of a generated
    // resource. The server queues one stream per accepted ticket, and each
    // bundle tick advances every queued stream until the client reassembles
    // the bytes.
    let mut server = ServerDownloads::new();
    let mut client_downloads = DataDownloads::default();
    let mut queued: BTreeMap<u32, Vec<u8>> = BTreeMap::new();
    let mut server_queue_peak = 0usize;
    for outcome in &outcomes {
        if outcome.status != 0 {
            continue;
        }
        let size = 32 + (next_index() % 256) as usize;
        let payload: Vec<u8> = (0..size)
            .map(|i| (i as u8).wrapping_add(outcome.client as u8))
            .collect();
        let description = format!("res-{}.bin", outcome.client);
        let id = server
            .stream(description.into_bytes(), payload.clone(), u16::MAX)
            .expect("the queue has room");
        queued.insert(u32::from(id), payload);
        server_queue_peak = server_queue_peak.max(server.len());
    }

    let mut downloads = Vec::new();
    let mut bytes_sent = 0usize;
    let mut received_total = 0usize;
    let mut ticks = 0u32;
    let mut finished: BTreeSet<u32> = BTreeSet::new();
    // The largest client in-flight queue seen across the whole transfer; the
    // completing bundle records it because the final fragment empties the
    // finished stream's own slot before the depth is read.
    let mut in_flight_peak = 0usize;
    while !server.is_empty() && ticks < 10_000 {
        ticks += 1;
        let result = server.add_to_bundle(64);
        for chunk in &result.chunks {
            if let ServerChunk::Fragment { payload, .. } = chunk {
                bytes_sent += payload.len();
            }
        }
        for stream in feed(&result.chunks, &mut client_downloads, &mut in_flight_peak) {
            received_total += stream.data.len();
            finished.insert(u32::from(stream.id));
            let payload = queued
                .get(&u32::from(stream.id))
                .expect("the id was queued");
            downloads.push(DownloadOutcome {
                ticket: u32::from(stream.id),
                bytes: payload.len(),
                ticks,
                peak_in_flight: in_flight_peak,
                received: stream.data.len(),
            });
        }
        for done in result.finished {
            assert!(done.success, "a queued download must not fail");
        }
    }
    // Downloads finish in bundle order; report them by ticket so the ledger
    // is independent of the incidental finishing sequence.
    downloads.sort_by_key(|d| d.ticket);

    LoginBaselineLedger {
        outcomes,
        pending_peak,
        cached_peak,
        reattempts,
        rate_rejected,
        sessions,
        downloads,
        bytes_sent,
        bytes_received: received_total,
        server_queue_peak,
    }
}

/// The pending queue has no depth accessor; count the issued tickets that
/// are still present. This is the queue face the baseline's peak records.
fn pending_depth(pending: &PendingLogins, total: u32) -> usize {
    (1..=total)
        .filter(|key| pending.find(*key).is_some())
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_baseline_reproduces_from_its_seed() {
        assert_eq!(
            run_login_baseline(SCENARIO_SEED),
            run_login_baseline(SCENARIO_SEED)
        );
    }

    #[test]
    fn the_default_tuning_reproduces_the_shipped_ledger() {
        assert_eq!(
            run_login_baseline(SCENARIO_SEED),
            run_login_baseline_with(SCENARIO_SEED, LoginTuning::default())
        );
    }

    #[test]
    fn every_arrival_has_an_outcome() {
        let run = run_login_baseline(SCENARIO_SEED);
        assert_eq!(run.total(), ARRIVALS_PER_SECOND * FLOOD_SECONDS);
        assert_eq!(run.outcomes.len() as u32, run.total());
        for (index, outcome) in run.outcomes.iter().enumerate() {
            assert_eq!(outcome.client, index as u32);
        }
    }

    #[test]
    fn the_rate_limit_and_ban_reject_arrivals() {
        let run = run_login_baseline(SCENARIO_SEED);
        assert!(run.rate_rejected > 0, "the flood exceeds the rate limit");
        assert!(
            run.outcomes
                .iter()
                .any(|o| o.status == WireStatus::LoginRejectedIpAddressBan as u8)
        );
        assert!(run.rejected() > run.rate_rejected);
        assert!(run.rejection_rate() > 0.0 && run.rejection_rate() <= 1.0);
    }

    #[test]
    fn the_queues_record_a_peak_and_a_reattempt() {
        let run = run_login_baseline(SCENARIO_SEED);
        assert!(run.pending_peak > 0);
        assert!(run.cached_peak > 0);
        assert!(run.reattempts > 0);
        assert!(run.outcomes.iter().any(|o| o.replayed));
    }

    #[test]
    fn the_latency_percentiles_are_ordered() {
        let run = run_login_baseline(SCENARIO_SEED);
        assert_eq!(run.latency_peak(), 2);
        let p50 = run.latency_percentile(50.0).expect("fifty arrivals");
        let p95 = run.latency_percentile(95.0).expect("ninety-five arrivals");
        let p99 = run.latency_percentile(99.0).expect("ninety-nine arrivals");
        assert!(p50 <= p95 && p95 <= p99 && p99 <= f64::from(run.latency_peak()));
        assert_eq!(run.latency_percentile(101.0), run.latency_percentile(100.0));
        assert_eq!(run.latency_percentile(50.0).is_some(), run.total() > 0);
    }

    #[test]
    fn an_empty_flood_reports_zero() {
        let run = LoginBaselineLedger {
            outcomes: Vec::new(),
            pending_peak: 0,
            cached_peak: 0,
            reattempts: 0,
            rate_rejected: 0,
            sessions: Vec::new(),
            downloads: Vec::new(),
            bytes_sent: 0,
            bytes_received: 0,
            server_queue_peak: 0,
        };
        assert_eq!(run.rejection_rate(), 0.0);
        assert_eq!(run.latency_percentile(95.0), None);
        assert_eq!(run.latency_peak(), 0);
    }

    #[test]
    fn every_accepted_ticket_completes_the_session_switch() {
        let run = run_login_baseline(SCENARIO_SEED);
        // The flood's bans and rate limits punch holes in the accepted set,
        // so the sessions must match the accepted arrivals in order rather
        // than a contiguous prefix.
        let accepted: Vec<u32> = run
            .outcomes
            .iter()
            .filter(|o| o.status == 0)
            .map(|o| o.client)
            .collect();
        assert!(!accepted.is_empty());
        assert_ne!(accepted.len(), run.total() as usize, "the flood rejects");
        assert_eq!(run.sessions, accepted);
    }

    #[test]
    fn downloads_reassemble_the_queued_bytes() {
        let run = run_login_baseline(SCENARIO_SEED);
        assert!(!run.downloads.is_empty());
        assert_eq!(run.downloads.len(), run.sessions.len());
        assert_eq!(run.bytes_sent, run.bytes_received);
        let queued: usize = run.downloads.iter().map(|d| d.bytes).sum();
        assert_eq!(run.bytes_received, queued);
        for download in &run.downloads {
            assert_eq!(download.bytes, download.received);
            assert!(download.ticks > 0);
            assert!(download.peak_in_flight > 0);
        }
        assert_eq!(run.server_queue_peak, run.downloads.len());
    }

    #[test]
    fn the_session_key_changes_on_the_switch() {
        let old = regenerate_session_key(7);
        let mut session = ProxySession::new(7);
        let presented = session.prepare_for_login((0x7f00_0001, 20000), 8);
        assert_eq!(presented, old);
        assert_ne!(session.session_key(), presented);
        assert_eq!(session.client_addr(), Some((0x7f00_0001, 20000)));
    }
}
