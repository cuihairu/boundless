//! BW-06-01-02 login challenge negotiation.
//!
//! Before it reads credentials, the reference LoginApp can require the client
//! to answer a challenge. `LoginApp::processForLoginChallenge`
//! (`server/loginapp/loginapp.cpp`) decides whether to issue one, and
//! `LoginApp::challengeResponse` verifies the client's reply. The challenge
//! types and the factory registry live in `lib/connection/`:
//!
//! * `login_challenge.hpp` — the four-method `LoginChallenge` interface;
//! * `login_challenge_factory.cpp` — `delay`, `fail` and the registry
//!   (`LoginChallengeFactories::registerDefaultFactories`);
//! * `cuckoo_cycle_login_challenge_factory.cpp` — the proof-of-work challenge.
//!
//! A challenge is issued only when `Config::challengeType` is non-empty; with
//! an empty type the request falls straight through to the logon parameters
//! (the "disabled path"). The wire reply for a challenge is
//! `LogOnStatus::LOGIN_CHALLENGE_ISSUED`, the challenge type string, then the
//! challenge data from `writeChallengeToStream`. The client answers through
//! `challengeResponse`: the calculation duration as a `float`, then the
//! challenge response from `writeResponseToStream`.

use foundation::stream::BinaryStream;

/// `SIZESHIFT` in `cuckoo_cycle_login_challenge_factory.cpp`: the graph has
/// `1 << SIZESHIFT` nodes.
const SIZESHIFT: u32 = 20;
/// `PROOFSIZE`: the cycle length the proof of work must find.
const PROOFSIZE: usize = 42;
/// `SIZE`: `1 << SIZESHIFT`.
const SIZE: u64 = 1 << SIZESHIFT;
/// `HALFSIZE`.
const HALFSIZE: u64 = SIZE / 2;
/// `NODEMASK`: the mask applied to a SipHash output to land in `HALFSIZE`.
const NODEMASK: u64 = HALFSIZE - 1;

/// `siphash_ctx`: the four key words, expanded from the SHA-256 of the header.
#[derive(Clone, Copy)]
struct SipState {
    v: [u64; 4],
}

/// One `SIPROUND` of SipHash-2-4.
#[inline]
fn sip_round(v0: &mut u64, v1: &mut u64, v2: &mut u64, v3: &mut u64) {
    *v0 = v0.wrapping_add(*v1);
    *v2 = v2.wrapping_add(*v3);
    *v1 = v1.rotate_left(13);
    *v3 = v3.rotate_left(16);
    *v1 ^= *v0;
    *v3 ^= *v2;
    *v0 = v0.rotate_left(32);
    *v2 = v2.wrapping_add(*v1);
    *v0 = v0.wrapping_add(*v3);
    *v1 = v1.rotate_left(17);
    *v3 = v3.rotate_left(21);
    *v1 ^= *v2;
    *v3 ^= *v0;
    *v2 = v2.rotate_left(32);
}

/// `setheader`: SHA-256 the header and fold the first sixteen bytes into the
/// four SipHash key words with the fixed IV xor constants.
fn set_header(header: &[u8]) -> SipState {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(header);
    let k0 = u64::from_le_bytes(digest[0..8].try_into().expect("sha256 is 32 bytes"));
    let k1 = u64::from_le_bytes(digest[8..16].try_into().expect("sha256 is 32 bytes"));
    SipState {
        v: [
            k0 ^ 0x736f_6d65_7073_6575,
            k1 ^ 0x646f_7261_6e64_6f6d,
            k0 ^ 0x6c79_6765_6e65_7261,
            k1 ^ 0x7465_6462_7974_6573,
        ],
    }
}

/// `siphash24`: SipHash-2-4 specialized to a single little-endian 8-byte
/// message (the nonce), following the reference exactly.
fn siphash24(ctx: &SipState, nonce: u64) -> u64 {
    let mut v0 = ctx.v[0];
    let mut v1 = ctx.v[1];
    let mut v2 = ctx.v[2];
    let mut v3 = ctx.v[3] ^ nonce;
    for _ in 0..2 {
        sip_round(&mut v0, &mut v1, &mut v2, &mut v3);
    }
    v0 ^= nonce;
    v2 ^= 0xff;
    for _ in 0..4 {
        sip_round(&mut v0, &mut v1, &mut v2, &mut v3);
    }
    v0 ^ v1 ^ v2 ^ v3
}

/// `sipnode`: one endpoint of the edge for a nonce.
fn sipnode(ctx: &SipState, nonce: u32, uorv: u32) -> u32 {
    (siphash24(ctx, 2 * nonce as u64 + uorv as u64) & NODEMASK) as u32
}

