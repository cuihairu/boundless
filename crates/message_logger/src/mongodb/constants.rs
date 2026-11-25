//! The backend's constants (`mongodb/constants.hpp`), verbatim.
//!
//! Database and collection names are part of the on-disk contract — a query
//! tool reads them back — so they are pinned here rather than scattered.

/// `ADMIN_DB_NAME`: the database every server command runs against.
pub const ADMIN_DB_NAME: &str = "admin";
/// `BW_COMMON_DB_NAME`: the database shared by every user of one logger.
pub const BW_COMMON_DB_NAME: &str = "bw_ml_common";

// ---------------------------------------------------------------------------
// The per-user database and its entry collections
// ---------------------------------------------------------------------------

/// `USER_DB_PREFIX`, followed by the username and the logger ID.
pub const USER_DB_PREFIX: &str = "bw_ml_user_";
/// `USER_COLL_ENTRIES_PREFIX`: an entry collection is `entries_<suffix>`.
pub const USER_COLL_ENTRIES_PREFIX: &str = "entries_";
/// The length of an entry collection's `YYYYMMDDHHMMSS` suffix
/// (`user_log_writer.cpp:25`).
pub const COLL_SUFFIX_LENGTH: usize = 14;

/// `ENTRIES_KEY_TIMESTAMP`.
pub const ENTRIES_KEY_TIMESTAMP: &str = "ts";
/// `ENTRIES_KEY_COUNTER`.
pub const ENTRIES_KEY_COUNTER: &str = "cnt";
/// `ENTRIES_KEY_COMPONENT`.
pub const ENTRIES_KEY_COMPONENT: &str = "cpt";
/// `ENTRIES_KEY_APP_ID`.
pub const ENTRIES_KEY_APP_ID: &str = "aid";
/// `ENTRIES_KEY_PID`.
pub const ENTRIES_KEY_PID: &str = "pid";
/// `ENTRIES_KEY_CATEGORY`.
pub const ENTRIES_KEY_CATEGORY: &str = "ctg";
/// `ENTRIES_KEY_SOURCE`.
pub const ENTRIES_KEY_SOURCE: &str = "src";
/// `ENTRIES_KEY_SEVERITY`.
pub const ENTRIES_KEY_SEVERITY: &str = "svt";
/// `ENTRIES_KEY_HOST`.
pub const ENTRIES_KEY_HOST: &str = "host";
/// `ENTRIES_KEY_FMT_STR`.
pub const ENTRIES_KEY_FMT_STR: &str = "fmt";
/// `ENTRIES_KEY_MESSAGE`.
pub const ENTRIES_KEY_MESSAGE: &str = "msg";
/// `ENTRIES_KEY_META_DATA`.
pub const ENTRIES_KEY_META_DATA: &str = "md";
/// `ENTRIES_KEY_SHARD_KEY` — appended only in a sharded deployment.
pub const ENTRIES_KEY_SHARD_KEY: &str = "sk";

/// `USER_COLL_UID`: the collection holding one user's uid.
pub const USER_COLL_UID: &str = "uid";
/// `USER_UID_KEY`.
pub const USER_UID_KEY: &str = "uid";

/// `SERVER_STARTS_COLL_NAME` — a capped collection of server start times.
pub const SERVER_STARTS_COLL_NAME: &str = "server_start_ups";
/// `SERVER_STARTS_KEY_TIME`.
pub const SERVER_STARTS_KEY_TIME: &str = "time";
/// `SERVER_STARTS_COUNT_LIMIT`: at most this many start records.
pub const SERVER_STARTS_COUNT_LIMIT: u32 = 50;
/// `SERVER_STARTS_SIZE_LIMIT`: in bytes.
pub const SERVER_STARTS_SIZE_LIMIT: u32 = 5000;

/// `COMPONENTS_COLL_NAME`: one user's component records.
pub const COMPONENTS_COLL_NAME: &str = "components";
/// `COMPONENTS_KEY_HOST`.
pub const COMPONENTS_KEY_HOST: &str = "host";
/// `COMPONENTS_KEY_APPID`.
pub const COMPONENTS_KEY_APPID: &str = "aid";
/// `COMPONENTS_KEY_PID`.
pub const COMPONENTS_KEY_PID: &str = "pid";
/// `COMPONENTS_KEY_COMPONENT`.
pub const COMPONENTS_KEY_COMPONENT: &str = "cpt";
/// `COMPONENTS_UID` — only in the in-memory record, never written.
pub const COMPONENTS_UID: &str = "uid";
/// `COMPONENTS_USERNAME` — only in the in-memory record, never written.
pub const COMPONENTS_USERNAME: &str = "username";

