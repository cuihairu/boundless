//! The conditional MongoDB backend (`server/tools/message_logger/mongodb/`).
//!
//! In the reference this whole tree is compiled only when the build probes for
//! the legacy driver headers and finds them: `CMakeLists.message_logger.txt`
//! guards `mongo/client/dbclient.h`, appends `MONGODB_SRCS` and adds
//! `-DHAS_MONGODB_SUPPORT`, and `logger.cpp:718-734` builds a
//! `LogStorageMongoDB` only then — otherwise it reports "compiled on a system
//! that doesn't support MongoDB" and exits. This port carries the same
//! conditionality as a cargo feature: nothing here compiles unless the crate
//! is built with `--features mongodb`, so the default build has no MongoDB
//! code at all, as a driverless host's reference build has none.
//!
//! The driver itself is the seam: [`driver::MongoConnection`] plays the part of
//! `mongo::DBClientConnection` and [`driver::InMemoryStore`] is the
//! implementation the fixtures run against. Every document the storage writes
//! is encoded to real BSON bytes ([`bson::BsonObj::encode`]) and decoded again
//! on the way back, so the byte layout — not just the field names — is what
//! the tests check.
//!
//! ## Registered deviations
//!
//! * **The worker thread is the caller's thread.** The reference parks all
//!   database work on one background thread (`startThreads("MongoDBTaskManager",
//!   1, …)`, `log_storage.cpp:153`), which is what makes its
//!   `ConnectionThreadData` lock-free. A single thread is untestable, so the
//!   port runs the task queue inline on the caller's thread and takes the one
//!   mutex the reference needed anyway. Same task order, same results.
//! * **`abortFurtherProcessing` is not inverted.** The reference clears its own
//!   abort flag (`connection_thread_data.cpp:619-626` writes `= false`), so the
//!   unrecoverable-error path that ends in `exit(EXIT_FAILURE)` can never fire
//!   there. The port sets the flag as documented; tests assert the flag.
//! * **Expiry arithmetic uses UTC, like the reference.** `timegm` is UTC by
//!   construction, so no local-timezone claim is made.
//! * **One connection, not a thread pool.** `LogStorageMongoDB::init` waits
//!   for the worker's connection before declaring success; the port runs
//!   `on_start` inline and fails the init when the socket never came up.
//!   `tick` reports the unrecoverable-error state instead of calling
//!   `exit(EXIT_FAILURE)` — the caller decides.
//! * **Reconnect attempts are bounded by the driver.** The reference
//!   retries forever with `waitBetweenReconnects_` sleeps; the seam's
//!   reconnection policy is the fixture's, with `0` meaning unbounded.
//! * **UTF-8 sanitisation is a no-op.** `createBson` re-sanitises old
//!   versions' messages (`isValidUtf8Str`), which Rust's `String` makes
//!   unreachable — every string is valid UTF-8 by construction.

pub mod bson;
pub mod collections;
pub mod connection;
pub mod constants;
pub mod driver;
pub mod metadata;
pub mod storage;
pub mod tasks;
pub mod types;
pub mod user_log;