/// `sipedge`: both endpoints of the edge for a nonce.
fn sipedge(ctx: &SipState, nonce: u32) -> (u32, u32) {
    (sipnode(ctx, nonce, 0), sipnode(ctx, nonce, 1))
}

/// `verify`: whether the (ascending, all `< easiness`) nonces form a
/// `PROOFSIZE`-cycle in the header-generated cuckoo graph.
fn verify_nonces(nonces: &[u32; PROOFSIZE], header: &[u8], easiness: u64) -> bool {
    let ctx = set_header(header);
    let mut us = [0u32; PROOFSIZE];
    let mut vs = [0u32; PROOFSIZE];
    for n in 0..PROOFSIZE {
        let nonce = nonces[n] as u64;
        if nonce >= easiness || (n > 0 && nonce <= nonces[n - 1] as u64) {
            return false;
        }
        let (u, v) = sipedge(&ctx, nonces[n]);
        us[n] = u;
        vs[n] = v;
    }
    // Follow the cycle from edge 0, alternating the shared v then shared u
    // endpoint, until we return to edge 0; every step must have a unique
    // successor, and the walk must consume exactly all the edges.
    let mut i = 0usize;
    let mut n_edges = PROOFSIZE as i32;
    loop {
        let mut j = i;
        for k in 0..PROOFSIZE {
            if k != i && vs[k] == vs[i] {
                if j != i {
                    return false;
                }
                j = k;
            }
        }
        if j == i {
            return false;
        }
        i = j;
        for k in 0..PROOFSIZE {
            if k != j && us[k] == us[j] {
                if i != j {
                    return false;
                }
                i = k;
            }
        }
        if i == j {
            return false;
        }
        n_edges -= 2;
        if i == 0 {
            break;
        }
    }
    n_edges == 0
}

// ---------------------------------------------------------------------------
// LoginChallenge trait
// ---------------------------------------------------------------------------

/// `LoginChallenge` (`lib/connection/login_challenge.hpp`).
///
/// The four methods pair up: a server writes the challenge to the client
/// (`write_challenge_to_stream`) and the client reads it
/// (`read_challenge_from_stream`); the client writes its answer
/// (`write_response_to_stream`) and the server verifies it
/// (`read_response_from_stream`).
pub trait LoginChallenge {
    /// `writeChallengeToStream`.
    fn write_challenge_to_stream(&self, stream: &mut BinaryStream) -> bool;
    /// `readChallengeFromStream`.
    fn read_challenge_from_stream(&mut self, stream: &mut BinaryStream) -> bool;
    /// `writeResponseToStream`.
    fn write_response_to_stream(&self, stream: &mut BinaryStream) -> bool;
    /// `readResponseFromStream`.
    fn read_response_from_stream(&mut self, stream: &mut BinaryStream) -> bool;
}

/// `DelayLoginChallenge` (`login_challenge_factory.cpp`): the client waits
/// `duration` seconds then echoes it back; the server accepts an exact match.
///
/// The reference notes this is an example, not a real production gate — the
/// duration is not enforced on the server.
pub struct DelayChallenge {
    duration: f32,
}

impl DelayChallenge {
    /// Constructor.
    pub fn new(duration: f32) -> Self {
        Self { duration }
    }

    /// `duration()`.
    pub fn duration(&self) -> f32 {
        self.duration
    }
}

impl LoginChallenge for DelayChallenge {
    fn write_challenge_to_stream(&self, stream: &mut BinaryStream) -> bool {
        stream.write_f32(self.duration);
        true
    }

    fn read_challenge_from_stream(&mut self, stream: &mut BinaryStream) -> bool {
        match stream.read_f32() {
            Ok(duration) => {
                self.duration = duration;
                true
            }
            Err(_) => false,
        }
    }

    fn write_response_to_stream(&self, stream: &mut BinaryStream) -> bool {
        // The reference client sleeps here, then echoes the duration.
        stream.write_f32(self.duration);
        true
    }

    fn read_response_from_stream(&mut self, stream: &mut BinaryStream) -> bool {
        match stream.read_f32() {
            Ok(duration) => duration == self.duration,
            Err(_) => false,
        }
    }
}

/// `AlwaysFailLoginChallenge` (`login_challenge_factory.cpp`): every response
/// is rejected.
pub struct FailChallenge;

impl LoginChallenge for FailChallenge {
    fn write_challenge_to_stream(&self, _stream: &mut BinaryStream) -> bool {
        true
    }

    fn read_challenge_from_stream(&mut self, _stream: &mut BinaryStream) -> bool {
        true
    }

    fn write_response_to_stream(&self, _stream: &mut BinaryStream) -> bool {
        true
    }

