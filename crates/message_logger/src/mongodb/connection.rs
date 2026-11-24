//! The background worker's state (`mongodb/connection_info.hpp` +
//! `mongodb/connection_thread_data.{hpp,cpp}`).
//!
//! The reference parks every database operation on one background thread
//! whose `ConnectionThreadData` owns its own connection, the user log
//! writers, and the three status flags the main thread polls. The port runs
//! that same state inline on the caller's thread (the registered worker
//! deviation), so the flags live behind one mutex exactly as the
//! reference's `statusFlagsMutex_` does.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use crate::mongodb::constants;
use crate::mongodb::driver::{CommandReply, MongoConnection};
use crate::mongodb::types::Clock;
use crate::mongodb::types::CollList;
use crate::mongodb::user_log::UserLogWriter;

/// `DBTaskStatus` (`connection_thread_data.hpp:22-27`): how a queued record
/// write ended.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DBTaskStatus {
    /// `DB_TASK_SUCCESS`.
    Success,
    /// `DB_TASK_CONNECTION_ERROR`: the socket died mid-write.
    ConnectionError,
    /// `DB_TASK_GENERAL_ERROR`: the server refused the write.
    GeneralError,
}

/// `ConnectionInfo` (`connection_info.hpp:16-30`): everything
/// `connectToDB` needs. The defaults are `TCP_TIME_OUT` and
/// `WAIT_BETWEEN_RECONNECTS`; `initFromConfig` fills the rest.
#[derive(Clone, Debug)]
pub struct ConnectionInfo {
    /// `dbHost_`.
    pub db_host: String,
    /// `dbPort_`.
    pub db_port: u16,
    /// `dbUser_`.
    pub db_user: String,
    /// `dbPasswd_`.
    pub db_passwd: String,
    /// `tcpTimeout_`, in seconds.
    pub tcp_timeout: u32,
    /// `waitBetweenReconnects_`, in seconds; zero skips the wait.
    pub wait_between_reconnects: u16,
}

impl Default for ConnectionInfo {
    fn default() -> Self {
        Self {
            db_host: String::new(),
            db_port: 0,
            db_user: String::new(),
            db_passwd: String::new(),
            tcp_timeout: constants::TCP_TIME_OUT,
            wait_between_reconnects: constants::WAIT_BETWEEN_RECONNECTS,
        }
    }
}

/// `LogStorageMongoDB::connectToDB` (`log_storage.cpp:644-681`): connect,
/// then authenticate against the admin database. A static method in the
/// reference because both the main connection and the worker's call it.
pub fn connect_to_db<C: MongoConnection + 'static>(connection: &C, info: &ConnectionInfo) -> bool {
    connection.set_so_timeout(info.tcp_timeout);

    if !connection.connect(&info.db_host, info.db_port) {
        return false;
    }

    if !connection.auth(constants::ADMIN_DB_NAME, &info.db_user, &info.db_passwd) {
        return false;
    }

    true
}

/// `ConnectionThreadData::getCollectionNames`
/// (`connection_thread_data.cpp:639-684`): the BWT-28917 workaround, which
/// reads collection names without crashing on an unreachable server. The
/// driver seam carries the workaround inside
/// [`MongoConnection::collection_names`]; here it is only the names list,
/// fully qualified, `$`-containing entries skipped.
pub fn get_collection_names<C: MongoConnection + 'static>(
    connection: &C,
    db_name: &str,
) -> Option<CollList> {
    connection.collection_names(db_name).ok()
}

/// The three flags the main thread polls, behind the one mutex
/// (`statusFlagsMutex_`).
#[derive(Default)]
struct StatusFlags {
    connected: bool,
    allow_reconnect: bool,
    abort_further_processing: bool,
}

