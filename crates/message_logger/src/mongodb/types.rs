//! The backend's type aliases (`mongodb/types.hpp`) plus the two seams the
//! port adds for testability: the wall clock (`gettimeofday`, `time(NULL)`)
//! and the UID lookup (`LogStorage::resolveUID`, `log_storage.cpp:163-220`).

use crate::mongodb::bson::BsonObj;

/// `CollList`: the collection names of one database, fully qualified
/// (`db.collection`).
pub type CollList = Vec<String>;

/// `BSONObjBuffer`: a batch of documents that can be written in one go
/// (`user_log_writer.cpp:136` inserts one as a single insert).
pub type BsonObjBuffer = Vec<BsonObj>;

/// The clock the backend reads — `gettimeofday` in `getCurrentTime`
/// (`log_storage.cpp:1074-1080`) and `time(NULL)` in
/// `getTimeStampSuffix`/`isCollectionExpired`
/// (`user_log_writer.cpp:630-643, 667-697`). A seam so fixtures can pin the
/// date a collection suffix carries.
pub trait Clock: Send + Sync {
    /// `time(NULL)` — wall seconds.
    fn now_secs(&self) -> i64;

    /// `gettimeofday` reduced to `tv_sec * 1000 + tv_usec / 1000` — wall
    /// milliseconds, the entry timestamp unit.
    fn now_millis(&self) -> u64;
}

/// The real clock.
pub struct WallClock;

impl Clock for WallClock {
    fn now_secs(&self) -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    }

    fn now_millis(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }
}

/// `LogStorage::resolveUID` queries BWMachined over the machine-guard
/// protocol for the username behind a uid, falling back to a broadcast and
/// then to the uid itself (`log_storage.cpp:163-220`). The network hop is
/// this seam: `None` is `reason != Mercury::REASON_SUCCESS`.
pub trait UidResolver: Send + Sync {
    /// The username for `uid` as seen from `ip`, or `None` when the query
    /// failed outright (the failure that aborts a user database's creation).
    fn resolve_uid(&self, uid: u16, ip: u32) -> Option<String>;
}

/// The outcome of the `gethostbyaddr` the backend uses in two places —
/// `Hostnames::getHostByAddr` (the fallback to the dotted quad) and
/// `HostnamesValidator::validateOneHostname` (the retry / degrade ladder).
#[derive(Clone)]
pub enum HostLookup {
    /// `ent` is non-`NULL`: `h_name`, truncated at the first dot by the
    /// caller.
    Resolved(String),
    /// `h_errno == HOST_NOT_FOUND`.
    HostNotFound,
    /// `h_errno == TRY_AGAIN`: retry the lookup.
    TryAgain,
    /// Any other `h_errno` (`NO_DATA`, `NO_RECOVERY`).
    Failed,
}

/// The reverse DNS hop. No network is reached in the port: fixtures supply
/// the answers, and the default answers `HostNotFound`, the branch the
/// backend takes when a host has no PTR record.
pub trait HostResolver: Send + Sync {
    /// `gethostbyaddr(&ip)`, reduced to its four outcomes.
    fn lookup(&self, ip: u32) -> HostLookup;
}

/// No resolver configured: every lookup reports `HostNotFound`.
pub struct NoHostResolver;

impl HostResolver for NoHostResolver {
    fn lookup(&self, _ip: u32) -> HostLookup {
        HostLookup::HostNotFound
    }
}

/// `inet_ntoa`: a network-order `IPAddress` as a dotted quad.
pub fn ip_to_string(ip: u32) -> String {
    format!(
        "{}.{}.{}.{}",
        (ip >> 24) & 0xff,
        (ip >> 16) & 0xff,
        (ip >> 8) & 0xff,
        ip & 0xff
    )
}

/// The fallback path the reference always reaches when machined answers but
/// names nobody: uid 0 means "root" (`log_storage.cpp:169-173`), any other
/// uid degrades to its decimal string
/// (`log_storage.cpp:203-211`). This is the behaviour when no machined
/// answer ever carries a username.
pub struct DefaultUidResolver;

impl UidResolver for DefaultUidResolver {
    fn resolve_uid(&self, uid: u16, _ip: u32) -> Option<String> {
        if uid == 0 {
            Some("root".to_string())
        } else {
            Some(uid.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ip_to_string_reads_a_dotted_quad() {
        assert_eq!(ip_to_string(0x7f00_0001), "127.0.0.1");
        assert_eq!(ip_to_string(0xc0a8_0102), "192.168.1.2");
        assert_eq!(ip_to_string(0), "0.0.0.0");
        assert_eq!(ip_to_string(0xffff_ffff), "255.255.255.255");
    }

    #[test]
    fn the_default_resolver_roots_uid_zero_and_degrades_the_rest() {
        assert_eq!(DefaultUidResolver.resolve_uid(0, 1), Some("root".into()));
        assert_eq!(DefaultUidResolver.resolve_uid(1000, 1), Some("1000".into()));
    }

    #[test]
    fn the_wall_clock_reads_forward() {
        let clock = WallClock;
        let secs = clock.now_secs();
        let millis = clock.now_millis();
        assert!(secs > 1_700_000_000, "sane epoch seconds: {secs}");
        assert!(
            millis >= (secs as u64) * 1000,
            "millis at or past the second"
        );
    }
}