    fn read_response_from_stream(&mut self, _stream: &mut BinaryStream) -> bool {
        false
    }
}

/// `CuckooCycleLoginChallenge` (`cuckoo_cycle_login_challenge_factory.cpp`):
/// a proof-of-work challenge.
///
/// The challenge is a random 16-hex-digit prefix plus a colon and the
/// `maxNonce`. The client answers with a `key` (the prefix followed by an
/// attempt counter) and the 42 nonces of a found cycle; the server verifies
/// the nonces against the key-derived graph.
pub struct CuckooCycleChallenge {
    prefix: String,
    max_nonce: u64,
}

impl CuckooCycleChallenge {
    /// Constructor.
    pub fn new(prefix: String, max_nonce: u64) -> Self {
        Self { prefix, max_nonce }
    }

    /// `prefix()`.
    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    /// `maxNonce()`.
    pub fn max_nonce(&self) -> u64 {
        self.max_nonce
    }
}

impl LoginChallenge for CuckooCycleChallenge {
    fn write_challenge_to_stream(&self, stream: &mut BinaryStream) -> bool {
        let _ = stream.write_string(&self.prefix);
        stream.write_u64(self.max_nonce);
        true
    }

    fn read_challenge_from_stream(&mut self, stream: &mut BinaryStream) -> bool {
        let Ok(prefix) = stream.read_string() else {
            return false;
        };
        let Ok(max_nonce) = stream.read_u64() else {
            return false;
        };
        self.prefix = prefix;
        self.max_nonce = max_nonce;
        true
    }

    fn write_response_to_stream(&self, _stream: &mut BinaryStream) -> bool {
        // The server runtime does not mine; the client-side solver lives in
        // the connection library, so this is not implemented here.
        false
    }

    fn read_response_from_stream(&mut self, stream: &mut BinaryStream) -> bool {
        let Ok(key) = stream.read_string() else {
            return false;
        };
        if !key.starts_with(&self.prefix) {
            return false;
        }
        if stream.remaining() != PROOFSIZE * 4 {
            return false;
        }
        let mut nonces = [0u32; PROOFSIZE];
        for nonce in nonces.iter_mut() {
            let Ok(value) = stream.read_u32() else {
                return false;
            };
            *nonce = value;
        }
        verify_nonces(&nonces, key.as_bytes(), self.max_nonce)
    }
}

// ---------------------------------------------------------------------------
// Factories
// ---------------------------------------------------------------------------

/// The challenge type name for the `delay` challenge.
pub const CHALLENGE_DELAY: &str = "delay";
/// The challenge type name for the `fail` challenge.
pub const CHALLENGE_FAIL: &str = "fail";
/// The challenge type name for the `cuckoo_cycle` challenge.
pub const CHALLENGE_CUCKOO_CYCLE: &str = "cuckoo_cycle";

/// `DelayLoginChallengeFactory::DEFAULT_DURATION`.
const DEFAULT_DELAY_DURATION: f32 = 1.0;
/// `CuckooCycleLoginChallengeFactory::DEFAULT_EASINESS`.
const DEFAULT_CUCKOO_EASINESS: f64 = 50.0;

/// `LoginChallengeFactories` (`login_challenge_factory.cpp`): the three
/// registered factories and their configuration state.
///
/// Configuration mirrors the reference setters: the `delay` duration must be
/// positive, the `cuckoo_cycle` easiness must be in `(0, 100]`, and a rejected
/// configuration leaves the previous value in place.
#[derive(Clone, Debug)]
pub struct ChallengeFactories {
    delay_duration: f32,
    cuckoo_easiness: f64,
}

impl ChallengeFactories {
    /// Constructor with the default configuration.
    pub fn new() -> Self {
        Self {
            delay_duration: DEFAULT_DELAY_DURATION,
            cuckoo_easiness: DEFAULT_CUCKOO_EASINESS,
        }
    }

    /// `DelayLoginChallengeFactory::configure`: validate before committing.
    pub fn configure_delay(&mut self, duration: f32) -> bool {
        if duration <= 0.0 {
            return false;
        }
        self.delay_duration = duration;
        true
    }

    /// `CuckooCycleLoginChallengeFactory::configure`: validate before
    /// committing.
    pub fn configure_cuckoo(&mut self, easiness: f64) -> bool {
        if easiness <= 0.0 || easiness > 100.0 {
            return false;
        }
        self.cuckoo_easiness = easiness;
        true
    }

    /// `delay_duration()`.
    pub fn delay_duration(&self) -> f32 {
        self.delay_duration
    }

    /// `easiness()`.
    pub fn cuckoo_easiness(&self) -> f64 {
        self.cuckoo_easiness
    }