/// `ConnectionThreadData` (`connection_thread_data.hpp:36-97`).
pub struct ConnectionThreadData<C: MongoConnection + 'static> {
    /// `conn_` — the worker's own connection.
    connection: Arc<C>,
    /// `userLogWriters_`: uid → writer.
    user_log_writers: BTreeMap<u16, UserLogWriter<C>>,
    /// `loggerID_`.
    logger_id: String,
    /// `connectionInfo_`.
    connection_info: ConnectionInfo,
    /// `expireSeconds_`.
    expire_seconds: u64,
    /// `isCluster_`.
    is_cluster: AtomicBool,
    /// `statusFlagsMutex_` + the three flags.
    status: Mutex<StatusFlags>,
    /// The bound on reconnect attempts in one `reconnectIfNecessary` walk.
    /// Zero means unbounded, which is the reference's `while (true)` loop
    /// (safe there: a separate thread can spin). The port runs on the
    /// caller's thread, so tests that exercise a failing reconnect set a
    /// small bound.
    max_reconnect_attempts: AtomicU32,
    /// The clock the user log writers read.
    clock: Arc<dyn Clock>,
}

impl<C: MongoConnection + 'static> ConnectionThreadData<C> {
    /// The reference constructor (`connection_thread_data.cpp:28-40`).
    pub fn new(
        connection: Arc<C>,
        connection_info: ConnectionInfo,
        expire_seconds: u64,
        logger_id: String,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            connection,
            user_log_writers: BTreeMap::new(),
            logger_id,
            connection_info,
            expire_seconds,
            is_cluster: AtomicBool::new(false),
            status: Mutex::new(StatusFlags {
                connected: false,
                allow_reconnect: true,
                abort_further_processing: false,
            }),
            max_reconnect_attempts: AtomicU32::new(0),
            clock,
        }
    }

    /// `onStart` (`connection_thread_data.cpp:50-96`): connect, learn the
    /// cluster status, reopen every existing user log writer, run the
    /// initial purge, and only then report connected.
    pub fn on_start(&mut self) -> bool {
        if !connect_to_db(self.connection.as_ref(), &self.connection_info) {
            return false;
        }

        if !self.init_cluster_status() {
            return false;
        }

        if !self.init_user_log_writers() {
            return false;
        }

        // Initial purge of expired data.
        for writer in self.user_log_writers.values_mut() {
            if let Err(crate::mongodb::driver::MongoError::Connection(_)) =
                writer.purge_expired_colls(self.expire_seconds)
            {
                self.set_connected(false);
                return false;
            }
        }

        // Inform the main thread that it is safe to start its flush timer.
        self.set_connected(true);

        true
    }

    /// `initClusterStatus` (`connection_thread_data.cpp:102-149`): an
    /// `ismaster` whose `msg` names `isdbgrid` means a sharded cluster.
    fn init_cluster_status(&mut self) -> bool {
        let mut command = crate::mongodb::bson::BsonObj::new();
        command.append("ismaster", crate::mongodb::bson::Bson::Int(1));

        match self
            .connection
            .run_command(constants::ADMIN_DB_NAME, &command)
        {
            Ok(CommandReply::IsMaster { msg }) => {
                self.is_cluster
                    .store(msg.contains("isdbgrid"), Ordering::Relaxed);
                true
            }
            Ok(_) => false,
            Err(crate::mongodb::driver::MongoError::Connection(_)) => {
                self.set_connected(false);
                false
            }
            Err(_) => false,
        }
    }

    /// `initUserLogWriters` (`connection_thread_data.cpp:155-245`): reopen a
    /// writer per existing `bw_ml_user_<user>#<loggerID>` database. A user
    /// database without a readable uid is dropped when it holds no
    /// collections (an artifact of databases materialising on first use)
    /// and is fatal otherwise.
    fn init_user_log_writers(&mut self) -> bool {
        let user_databases = match self.get_user_databases() {
            Some(map) => map,
            None => return false,
        };

        for (username, db_name) in user_databases {
            let uid = match self.get_uid_from_db(&db_name) {
                Some(uid) => uid,
                None => {
                    // Check whether there are logs. No logs: drop the empty
                    // database; logs without a uid: fatal.
                    let coll_list = match get_collection_names(&*self.connection, &db_name) {
                        Some(list) => list,
                        None => return false,
                    };

                    if coll_list.is_empty() {
                        self.connection.drop_database(&db_name);
                        continue;
                    }

                    return false;
                }
            };

            self.user_log_writers.insert(
                uid,
                UserLogWriter::new(
                    Arc::clone(&self.connection),
                    uid,
                    username,
                    self.logger_id.clone(),
                    Arc::clone(&self.clock),
                ),
            );
        }

        true
    }

    /// `getUserLogWriter` (`connection_thread_data.cpp:251-289`): the
    /// lookup-or-create. A fresh writer initialises its database on first
    /// use; a failed initialisation leaves the map entry absent and the
    /// caller empty-handed.
    pub fn get_user_log_writer(
        &mut self,
        uid: u16,
        username: &str,
    ) -> Option<&mut UserLogWriter<C>> {
        if !self.user_log_writers.contains_key(&uid) {
            let mut writer = UserLogWriter::new(
                Arc::clone(&self.connection),
                uid,
                username.to_string(),
                self.logger_id.clone(),
                Arc::clone(&self.clock),
            );

            let is_cluster = self.get_is_cluster();
            match writer.init_db(is_cluster) {
                Ok(true) => {
                    self.user_log_writers.insert(uid, writer);
                }
                Ok(false) => {
                    return None;
                }
                Err(crate::mongodb::driver::MongoError::Connection(_)) => {
                    self.set_connected(false);
                    return None;
                }
                Err(_) => {
                    return None;
                }
            }
        }

        self.user_log_writers.get_mut(&uid)
    }

    /// `rollUserLogs` (`connection_thread_data.cpp:292-313`).
    pub fn roll_user_logs(&mut self) -> bool {
        let expire_seconds = self.expire_seconds;
        let mut ok = true;
        let mut lost_connection = false;

        for writer in self.user_log_writers.values_mut() {
            match writer.roll_and_purge(expire_seconds) {
                Ok(passed) => ok = ok && passed,
                Err(crate::mongodb::driver::MongoError::Connection(_)) => {
                    lost_connection = true;
                    ok = false;
                }
                Err(_) => ok = false,
            }
        }
        if lost_connection {
            self.set_connected(false);
        }

        ok
    }

    /// `getUserDatabases` (`connection_thread_data.cpp:319-379`): the
    /// `listDatabases` reply filtered to this logger's user databases,
    /// username → database name.
    fn get_user_databases(&self) -> Option<BTreeMap<String, String>> {
        let mut command = crate::mongodb::bson::BsonObj::new();
        command.append("listDatabases", crate::mongodb::bson::Bson::Int(1));

        let databases = match self
            .connection
            .run_command(constants::ADMIN_DB_NAME, &command)
        {
            Ok(CommandReply::ListDatabases(databases)) => databases,
            Ok(_) => return None,
            Err(crate::mongodb::driver::MongoError::Connection(_)) => {
                self.set_connected(false);
                return None;
            }
            Err(_) => return None,
        };

        let prefix = constants::USER_DB_PREFIX;
        let suffix = format!("{}{}", constants::LOGGER_ID_SEPARATOR, self.logger_id);

        let mut map = BTreeMap::new();
        for db_name in databases {
            let Some(user) = db_name
                .strip_prefix(prefix)
                .filter(|rest| rest.len() > suffix.len() && rest.ends_with(&suffix))
            else {
                continue;
            };

            let username = user[..user.len() - suffix.len()].to_string();
            map.insert(username, db_name);
        }

        Some(map)
    }

    /// `getUidFromDB` (`connection_thread_data.cpp:385-434`): the one
    /// document in `<db>.uid`, read for its `uid` field.
    fn get_uid_from_db(&self, db_name: &str) -> Option<u16> {
        let namespace = format!("{}.{}", db_name, constants::USER_COLL_UID);

        let rows = match self.connection.query(&namespace, &Default::default()) {
            Ok(rows) => rows,
            Err(crate::mongodb::driver::MongoError::Connection(_)) => {
                self.set_connected(false);
                return None;
            }
            Err(_) => return None,
        };

        let record = rows.into_iter().next()?;

        if !record.has_field(constants::USER_UID_KEY) {
            return None;
        }

        Some(record.get_int(constants::USER_UID_KEY) as u16)
    }

    /// `addRecordToCollection` (`connection_thread_data.cpp:440-476`).
    pub fn add_record_to_collection(
        &self,
        collection_name: &str,
        new_record: &crate::mongodb::bson::BsonObj,
    ) -> DBTaskStatus {
        match self.connection.insert(collection_name, new_record) {
            Ok(()) => DBTaskStatus::Success,
            Err(crate::mongodb::driver::MongoError::Connection(_)) => {
                self.set_connected(false);
                DBTaskStatus::ConnectionError
            }
            Err(_) => DBTaskStatus::GeneralError,
        }
    }

    /// `updateRecordInCollection` (`connection_thread_data.cpp:482-521`).
    pub fn update_record_in_collection(
        &self,
        collection_name: &str,
        existing_record: &crate::mongodb::bson::BsonObj,
        new_record: &crate::mongodb::bson::BsonObj,
    ) -> DBTaskStatus {
        match self
            .connection
            .update(collection_name, existing_record, new_record)
        {
            Ok(()) => DBTaskStatus::Success,
            Err(crate::mongodb::driver::MongoError::Connection(_)) => {
                self.set_connected(false);
                DBTaskStatus::ConnectionError
            }
            Err(_) => DBTaskStatus::GeneralError,
        }
    }

    /// `reconnectIfNecessary` (`connection_thread_data.cpp:541-576`): the
    /// retry loop every task prologue walks. The reference loops forever on
    /// a separate thread; the port bounds the loop with
    /// [`Self::set_max_reconnect_attempts`] (zero keeps it unbounded) so an
    /// inline caller cannot hang.
    pub fn reconnect_if_necessary(&self) -> bool {
        if self.is_connected() {
            return true;
        }

        let bound = self.max_reconnect_attempts.load(Ordering::Relaxed);
        let mut attempts = 0u32;

        loop {
            if self.should_abort_reconnect() {
                return false;
            }

            if connect_to_db(self.connection.as_ref(), &self.connection_info) {
                self.set_connected(true);
                return true;
            }

            attempts += 1;
            if bound != 0 && attempts >= bound {
                return false;
            }

            if self.connection_info.wait_between_reconnects != 0 {
                std::thread::sleep(std::time::Duration::from_secs(
                    self.connection_info.wait_between_reconnects as u64,
                ));
            }
        }
    }

    /// `getIsCluster`.
    pub fn get_is_cluster(&self) -> bool {
        self.is_cluster.load(Ordering::Relaxed)
    }

    /// `setConnected`.
    pub fn set_connected(&self, connected: bool) {
        self.status.lock().unwrap().connected = connected;
    }

    /// `isConnected`.
    pub fn is_connected(&self) -> bool {
        self.status.lock().unwrap().connected
    }

    /// `stopReconnectAttempts`.
    pub fn stop_reconnect_attempts(&self) {
        self.status.lock().unwrap().allow_reconnect = false;
    }

    /// `shouldAbortReconnect`.
    pub fn should_abort_reconnect(&self) -> bool {
        !self.status.lock().unwrap().allow_reconnect
    }

    /// `abortFurtherProcessing`. The reference clears its own flag here
    /// (`connection_thread_data.cpp:619-626` writes `= false`), which makes
    /// the unrecoverable-error path dead code; the port sets the flag as
    /// the name and every call site describe.
    pub fn abort_further_processing(&self) {
        self.status.lock().unwrap().abort_further_processing = true;
    }

    /// `shouldAbortFurtherProcessing`.
    pub fn should_abort_further_processing(&self) -> bool {
        self.status.lock().unwrap().abort_further_processing
    }

    /// The reconnect-attempt bound tests use; zero is the reference's
    /// unbounded loop.
    pub fn set_max_reconnect_attempts(&self, attempts: u32) {
        self.max_reconnect_attempts
            .store(attempts, Ordering::Relaxed);
    }
}