// ---------------------------------------------------------------------------
// The common database's collections
// ---------------------------------------------------------------------------

/// `HOST_NAMES_COLL`.
pub const HOST_NAMES_COLL: &str = "hosts";
/// `HOST_NAMES_KEY_IP`.
pub const HOST_NAMES_KEY_IP: &str = "ip";
/// `HOST_NAMES_KEY_HOSTNAME`.
pub const HOST_NAMES_KEY_HOSTNAME: &str = "hostname";

/// `FMT_STRS_COLL`.
pub const FMT_STRS_COLL: &str = "format_strings";
/// `FMT_STRS_KEY_ID`.
pub const FMT_STRS_KEY_ID: &str = "id";
/// `FMT_STRS_KEY_FMT_STR`.
pub const FMT_STRS_KEY_FMT_STR: &str = "fmt_str";

/// `CATEGORIES_COLL`.
pub const CATEGORIES_COLL: &str = "categories";
/// `CATEGORIES_KEY_ID`.
pub const CATEGORIES_KEY_ID: &str = "id";
/// `CATEGORIES_KEY_NAME`.
pub const CATEGORIES_KEY_NAME: &str = "name";

/// `COMPONENTS_COLL`: the shared component-name table.
pub const COMPONENTS_COLL: &str = "components";
/// `COMPONENTS_KEY_ID`.
pub const COMPONENTS_KEY_ID: &str = "id";
/// `COMPONENTS_KEY_NAME`.
pub const COMPONENTS_KEY_NAME: &str = "name";

/// `SOURCES_COLL`.
pub const SOURCES_COLL: &str = "sources";
/// `SOURCES_KEY_ID`.
pub const SOURCES_KEY_ID: &str = "id";
/// `SOURCES_KEY_NAME`.
pub const SOURCES_KEY_NAME: &str = "name";

/// `SEVERITIES_COLL`.
pub const SEVERITIES_COLL: &str = "severities";
/// `SEVERITIES_KEY_LEVEL`.
pub const SEVERITIES_KEY_LEVEL: &str = "level";
/// `SEVERITIES_KEY_NAME`.
pub const SEVERITIES_KEY_NAME: &str = "name";

/// `VERSION_COLL`.
pub const VERSION_COLL: &str = "schema_version";
/// `VERSION_KEY_VERSION`.
pub const VERSION_KEY_VERSION: &str = "version";

/// `SCHEMA_VERSION`: 1 was the BW2.9 layout, 2 the split collections of BW2.10.
pub const SCHEMA_VERSION: u32 = 2;

// ---------------------------------------------------------------------------
// Connection and buffering defaults
// ---------------------------------------------------------------------------

/// `TCP_TIME_OUT`, in seconds (the driver takes a second-based socket timeout).
pub const TCP_TIME_OUT: u32 = 5;
/// `WAIT_BETWEEN_RECONNECTS`, in seconds. Zero skips the sleep.
pub const WAIT_BETWEEN_RECONNECTS: u16 = 1;
/// `MAX_LOG_BUFFER_SIZE`: the buffer size that triggers a flush.
pub const MAX_LOG_BUFFER_SIZE: u32 = 1024;
/// `RESERVE_LOG_BUFFER_SIZE`: a buffer's reserved capacity.
pub const RESERVE_LOG_BUFFER_SIZE: usize = 128;
/// `LOG_FLUSH_INTERVAL`, in milliseconds (half a second).
pub const LOG_FLUSH_INTERVAL: u32 = 500;
/// `EXPIRE_LOGS_DAYS`.
pub const EXPIRE_LOGS_DAYS: u32 = 7;

/// `LOGGER_ID_SEPARATOR`: valid in a database name, invalid in a username.
pub const LOGGER_ID_SEPARATOR: &str = "#";
/// `LOGGER_ID_NA`: the logger ID when none is configured.
pub const LOGGER_ID_NA: &str = "";
/// `CONFIG_NAME_MONGO`: the config section the backend reads.
pub const CONFIG_NAME_MONGO: &str = "mongodb";

/// The component name whose log marks a server start
/// (`BW_TOOLS_COMPONENT_NAME`, `constants.hpp:26`).
pub const BW_TOOLS_COMPONENT_NAME: &str = "Tools";
/// The message text that follows it (`constants.hpp:27`).
pub const BW_TOOLS_SERVER_START_MSG: &str = "Starting server";