    /// `createChallenge`: create a challenge of the named type, or `None` when
    /// no such factory is registered.
    pub fn create(&self, name: &str) -> Option<Box<dyn LoginChallenge>> {
        match name {
            CHALLENGE_DELAY => Some(Box::new(DelayChallenge::new(self.delay_duration))),
            CHALLENGE_FAIL => Some(Box::new(FailChallenge)),
            CHALLENGE_CUCKOO_CYCLE => Some(Box::new(self.create_cuckoo())),
            _ => None,
        }
    }

    /// Build the `cuckoo_cycle` challenge for the configured easiness. The
    /// `maxNonce` is `easiness * SIZE / 100`, as the reference factory does.
    pub fn create_cuckoo(&self) -> CuckooCycleChallenge {
        CuckooCycleChallenge::new(
            self.random_prefix(),
            (self.cuckoo_easiness * SIZE as f64 / 100.0) as u64,
        )
    }

    /// A random 16-hex-digit prefix plus a colon, as the reference builds it
    /// from `RAND_bytes`. This port uses a `splitmix64` sequence for the
    /// entropy; the proof of work does not depend on the prefix being
    /// cryptographically random.
    fn random_prefix(&self) -> String {
        let mut state = next_random();
        let mut prefix = String::with_capacity(17);
        for _ in 0..8 {
            state = splitmix64(state);
            prefix.push_str(&format!("{:02x}", (state >> 56) as u8));
        }
        prefix.push(':');
        prefix
    }
}

impl Default for ChallengeFactories {
    fn default() -> Self {
        Self::new()
    }
}

/// A splitmix64 step: the xorshift-multiply-xor permutation.
fn splitmix64(mut state: u64) -> u64 {
    state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut z = state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// The next PRNG state: a counter seeded from the clock, so successive
/// challenges get distinct prefixes.
fn next_random() -> u64 {
    use std::cell::Cell;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static SEED: AtomicU64 = AtomicU64::new(0);
    thread_local! {
        static COUNTER: Cell<u64> = const { Cell::new(0) };
    }
    let clock = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9e37_79b9_7f4a_7c15);
    COUNTER.with(|c| {
        let n = c.get();
        c.set(n.wrapping_add(1));
        clock
            .wrapping_add(n)
            .wrapping_add(SEED.fetch_add(1, Ordering::Relaxed))
    })
}

// ---------------------------------------------------------------------------
// Negotiation state machine
// ---------------------------------------------------------------------------

/// A pending challenge attached to one client's login request.
pub struct PendingChallenge {
    challenge_type: String,
    challenge: Box<dyn LoginChallenge>,
}

impl PendingChallenge {
    /// `challengeType()`.
    pub fn challenge_type(&self) -> &str {
        &self.challenge_type
    }

    /// `pLoginChallenge()`.
    pub fn challenge(&self) -> &dyn LoginChallenge {
        self.challenge.as_ref()
    }
}

/// `ClientLoginRequest` challenge fields: whether the previous challenge was
/// answered incorrectly, and the outstanding challenge once one is issued.
#[derive(Default)]
pub struct LoginChallengeState {
    did_fail_challenge: bool,
    pending: Option<PendingChallenge>,
}

impl LoginChallengeState {
    /// `didFailChallenge()`.
    pub fn did_fail_challenge(&self) -> bool {
        self.did_fail_challenge
    }

    /// `pLoginChallenge()`.
    pub fn pending(&self) -> Option<&PendingChallenge> {
        self.pending.as_ref()
    }

    /// `setLoginChallenge`: record the issued challenge. The reference also
    /// resets the fail flag, since a new challenge supersedes a prior verdict.
    pub fn set_login_challenge(
        &mut self,
        challenge_type: String,
        challenge: Box<dyn LoginChallenge>,
    ) {
        self.did_fail_challenge = false;
        self.pending = Some(PendingChallenge {
            challenge_type,
            challenge,
        });
    }

    /// `clearChallenge`: drop the outstanding challenge. The fail flag is
    /// deliberately left alone, as the reference `clearChallenge` does.
    pub fn clear_challenge(&mut self) {
        self.pending = None;
    }
}

/// What `process_for_login_challenge` asks the caller to do next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChallengeAction {
    /// No challenge required; the login continues to the logon parameters.
    Proceed,
    /// A challenge reply must be sent. The outstanding challenge is in the
    /// request state's `pending`.
    SendChallenge,
    /// The login is rejected with `LOGIN_REJECTED_CHALLENGE_ERROR`.
    RejectChallengeError,
}

/// `LoginApp::processForLoginChallenge`: decide whether a challenge is issued
/// for a login request, and mutate the request state accordingly.
///
/// `state` is the per-address `ClientLoginRequest` challenge fields, `None`
/// when this is the first attempt from the address. `challenge_type` is
/// `Config::challengeType`.
pub fn process_for_login_challenge(
    state: &mut Option<LoginChallengeState>,
    challenge_type: &str,
    factories: &ChallengeFactories,
) -> ChallengeAction {
    if let Some(request) = state {
        if request.did_fail_challenge {
            return ChallengeAction::RejectChallengeError;
        }
        if request.pending.is_some() {
            // Still waiting on a response; this is probably a duplicate. Resend
            // the same challenge.
            return ChallengeAction::SendChallenge;
        }
        // The reply was already verified.
        return ChallengeAction::Proceed;
    }

    if challenge_type.is_empty() {
        // No challenge configured.
        return ChallengeAction::Proceed;
    }

    match factories.create(challenge_type) {
        Some(challenge) => {
            let mut request = LoginChallengeState::default();
            request.set_login_challenge(challenge_type.to_string(), challenge);
            *state = Some(request);
            ChallengeAction::SendChallenge
        }
        None => ChallengeAction::RejectChallengeError,
    }
}

/// The result of verifying a client's challenge response.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChallengeResponseOutcome {
    /// No login request for this address (probably timed out); the caller
    /// breaks its processing loop.
    TimedOut,
    /// The challenge was already verified; this is a re-send, and the login
    /// handler re-sends its cached result.
    AlreadyVerified,
    /// The response was rejected; the request now fails the challenge.
    Failed,
    /// The response was accepted; the challenge is cleared.
    Verified,
}

/// `LoginApp::challengeResponse`: read and verify a client's challenge reply.
///
/// The wire is the client's calculation duration as a `float`, then the
/// challenge response. On failure the request is marked `didFailChallenge`;
/// on success the challenge is cleared so the next login proceeds.
pub fn challenge_response(
    state: &mut Option<LoginChallengeState>,
    data: &mut BinaryStream,
) -> ChallengeResponseOutcome {
    let Some(request) = state else {
        // Probably timed out.
        return ChallengeResponseOutcome::TimedOut;
    };
    let Some(mut pending) = request.pending.take() else {
        // Challenge response already verified, this is a re-send; let the login
        // message handler re-send its cached result.
        return ChallengeResponseOutcome::AlreadyVerified;
    };

    // The client-reported calculation duration is not used for the decision.
    let _calculation_duration = data.read_f32().ok();

    let passed = pending.challenge.read_response_from_stream(data);
    if passed {
        // The challenge is cleared; the next login proceeds.
        ChallengeResponseOutcome::Verified
    } else {
        request.did_fail_challenge = true;
        ChallengeResponseOutcome::Failed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------------------------
    // A generic, textbook SipHash-2-4 (arbitrary key and message bytes) so the
    // specialized `siphash24` can be checked against an independent
    // implementation and the canonical reference vector.
    // ------------------------------------------------------------------

    fn generic_siphash24(key: &[u8; 16], input: &[u8]) -> u64 {
        let k0 = u64::from_le_bytes(key[..8].try_into().unwrap());
        let k1 = u64::from_le_bytes(key[8..].try_into().unwrap());
        let mut v0 = k0 ^ 0x736f_6d65_7073_6575;
        let mut v1 = k1 ^ 0x646f_7261_6e64_6f6d;
        let mut v2 = k0 ^ 0x6c79_6765_6e65_7261;
        let mut v3 = k1 ^ 0x7465_6462_7974_6573;
        let mut offset = 0;
        while offset + 8 <= input.len() {
            let m = u64::from_le_bytes(input[offset..offset + 8].try_into().unwrap());
            v3 ^= m;
            for _ in 0..2 {
                sip_round(&mut v0, &mut v1, &mut v2, &mut v3);
            }
            v0 ^= m;
            offset += 8;
        }
        let mut last = (input.len() as u64) << 56;
        for (i, &b) in input[offset..].iter().enumerate() {
            last |= (b as u64) << (8 * i);
        }
        v3 ^= last;
        for _ in 0..2 {
            sip_round(&mut v0, &mut v1, &mut v2, &mut v3);
        }
        v0 ^= last;
        v2 ^= 0xff;
        for _ in 0..4 {
            sip_round(&mut v0, &mut v1, &mut v2, &mut v3);
        }
        v0 ^ v1 ^ v2 ^ v3
    }

    #[test]
    fn generic_siphash_matches_the_canonical_vector() {
        // The SipHash-2-4 reference vectors (veorq/SipHash vectors.h): key
        // 00..0f, message 00..07. This anchors `sip_round` to the canonical
        // implementation; the specialized `siphash24` is the same round
        // schedule without the length-byte finalization (BigWorld's cuckoo
        // form), so it is not expected to equal the standard variant.
        let key: [u8; 16] = std::array::from_fn(|i| i as u8);
        let msg = [0u8, 1, 2, 3, 4, 5, 6, 7];
        assert_eq!(generic_siphash24(&key, &msg), 0x93f5_f579_9a93_2462);
    }

    // ------------------------------------------------------------------
    // Test-only cuckoo miner: finds a PROOFSIZE-cycle so the server verify can
    // be exercised with a genuine solution.
    // ------------------------------------------------------------------

    const MAX_PATH_LEN: usize = 8192;

    fn path(cuckoo: &[u32], mut u: u32, us: &mut [u32]) -> usize {
        let mut nu = 0usize;
        while u != 0 {
            nu += 1;
            if nu >= MAX_PATH_LEN {
                return usize::MAX;
            }
            us[nu] = u;
            u = cuckoo[u as usize];
        }
        nu
    }

    fn solution(
        ctx: &SipState,
        us: &[u32],
        nu: usize,
        vs: &[u32],
        nv: usize,
        easiness: u32,
    ) -> Option<Vec<u32>> {
        use std::collections::BTreeSet;
        let mut cycle = BTreeSet::new();
        cycle.insert((us[0], vs[0]));
        let mut i = nu;
        while i > 0 {
            i -= 1;
            cycle.insert((us[(i + 1) & !1], us[i | 1]));
        }
        let mut j = nv;
        while j > 0 {
            j -= 1;
            cycle.insert((vs[j | 1], vs[(j + 1) & !1]));
        }
        let mut sol = Vec::new();
        for nonce in 0..easiness {
            let (u, v) = sipedge(ctx, nonce);
            let edge = (1 + u, 1 + HALFSIZE as u32 + v);
            if cycle.remove(&edge) {
                sol.push(nonce);
            }
        }
        if sol.len() == PROOFSIZE {
            Some(sol)
        } else {
            None
        }
    }

    fn mine(header: &[u8], easiness: u32) -> Option<Vec<u32>> {
        let ctx = set_header(header);
        let mut cuckoo = vec![0u32; (SIZE + 1) as usize];
        let mut us = [0u32; MAX_PATH_LEN];
        let mut vs = [0u32; MAX_PATH_LEN];
        for nonce in 0..easiness {
            let (mut u0, mut v0) = sipedge(&ctx, nonce);
            u0 += 1;
            v0 += 1 + HALFSIZE as u32;
            let u = cuckoo[u0 as usize];
            let v = cuckoo[v0 as usize];
            if u == v0 || v == u0 {
                continue;
            }
            us[0] = u0;
            vs[0] = v0;
            let nu = path(&cuckoo, u, &mut us);
            let nv = path(&cuckoo, v, &mut vs);
            if nu == usize::MAX || nv == usize::MAX {
                return None;
            }
            if us[nu] == vs[nv] {
                let min = nu.min(nv);
                let mut nu = nu - min;
                let mut nv = nv - min;
                while us[nu] != vs[nv] {
                    nu += 1;
                    nv += 1;
                }
                let len = nu + nv + 1;
                if len == PROOFSIZE {
                    return solution(&ctx, &us, nu, &vs, nv, easiness);
                }
                continue;
            }
            if nu < nv {
                let mut k = nu;
                while k > 0 {
                    cuckoo[us[k] as usize] = us[k - 1];
                    k -= 1;
                }
                cuckoo[u0 as usize] = v0;
            } else {
                let mut k = nv;
                while k > 0 {
                    cuckoo[vs[k] as usize] = vs[k - 1];
                    k -= 1;
                }
                cuckoo[v0 as usize] = u0;
            }
        }
        None
    }

    // ------------------------------------------------------------------
    // cuckoo verify
    // ------------------------------------------------------------------

    #[test]
    fn verify_rejects_bad_wire_input() {
        let header = b"0123456789abcdef:0";
        // Non-ascending nonces are rejected before the graph is consulted.
        let mut nonces = [7u32; PROOFSIZE];
        nonces[0] = 1;
        assert!(!verify_nonces(&nonces, header, 1000));
        // A nonce at or above easiness is rejected.
        let mut nonces = [0u32; PROOFSIZE];
        for (i, n) in nonces.iter_mut().enumerate() {
            *n = i as u32 + 1;
        }
        assert!(!verify_nonces(&nonces, header, 5));
        // A strictly ascending run that is not a cycle is rejected.
        assert!(!verify_nonces(&nonces, header, 1000));
    }

    #[test]
    fn cuckoo_roundtrips_wire_format() {
        let factories = ChallengeFactories::new();
        let server = factories.create_cuckoo();
        let mut stream = BinaryStream::new();
        assert!(server.write_challenge_to_stream(&mut stream));
        let bytes = stream.as_bytes().to_vec();
        // Packed-length prefix (17 bytes of prefix text) + prefix + max_nonce.
        assert!(bytes.len() > 17 + 8);

        let mut client = factories.create_cuckoo();
        assert!(client.read_challenge_from_stream(&mut BinaryStream::from_slice(&bytes)));
        assert_eq!(server.prefix(), client.prefix());
        assert_eq!(server.max_nonce(), client.max_nonce());
        assert_eq!(server.prefix().len(), 17);
        assert_eq!(server.max_nonce(), 524_288); // 50% of 2^20
    }

    #[test]
    fn cuckoo_response_verifies_a_mined_solution() {
        let prefix = "0123456789abcdef:".to_string();
        // The default 50% easiness sits on the cuckoo phase-transition
        // boundary (the 2-core does not form at average degree 1), so mine at
        // 70% where 42-cycles are reliably present. The reference client loops
        // over keys because a single key at 50% is marginal.
        let max_nonce = 70 * SIZE as u32 / 100; // 70% of 2^20
        let key = format!("{prefix}0");
        let mut server = CuckooCycleChallenge::new(prefix, max_nonce as u64);

        let sol = mine(key.as_bytes(), max_nonce).expect("a 42-cycle exists at 70%");
        assert_eq!(sol.len(), PROOFSIZE);

        let mut response = BinaryStream::new();
        let _ = response.write_string(&key);
        for nonce in &sol {
            response.write_u32(*nonce);
        }
        assert!(server.read_response_from_stream(&mut response));
        assert!(response.at_end());

        // A single altered nonce must fail.
        let mut tampered = BinaryStream::new();
        let _ = tampered.write_string(&key);
        for nonce in &sol {
            tampered.write_u32(nonce.wrapping_add(1));
        }
        assert!(!server.read_response_from_stream(&mut tampered));

        // A wrong key (not prefixed by the challenge) must fail.
        let mut wrong_key = BinaryStream::new();
        let _ = wrong_key.write_string("ffffffffffffffff:0");
        for nonce in &sol {
            wrong_key.write_u32(*nonce);
        }
        assert!(!server.read_response_from_stream(&mut wrong_key));

        // A response with the wrong remaining length must fail.
        let mut short = BinaryStream::new();
        let _ = short.write_string(&key);
        for nonce in &sol[..sol.len() - 1] {
            short.write_u32(*nonce);
        }
        assert!(!server.read_response_from_stream(&mut short));
    }

    // ------------------------------------------------------------------
    // Factories
    // ------------------------------------------------------------------

    #[test]
    fn factory_configuration_validates_before_committing() {
        let mut factories = ChallengeFactories::new();
        // Rejecting leaves the previous value in place.
        assert!(!factories.configure_delay(0.0));
        assert_eq!(factories.delay_duration(), 1.0);
        assert!(factories.configure_delay(2.5));
        assert_eq!(factories.delay_duration(), 2.5);

        assert!(!factories.configure_cuckoo(0.0));
        assert!(!factories.configure_cuckoo(101.0));
        assert_eq!(factories.cuckoo_easiness(), 50.0);
        assert!(factories.configure_cuckoo(75.0));
        assert_eq!(factories.cuckoo_easiness(), 75.0);
        assert_eq!(factories.create_cuckoo().max_nonce(), 786_432); // 75% of 2^20
    }

    #[test]
    fn factory_creates_each_registered_type() {
        let factories = ChallengeFactories::new();
        assert!(factories.create(CHALLENGE_DELAY).is_some());
        assert!(factories.create(CHALLENGE_FAIL).is_some());
        assert!(factories.create(CHALLENGE_CUCKOO_CYCLE).is_some());
        assert!(factories.create("not_a_type").is_none());
        assert!(factories.create("").is_none());
    }

    // ------------------------------------------------------------------
    // Delay / fail interop
    // ------------------------------------------------------------------

    #[test]
    fn delay_challenge_roundtrips_between_server_and_client() {
        let mut server = DelayChallenge::new(1.5);
        let mut stream = BinaryStream::new();
        assert!(server.write_challenge_to_stream(&mut stream));
        let mut client = DelayChallenge::new(0.0);
        assert!(client.read_challenge_from_stream(&mut stream));
        assert_eq!(client.duration(), 1.5);

        let mut response = BinaryStream::new();
        assert!(client.write_response_to_stream(&mut response));
        assert!(server.read_response_from_stream(&mut response));

        // A wrong duration is rejected.
        let mut wrong = BinaryStream::new();
        wrong.write_f32(2.0);
        assert!(!server.read_response_from_stream(&mut wrong));
    }

    #[test]
    fn fail_challenge_always_rejects() {
        let mut server = FailChallenge;
        let mut stream = BinaryStream::new();
        assert!(server.write_challenge_to_stream(&mut stream));
        let mut client = FailChallenge;
        assert!(client.read_challenge_from_stream(&mut stream));
        let mut response = BinaryStream::new();
        assert!(client.write_response_to_stream(&mut response));
        assert!(!server.read_response_from_stream(&mut response));
    }

    // ------------------------------------------------------------------
    // Negotiation state machine
    // ------------------------------------------------------------------

    #[test]
    fn disabled_challenge_proceeds_and_creates_no_state() {
        let factories = ChallengeFactories::new();
        let mut state: Option<LoginChallengeState> = None;
        assert_eq!(
            process_for_login_challenge(&mut state, "", &factories),
            ChallengeAction::Proceed
        );
        assert!(state.is_none());
    }

    #[test]
    fn enabled_challenge_issues_and_resends() {
        let factories = ChallengeFactories::new();
        let mut state: Option<LoginChallengeState> = None;
        assert_eq!(
            process_for_login_challenge(&mut state, CHALLENGE_DELAY, &factories),
            ChallengeAction::SendChallenge
        );
        let request = state.as_ref().unwrap();
        assert_eq!(request.pending().unwrap().challenge_type(), CHALLENGE_DELAY);
        assert!(!request.did_fail_challenge());

        // A duplicate request resends the same challenge.
        assert_eq!(
            process_for_login_challenge(&mut state, CHALLENGE_DELAY, &factories),
            ChallengeAction::SendChallenge
        );
        assert_eq!(
            state.as_ref().unwrap().pending().unwrap().challenge_type(),
            CHALLENGE_DELAY
        );
    }

    #[test]
    fn a_failed_challenge_rejects_on_the_next_login() {
        let factories = ChallengeFactories::new();
        let mut state: Option<LoginChallengeState> = None;
        assert_eq!(
            process_for_login_challenge(&mut state, CHALLENGE_FAIL, &factories),
            ChallengeAction::SendChallenge
        );
        // The client answers; `fail` rejects.
        let mut response = BinaryStream::new();
        response.write_f32(0.1);
        assert_eq!(
            challenge_response(&mut state, &mut response),
            ChallengeResponseOutcome::Failed
        );
        assert!(state.as_ref().unwrap().did_fail_challenge());
        // The next login is rejected outright.
        assert_eq!(
            process_for_login_challenge(&mut state, CHALLENGE_FAIL, &factories),
            ChallengeAction::RejectChallengeError
        );
    }

    #[test]
    fn a_verified_challenge_proceeds_on_the_next_login() {
        let factories = ChallengeFactories::new();
        let mut state: Option<LoginChallengeState> = None;
        assert_eq!(
            process_for_login_challenge(&mut state, CHALLENGE_DELAY, &factories),
            ChallengeAction::SendChallenge
        );
        // The client answers with the same duration.
        let mut response = BinaryStream::new();
        response.write_f32(0.5); // calculation duration
        response.write_f32(factories.delay_duration());
        assert_eq!(
            challenge_response(&mut state, &mut response),
            ChallengeResponseOutcome::Verified
        );
        assert!(!state.as_ref().unwrap().did_fail_challenge());
        // The challenge is cleared, so the next login proceeds.
        assert_eq!(
            process_for_login_challenge(&mut state, CHALLENGE_DELAY, &factories),
            ChallengeAction::Proceed
        );
    }

    #[test]
    fn challenge_response_without_state_is_timed_out() {
        let mut state: Option<LoginChallengeState> = None;
        let mut response = BinaryStream::new();
        assert_eq!(
            challenge_response(&mut state, &mut response),
            ChallengeResponseOutcome::TimedOut
        );
    }

    #[test]
    fn a_repeated_response_after_verification_is_a_resend() {
        let factories = ChallengeFactories::new();
        let mut state: Option<LoginChallengeState> = None;
        process_for_login_challenge(&mut state, CHALLENGE_DELAY, &factories);
        let mut response = BinaryStream::new();
        response.write_f32(0.5);
        response.write_f32(factories.delay_duration());
        assert_eq!(
            challenge_response(&mut state, &mut response),
            ChallengeResponseOutcome::Verified
        );
        // A second response for the same source is a re-send.
        let mut response = BinaryStream::new();
        assert_eq!(
            challenge_response(&mut state, &mut response),
            ChallengeResponseOutcome::AlreadyVerified
        );
    }
}
