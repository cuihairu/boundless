//! `mongodb/log_storage.hpp/cpp`: the MongoDB log storage.
//!
//! The reference splits the work between the caller's thread (parse, build
//! the entry document, buffer per user) and a background thread (the task
//! queue writing to the database). The port keeps the same split in shape,
//! with the worker running inline via [`TaskManager::tick`] — the
//! registered worker deviation of this module tree.
//!
//! Every write through this type crosses the driver as real BSON wire
//! bytes: documents are encoded before the in-memory fixture stores them,
//! and read back by decoding.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::component_message::ComponentMessage;
use crate::file_logger::LOG_METADATA_VERSION;
use crate::interpolator::{self, LogStringInterpolator};
use crate::message_header::MessageHeader;
use crate::mongodb::bson::{Bson, BsonObj};
use crate::mongodb::collections::{
    CategoriesMongoDB, FormatStringsMongoDB, GetHostResult, HostnamesMongoDB,
    LogComponentNamesMongoDB, ValidatorStatus,
};
use crate::mongodb::connection::{
    ConnectionInfo, ConnectionThreadData, connect_to_db, get_collection_names,
};
use crate::mongodb::constants;
use crate::mongodb::driver::MongoConnection;
use crate::mongodb::metadata::build_metadata_bson;
use crate::mongodb::tasks::{Task, TaskManager, priority};
use crate::mongodb::types::{Clock, CollList, UidResolver};
use crate::mongodb::user_log::UserLogBuffer;
use crate::registry::Address;
use crate::stream::MemoryIStream;
use crate::{MessageSource, OLDEST_VERSION_WITHOUT_SOURCE_ENUM, message_priority};

/// `LogStorage::AddLogMessageResult`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AddLogMessageResult {
    /// `LOG_ADDITION_SUCCESS`.
    Success,
    /// `LOG_ADDITION_IGNORED`.
    Ignored,
    /// `LOG_ADDITION_FAILED`.
    Failed,
}

/// `LogStorageMongoDB`.
pub struct LogStorageMongoDB<C: MongoConnection + 'static> {
    /// The clock behind `getCurrentTime` — a seam so fixtures pin time.
    clock: Arc<dyn Clock>,
    /// `writeToStdout_`: echo every written entry's JSON to stdout.
    write_to_stdout: bool,
    /// `userLogs_`: the per-user buffers, keyed by uid.
    user_logs: BTreeMap<u16, UserLogBuffer>,
    /// `loggerID_`.
    logger_id: String,
    /// `commonDBName_`: the shared database, suffixed with the logger id.
    common_db_name: String,
    /// `conn_`.
    connection: Arc<C>,
    /// `connectionInfo_`.
    connection_info: ConnectionInfo,
    /// `pConnectionThreadData_`: created by [`Self::init`], `NULL` before
    /// that in the reference — hence the `Option`.
    thread_data: Option<ConnectionThreadData<C>>,
    /// `mongoDBTaskMgr_`.
    task_mgr: TaskManager,
    /// `canAddLogs_`: logging is suspended while the database is down.
    can_add_logs: bool,
    /// `hostnames_`.
    hostnames: HostnamesMongoDB<C>,
    /// `componentNames_`.
    component_names: LogComponentNamesMongoDB<C>,
    /// `formatStrings_`.
    format_strings: FormatStringsMongoDB<C>,
    /// `categories_`.
    categories: CategoriesMongoDB<C>,
    /// `appIDMap_`.
    app_id_map: BTreeMap<Address, u32>,
    /// `componentMap_`.
    component_map: BTreeMap<Address, BsonObj>,
    /// `logger_.shouldLogPriority` — the reference delegates to the owning
    /// `Logger`'s criticality threshold; the port carries it here (default
    /// `TRACE`, i.e. everything logs).
    priority_threshold: u8,
    /// `currentSecond_`.
    current_second: u64,
    /// `currentCounter_`.
    current_counter: u32,
    /// `currentBufSize_`.
    current_buf_size: u32,
    /// `maxBufSize_`.
    max_buf_size: u32,
    /// `flushInterval_`, milliseconds.
    flush_interval: u32,
    /// `expireLogsDays_`.
    expire_logs_days: u32,
    /// `LogStorage::resolveUID`'s machined hop — the seam.
    uid_resolver: Arc<dyn UidResolver>,
}

impl<C: MongoConnection + 'static> LogStorageMongoDB<C> {
    /// The constructor's member defaults (`log_storage.cpp:35-60`).
    pub fn new(
        logger_id: &str,
        connection: Arc<C>,
        uid_resolver: Arc<dyn UidResolver>,
        host_resolver: Arc<dyn crate::mongodb::types::HostResolver>,
        clock: Arc<dyn Clock>,
        write_to_stdout: bool,
    ) -> Self {
        let logger_id = if logger_id.is_empty() {
            constants::LOGGER_ID_NA.to_string()
        } else {
            logger_id.to_string()
        };
        let common_db_name = format!(
            "{}{}{}",
            constants::BW_COMMON_DB_NAME,
            constants::LOGGER_ID_SEPARATOR,
            logger_id
        );
        let task_mgr = TaskManager::new();
        let hostnames = HostnamesMongoDB::new(
            task_mgr.clone(),
            Arc::clone(&connection),
            format!("{}.{}", common_db_name, constants::HOST_NAMES_COLL),
            host_resolver,
        );
        let component_names = LogComponentNamesMongoDB::new(
            task_mgr.clone(),
            Arc::clone(&connection),
            format!("{}.{}", common_db_name, constants::COMPONENTS_COLL),
        );
        let format_strings = FormatStringsMongoDB::new(
            task_mgr.clone(),
            Arc::clone(&connection),
            format!("{}.{}", common_db_name, constants::FMT_STRS_COLL),
        );
        let categories = CategoriesMongoDB::new(
            task_mgr.clone(),
            Arc::clone(&connection),
            format!("{}.{}", common_db_name, constants::CATEGORIES_COLL),
        );

        Self {
            clock,
            write_to_stdout,
            user_logs: BTreeMap::new(),
            logger_id,
            common_db_name,
            connection,
            connection_info: ConnectionInfo::default(),
            thread_data: None,
            task_mgr,
            can_add_logs: false,
            hostnames,
            component_names,
            format_strings,
            categories,
            app_id_map: BTreeMap::new(),
            component_map: BTreeMap::new(),
            priority_threshold: message_priority::TRACE,
            current_second: 0,
            current_counter: 0,
            current_buf_size: 0,
            max_buf_size: constants::MAX_LOG_BUFFER_SIZE,
            flush_interval: constants::LOG_FLUSH_INTERVAL,
            expire_logs_days: constants::EXPIRE_LOGS_DAYS,
            uid_resolver,
        }
    }

    /// `init` (`log_storage.cpp:123-177`): read the config, connect, seed
    /// and load the common collections, then start the connection data.
    /// The reference parks a background thread that runs `onStart` and
    /// waits for the connection; the port runs `on_start` inline (the
    /// worker deviation) and fails the init when the socket never came up.
    pub fn init(&mut self, config: &BTreeMap<String, String>) -> bool {
        if !self.init_from_config(config) {
            return false;
        }

        if !connect_to_db(&*self.connection, &self.connection_info) {
            return false;
        }

        if !self.init_common_collections() {
            return false;
        }

        let expire_seconds = self.expire_logs_seconds();
        let mut thread_data = ConnectionThreadData::new(
            Arc::clone(&self.connection),
            self.connection_info.clone(),
            expire_seconds,
            self.logger_id.clone(),
            Arc::clone(&self.clock),
        );
        let started = thread_data.on_start() && thread_data.is_connected();
        self.thread_data = Some(thread_data);

        if !started {
            return false;
        }

        self.can_add_logs = true;
        true
    }

    /// `initFromConfig` (`log_storage.cpp:473-643`): the `mongodb` section.
    /// The port receives the section's key/value pairs directly — the
    /// `ConfigReader` hop is the caller's job. The four required keys fail
    /// the init when missing; the five optional ones fail on a malformed
    /// value, exactly like the reference's `sscanf` checks.
    fn init_from_config(&mut self, config: &BTreeMap<String, String>) -> bool {
        let get = |key: &str| config.get(key).cloned();

        let Some(host) = get("host") else {
            return false;
        };
        self.connection_info.db_host = host;

        let Some(port) = get("port").and_then(|p| p.parse::<u16>().ok()) else {
            return false;
        };
        self.connection_info.db_port = port;

        let Some(user) = get("user") else {
            return false;
        };
        self.connection_info.db_user = user;

        let Some(password) = get("password") else {
            return false;
        };
        self.connection_info.db_passwd = password;

        if let Some(value) = get("tcp_time_out") {
            let Some(parsed) = value.parse::<u32>().ok() else {
                return false;
            };
            self.connection_info.tcp_timeout = parsed;
        }

        if let Some(value) = get("wait_between_reconnects") {
            let Some(parsed) = value.parse::<u16>().ok() else {
                return false;
            };
            self.connection_info.wait_between_reconnects = parsed;
        }

        if let Some(value) = get("max_buffered_lines") {
            let Some(parsed) = value.parse::<u32>().ok() else {
                return false;
            };
            self.max_buf_size = parsed;
        }

        if let Some(value) = get("flush_interval") {
            let Some(parsed) = value.parse::<u32>().ok() else {
                return false;
            };
            self.flush_interval = parsed;
        }

        if let Some(value) = get("expire_logs_days") {
            let Some(parsed) = value.parse::<u32>().ok() else {
                return false;
            };
            self.expire_logs_days = parsed;
        }

        true
    }

    /// `initCommonCollections` (`log_storage.cpp:689-762`): load the four
    /// read collections, then seed the three write-if-absent ones.
    fn init_common_collections(&mut self) -> bool {
        if !self.hostnames.init()
            || !self.component_names.init()
            || !self.format_strings.init()
            || !self.categories.init()
        {
            return false;
        }

        let Some(coll_list) = get_collection_names(&*self.connection, &self.common_db_name) else {
            return false;
        };

        self.init_sources_coll(&coll_list)
            && self.init_severities_coll(&coll_list)
            && self.init_version_coll(&coll_list)
    }

    /// `initSourcesColl` (`log_storage.cpp:766-797`): the `CPP` and
    /// `SCRIPT` rows; a failed seed rolls the collection back.
    fn init_sources_coll(&self, coll_list: &CollList) -> bool {
        let ns = format!("{}.{}", self.common_db_name, constants::SOURCES_COLL);
        if !coll_list.contains(&ns) {
            if !self
                .connection
                .create_collection(&ns, 0, false, 0)
                .unwrap_or(false)
            {
                return false;
            }
            if !self.insert_source_to_db(MessageSource::Cpp)
                || !self.insert_source_to_db(MessageSource::Script)
            {
                self.connection.drop_collection(&ns);
                return false;
            }
        }
        true
    }

    /// `insertSourceToDB` (`log_storage.cpp:803-822`).
    fn insert_source_to_db(&self, source: MessageSource) -> bool {
        let ns = format!("{}.{}", self.common_db_name, constants::SOURCES_COLL);
        let mut obj = BsonObj::new();
        obj.append(
            constants::SOURCES_KEY_ID,
            Bson::Int(source.to_byte() as i32),
        );
        obj.append(
            constants::SOURCES_KEY_NAME,
            Bson::Str(source_name(source).to_string()),
        );
        if self.connection.insert(&ns, &obj).is_err() {
            return false;
        }
        self.connection.last_error().is_empty()
    }

    /// `initSeveritiesColl` (`log_storage.cpp:825-883`): the eight levels.
    fn init_severities_coll(&self, coll_list: &CollList) -> bool {
        let ns = format!("{}.{}", self.common_db_name, constants::SEVERITIES_COLL);
        if !coll_list.contains(&ns) {
            if !self
                .connection
                .create_collection(&ns, 0, false, 0)
                .unwrap_or(false)
            {
                return false;
            }
            let levels = [
                message_priority::TRACE,
                message_priority::DEBUG,
                message_priority::INFO,
                message_priority::NOTICE,
                message_priority::WARNING,
                message_priority::ERROR,
                message_priority::CRITICAL,
                message_priority::HACK,
            ];
            for level in levels {
                if !self.insert_severity_to_db(level) {
                    self.connection.drop_collection(&ns);
                    return false;
                }
            }
        }
        true
    }

    /// `insertSeverityToDB` (`log_storage.cpp:889-909`).
    fn insert_severity_to_db(&self, level: u8) -> bool {
        let ns = format!("{}.{}", self.common_db_name, constants::SEVERITIES_COLL);
        let mut obj = BsonObj::new();
        obj.append(constants::SEVERITIES_KEY_LEVEL, Bson::Int(level as i32));
        obj.append(
            constants::SEVERITIES_KEY_NAME,
            Bson::Str(severity_name(level).to_string()),
        );
        if self.connection.insert(&ns, &obj).is_err() {
            return false;
        }
        self.connection.last_error().is_empty()
    }

    /// `initVersionColl` (`log_storage.cpp:896-963`): seed the record, or
    /// verify the stored schema version matches the code's.
    fn init_version_coll(&self, coll_list: &CollList) -> bool {
        let ns = format!("{}.{}", self.common_db_name, constants::VERSION_COLL);
        if !coll_list.contains(&ns) {
            if !self
                .connection
                .create_collection(&ns, 0, false, 0)
                .unwrap_or(false)
            {
                return false;
            }
            return self.insert_version();
        }

        let rows = match self.connection.query(&ns, &Default::default()) {
            Ok(rows) => rows,
            Err(_) => return false,
        };
        if let Some(record) = rows.first() {
            let version = record.get_int(constants::VERSION_KEY_VERSION) as u32;
            if version != constants::SCHEMA_VERSION {
                return false;
            }
        } else {
            return self.insert_version();
        }
        true
    }

    /// `insertVersion` (`log_storage.cpp:969-986`).
    fn insert_version(&self) -> bool {
        let ns = format!("{}.{}", self.common_db_name, constants::VERSION_COLL);
        let mut obj = BsonObj::new();
        obj.append(
            constants::VERSION_KEY_VERSION,
            Bson::Int(constants::SCHEMA_VERSION as i32),
        );
        if self.connection.insert(&ns, &obj).is_err() {
            return false;
        }
        self.connection.last_error().is_empty()
    }

    /// `getExpireLogsSeconds` (`log_storage.cpp:717-724`): the whole-day
    /// total minus one hour, so a missed midnight roll doesn't retain an
    /// extra day.
    pub fn expire_logs_seconds(&self) -> u64 {
        (self.expire_logs_days as u64 * 24 - 1) * 60 * 60
    }

    /// `LogStorageMongoDB::addLogMessage` (`log_storage.cpp:273-312`) plus
    /// the base `LogStorage::addLogMessage` (`log_storage.cpp` of the
    /// parent): the connection gate, the header, the priority filter, the
    /// format string, the host, the category — then the write.
    pub fn add_log_message(
        &mut self,
        component_message: &ComponentMessage,
        address: &Address,
        input: &mut MemoryIStream,
    ) -> AddLogMessageResult {
        // Only trigger a real addLogMessage if we are connected to the DB.
        let connected = self.is_connected();
        self.can_add_logs = connected;

        if !self.can_add_logs {
            // If there are no tasks queued in the background then nothing
            // will ever attempt to reconnect. Add a manual reconnect task.
            if self.task_mgr.bg_task_list_size() == 0 {
                self.task_mgr
                    .add_background_task_at(Task::Reconnect, priority::MAX);
            }
            return AddLogMessageResult::Ignored;
        }

        // Stream off the header. Old versions carry the two-element header
        // with the deprecated script priority folded in.
        let header = if component_message.version <= OLDEST_VERSION_WITHOUT_SOURCE_ENUM {
            MessageHeader::read_without_source_enum(input)
        } else {
            MessageHeader::read(input)
        };
        let Some(header) = header else {
            return AddLogMessageResult::Failed;
        };

        if !self.should_log_priority(header.message_priority) {
            return AddLogMessageResult::Ignored;
        }

        let format = input.read_string();
        if input.error() {
            return AddLogMessageResult::Failed;
        }

        let Some(handler) = self.format_strings.resolve(&format) else {
            return AddLogMessageResult::Failed;
        };

        // Cache the IP address -> hostname mapping if necessary. The
        // MongoDB backend has no validator, so the BW_GET_HOST_ADDED
        // propagation branch of the base class never runs.
        let (_hostname, result) = self.hostnames.get_host_by_addr(address.ip);
        if result == GetHostResult::Error {
            return AddLogMessageResult::Failed;
        }

        let category_id = self
            .categories
            .resolve_or_create_name_to_id(&header.category);

        self.write_log_to_db(
            component_message,
            address,
            input,
            &header,
            &handler,
            category_id,
        )
    }

    /// `logger_.shouldLogPriority`.
    fn should_log_priority(&self, priority: u8) -> bool {
        priority >= self.priority_threshold
    }

    /// The criticality filter seam — see the field docs.
    pub fn set_priority_threshold(&mut self, threshold: u8) {
        self.priority_threshold = threshold;
    }

    /// `LogStorageMongoDB::writeLogToDB` (`log_storage.cpp:318-465`).
    #[allow(clippy::too_many_arguments)]
    fn write_log_to_db(
        &mut self,
        component_message: &ComponentMessage,
        address: &Address,
        input: &mut MemoryIStream,
        header: &MessageHeader,
        handler: &LogStringInterpolator,
        category_id: u16,
    ) -> AddLogMessageResult {
        // 1. Prepare user log buffer. Create a new one if it's a new user.
        let uid = component_message.uid;
        if !self.user_logs.contains_key(&uid) {
            let Some(username) = self.uid_resolver.resolve_uid(uid, address.ip) else {
                return AddLogMessageResult::Failed;
            };
            self.user_logs.insert(uid, UserLogBuffer::new(username));
        }
        let username = self.user_logs[&uid].username().to_string();

        // 2. Parse the log message into a BSON object.
        let curr_time = self.clock.now_millis();
        let log_bson = self.create_bson(
            component_message,
            address,
            input,
            header,
            handler,
            category_id,
            curr_time,
        );

        // A Tools component announcing itself also lands a server-start
        // record.
        if component_message.component_name == constants::BW_TOOLS_COMPONENT_NAME
            && log_bson.get_string(constants::ENTRIES_KEY_MESSAGE)
                == constants::BW_TOOLS_SERVER_START_MSG
        {
            self.task_mgr.add_background_task(Task::AddServerStart {
                uid,
                username: username.clone(),
                server_start_time: curr_time,
            });
        }

        if self.write_to_stdout {
            print!("{}", log_bson.json_string());
        }

        // The component's first log line also writes its components record.
        if !self.component_map.contains_key(address) {
            let Some(username) = self.uid_resolver.resolve_uid(uid, address.ip) else {
                return AddLogMessageResult::Failed;
            };
            let component_id = self
                .component_names
                .get_id_of_component_name(&component_message.component_name);

            let mut component_bson = BsonObj::new();
            component_bson.append(constants::COMPONENTS_KEY_HOST, Bson::Int(address.ip as i32));
            component_bson.append(
                constants::COMPONENTS_KEY_PID,
                Bson::Int(component_message.pid as i32),
            );
            component_bson.append(
                constants::COMPONENTS_KEY_COMPONENT,
                Bson::Int(component_id as i32),
            );
            component_bson.append(constants::COMPONENTS_KEY_APPID, Bson::Int(0));

            let mut component_map_bson = BsonObj::new();
            component_map_bson.append(constants::COMPONENTS_KEY_HOST, Bson::Int(address.ip as i32));
            component_map_bson.append(
                constants::COMPONENTS_KEY_PID,
                Bson::Int(component_message.pid as i32),
            );
            component_map_bson.append(
                constants::COMPONENTS_KEY_COMPONENT,
                Bson::Int(component_id as i32),
            );
            component_map_bson.append(constants::COMPONENTS_UID, Bson::Int(uid as i32));
            component_map_bson.append(constants::COMPONENTS_USERNAME, Bson::Str(username.clone()));

            self.task_mgr.add_background_task(Task::WriteAppID {
                uid,
                username: username.clone(),
                obj: component_bson,
            });

            self.component_map.insert(*address, component_map_bson);
        }

        // 3. Add the log to user log buffer and flush the buffer if
        // necessary.
        self.user_logs
            .get_mut(&uid)
            .expect("uid just inserted or found")
            .append(log_bson);
        self.current_buf_size += 1;

        if self.current_buf_size >= self.max_buf_size {
            return self.flush_logs();
        }

        AddLogMessageResult::Success
    }

    /// `createBson` (`log_storage.cpp:994-1058`): the entry document, its
    /// fields in the reference's order.
    #[allow(clippy::too_many_arguments)]
    fn create_bson(
        &mut self,
        component_message: &ComponentMessage,
        address: &Address,
        input: &mut MemoryIStream,
        header: &MessageHeader,
        handler: &LogStringInterpolator,
        category_id: u16,
        timestamp: u64,
    ) -> BsonObj {
        let mut obj = BsonObj::new();
        obj.append(
            constants::ENTRIES_KEY_TIMESTAMP,
            Bson::Date(timestamp as i64),
        );
        let counter = self.get_counter(timestamp);
        obj.append(constants::ENTRIES_KEY_COUNTER, Bson::Int(counter as i32));
        obj.append(
            constants::ENTRIES_KEY_CATEGORY,
            Bson::Int(category_id as i32),
        );
        obj.append(
            constants::ENTRIES_KEY_SOURCE,
            Bson::Int(header.message_source.to_byte() as i32),
        );
        obj.append(
            constants::ENTRIES_KEY_SEVERITY,
            Bson::Int(header.message_priority as i32),
        );
        obj.append(constants::ENTRIES_KEY_HOST, Bson::Int(address.ip as i32));
        obj.append(
            constants::ENTRIES_KEY_PID,
            Bson::Int(component_message.pid as i32),
        );

        let fmt_string = handler.format_string().to_string();
        let fmt_id = self.format_strings.get_id_of_fmt_string(&fmt_string);
        obj.append(constants::ENTRIES_KEY_FMT_STR, Bson::Int(fmt_id as i32));

        let component_id = self
            .component_names
            .get_id_of_component_name(&component_message.component_name);
        obj.append(
            constants::ENTRIES_KEY_COMPONENT,
            Bson::Int(component_id as i32),
        );

        obj.append(
            constants::ENTRIES_KEY_APP_ID,
            Bson::Int(self.get_app_id(address) as i32),
        );

        let (msg, metadata) = self.parse_message(handler, input, component_message.version);

        // New version apps validate UTF8 on the sending side, old ones do
        // not. The port's strings are always valid UTF-8, so the
        // sanitisation is a no-op — the registered deviation of this file.
        obj.append(constants::ENTRIES_KEY_MESSAGE, Bson::Str(msg));
        obj.append(constants::ENTRIES_KEY_META_DATA, Bson::Doc(metadata));

        let is_cluster = self
            .thread_data
            .as_ref()
            .map(|data| data.get_is_cluster())
            .unwrap_or(false);
        if is_cluster {
            obj.append(
                constants::ENTRIES_KEY_SHARD_KEY,
                Bson::Int(self.get_shard_key(component_message.pid)),
            );
        }
        obj
    }

    /// `getShardKey` (`log_storage.cpp:1085-1092`): spread consecutive
    /// pids, fold to ten buckets.
    fn get_shard_key(&self, pid: u32) -> i32 {
        (pid.wrapping_mul(7) % 10) as i32
    }

    /// `getCounter` (`log_storage.cpp:1102-1115`): restarts at 1 every
    /// second.
    fn get_counter(&mut self, milliseconds: u64) -> u32 {
        let second = milliseconds / 1000;
        if second == self.current_second {
            self.current_counter += 1;
        } else {
            self.current_second = second;
            self.current_counter = 1;
        }
        self.current_counter
    }

    /// `getAppID`: zero when the address never announced itself.
    pub fn get_app_id(&self, address: &Address) -> u32 {
        self.app_id_map.get(address).copied().unwrap_or(0)
    }

    /// `parseMessage` (`log_storage.cpp:1124-1143`): render the arguments,
    /// then the typed metadata when the stream carries it.
    fn parse_message(
        &self,
        handler: &LogStringInterpolator,
        input: &mut MemoryIStream,
        version: u8,
    ) -> (String, BsonObj) {
        let mut msg = String::new();
        let _ = interpolator::stream_to_string(handler, input, version, &mut msg);

        // inputStream may have 1 byte remaining for NULL.
        if version >= LOG_METADATA_VERSION && input.remaining() > 1 {
            let metadata = build_metadata_bson(input);
            input.finish();
            return (msg, metadata);
        }
        (msg, BsonObj::default())
    }

    /// `flushLogs` (`log_storage.cpp:1174-1204`): one `FlushTask` per
    /// non-empty user buffer; the task consumes the buffer wholesale.
    pub fn flush_logs(&mut self) -> AddLogMessageResult {
        let mut tasks = Vec::new();
        for (uid, buffer) in self.user_logs.iter_mut() {
            if buffer.size() == 0 {
                continue;
            }
            tasks.push(Task::Flush {
                uid: *uid,
                username: buffer.username().to_string(),
                log_buffer: buffer.take(),
            });
        }
        for task in tasks {
            self.task_mgr.add_background_task(task);
        }

        self.current_buf_size = 0;
        AddLogMessageResult::Success
    }

    /// `startFlushTimer`/`handleTimeout` (`log_storage.cpp:1148-1170`): the
    /// dispatcher timer is the caller's; the interval is exposed and the
    /// tick is this method.
    pub fn handle_timeout(&mut self) -> AddLogMessageResult {
        self.flush_logs()
    }

    /// The flush interval, milliseconds (`startFlushTimer`).
    pub fn flush_interval(&self) -> u32 {
        self.flush_interval
    }

    /// `tick` (`log_storage.cpp:196-210`): drain one task. The reference
    /// calls `exit(EXIT_FAILURE)` when the worker signalled an
    /// unrecoverable error; the port reports it instead — the caller
    /// decides (`true` means stop). Registered deviation.
    pub fn tick(&mut self) -> bool {
        let Some(data) = self.thread_data.as_mut() else {
            return true;
        };
        self.task_mgr.tick(data);

        data.should_abort_further_processing()
    }

    /// `roll` (`log_storage.cpp:214-221`): queue the roll at `HIGH`.
    pub fn roll(&mut self) -> bool {
        self.task_mgr
            .add_background_task_at(Task::Roll, priority::HIGH);
        true
    }

    /// `setAppInstanceID` (`log_storage.cpp:226-244`): remember the id, and
    /// queue an update when the component already wrote its record.
    pub fn set_app_instance_id(&mut self, address: &Address, id: u32) -> bool {
        self.app_id_map.insert(*address, id);

        if let Some(obj) = self.component_map.get(address) {
            let uid = obj.get_int(constants::COMPONENTS_UID) as u16;
            let username = obj.get_string(constants::COMPONENTS_USERNAME);
            self.task_mgr.add_background_task(Task::UpdateAppID {
                uid,
                username,
                obj: obj.clone(),
                appid: id as i32,
            });
        }
        true
    }

    /// `stopLoggingFromComponent` (`log_storage.cpp:248-263`).
    pub fn stop_logging_from_component(&mut self, address: &Address) -> bool {
        self.app_id_map.remove(address);
        self.component_map.remove(address);
        true
    }

    /// `validateNextHostname` (`log_storage.cpp:267-269`).
    pub fn validate_next_hostname(&mut self) -> ValidatorStatus {
        self.hostnames.validate_next_hostname()
    }

    /// `isConnected` (`log_storage.cpp:185-189`).
    pub fn is_connected(&self) -> bool {
        self.thread_data
            .as_ref()
            .map(|data| data.is_connected())
            .unwrap_or(false)
    }

    /// `currentBufSize_`, for the fixture's assertions.
    pub fn current_buf_size(&self) -> u32 {
        self.current_buf_size
    }

    /// The queued task count, for the fixture's assertions.
    pub fn bg_task_list_size(&self) -> usize {
        self.task_mgr.bg_task_list_size()
    }

    /// The per-user buffers, for the fixture's assertions.
    pub fn user_logs(&self) -> &BTreeMap<u16, UserLogBuffer> {
        &self.user_logs
    }
}

/// `messageSourceAsCString` (`debug_message_source.cpp`).
fn source_name(source: MessageSource) -> &'static str {
    match source {
        MessageSource::Cpp => "CPP",
        MessageSource::Script => "SCRIPT",
    }
}

/// `messagePrefix` (`debug_message_priority.cpp`).
fn severity_name(level: u8) -> &'static str {
    match level {
        message_priority::TRACE => "TRACE",
        message_priority::DEBUG => "DEBUG",
        message_priority::INFO => "INFO",
        message_priority::NOTICE => "NOTICE",
        message_priority::WARNING => "WARNING",
        message_priority::ERROR => "ERROR",
        message_priority::CRITICAL => "CRITICAL",
        _ => "HACK",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MESSAGE_LOGGER_VERSION;
    use crate::mongodb::driver::InMemoryStore;
    use crate::mongodb::types::{DefaultUidResolver, NoHostResolver};
    use crate::stream::MemoryOStream;

    const LOGGER_ID: &str = "test";
    const COMMON: &str = "bw_ml_common#test";
    const USER_DB: &str = "bw_ml_user_1000#test";
    const FIXED_MILLIS: u64 = 1_700_000_000_000;

    /// A clock pinned to one instant, so the entry timestamps and the
    /// per-second counter are reproducible.
    struct FixedClock;

    impl Clock for FixedClock {
        fn now_secs(&self) -> i64 {
            (FIXED_MILLIS / 1000) as i64
        }

        fn now_millis(&self) -> u64 {
            FIXED_MILLIS
        }
    }

    /// The `mongodb` section of the config file.
    fn config() -> BTreeMap<String, String> {
        let mut config = BTreeMap::new();
        config.insert("host".to_string(), "127.0.0.1".to_string());
        config.insert("port".to_string(), "27017".to_string());
        config.insert("user".to_string(), "bw".to_string());
        config.insert("password".to_string(), "secret".to_string());
        config
    }

    fn storage(store: &Arc<InMemoryStore>) -> LogStorageMongoDB<InMemoryStore> {
        LogStorageMongoDB::new(
            LOGGER_ID,
            Arc::clone(store),
            Arc::new(DefaultUidResolver),
            Arc::new(NoHostResolver),
            Arc::new(FixedClock),
            false,
        )
    }

    /// Real MongoDB creates a collection on first insert; the in-memory
    /// store wants the four the task queue fills explicit.
    fn precreate_common(store: &InMemoryStore) {
        for coll in [
            constants::HOST_NAMES_COLL,
            constants::COMPONENTS_COLL,
            constants::FMT_STRS_COLL,
            constants::CATEGORIES_COLL,
        ] {
            store
                .create_collection(&format!("{COMMON}.{coll}"), 0, false, 0)
                .unwrap();
        }
    }

    /// An initialised storage plus the four common collections the task
    /// queue fills.
    fn inited(store: &Arc<InMemoryStore>) -> LogStorageMongoDB<InMemoryStore> {
        let mut storage = storage(store);
        assert!(storage.init(&config()));
        precreate_common(store);
        storage
    }

    /// Run queued tasks until the queue is empty; a `true` from
    /// [`LogStorageMongoDB::tick`] is the unrecoverable-error signal.
    fn drain(storage: &mut LogStorageMongoDB<InMemoryStore>) {
        while storage.bg_task_list_size() > 0 {
            assert!(!storage.tick(), "task signalled an unrecoverable error");
        }
    }

    /// The wire an app sends for one record: header, format string, the
    /// packed arguments, optionally the typed metadata.
    fn wire_message(
        header: &MessageHeader,
        format: &str,
        build: impl FnOnce(&mut MemoryOStream),
    ) -> Vec<u8> {
        let mut stream = MemoryOStream::new();
        header.write(&mut stream);
        stream.write_string(format);
        build(&mut stream);
        stream.data().to_vec()
    }

    fn info_header(category: &str) -> MessageHeader {
        MessageHeader {
            component_priority: 0,
            message_priority: message_priority::INFO,
            message_source: MessageSource::Cpp,
            category: category.to_string(),
        }
    }

    /// uid 1000 announcing itself from loopback.
    fn app_message() -> (ComponentMessage, Address) {
        (
            ComponentMessage {
                version: MESSAGE_LOGGER_VERSION,
                logger_id: LOGGER_ID.to_string(),
                uid: 1000,
                pid: 4321,
                component_name: "cellapp".to_string(),
            },
            Address::new(0x7f00_0001, 30001),
        )
    }

    fn log_line(
        storage: &mut LogStorageMongoDB<InMemoryStore>,
        format: &str,
    ) -> AddLogMessageResult {
        let (message, address) = app_message();
        let bytes = wire_message(&info_header("CellApp"), format, |stream| {
            stream.write_u32(42);
        });
        let mut input = MemoryIStream::new(&bytes);
        storage.add_log_message(&message, &address, &mut input)
    }

    /// The one `entries_<suffix>` collection a single flush creates.
    fn entries_namespace(store: &InMemoryStore) -> String {
        let prefix = format!("{USER_DB}.{}", constants::USER_COLL_ENTRIES_PREFIX);
        store
            .known_collections()
            .into_iter()
            .find(|ns| ns.starts_with(&prefix))
            .expect("an entries collection after the flush")
    }

    #[test]
    fn init_seeds_the_common_collections() {
        let store = Arc::new(InMemoryStore::new());
        let mut storage = storage(&store);
        assert!(storage.init(&config()));

        let sources = store.documents(&format!("{COMMON}.{}", constants::SOURCES_COLL));
        assert_eq!(sources.len(), 2);
        assert_eq!(sources[0].get_int(constants::SOURCES_KEY_ID), 0);
        assert_eq!(sources[0].get_string(constants::SOURCES_KEY_NAME), "CPP");
        assert_eq!(sources[1].get_int(constants::SOURCES_KEY_ID), 1);
        assert_eq!(sources[1].get_string(constants::SOURCES_KEY_NAME), "SCRIPT");

        let severities = store.documents(&format!("{COMMON}.{}", constants::SEVERITIES_COLL));
        assert_eq!(severities.len(), 8);
        assert_eq!(severities[0].get_int(constants::SEVERITIES_KEY_LEVEL), 0);
        assert_eq!(
            severities[0].get_string(constants::SEVERITIES_KEY_NAME),
            "TRACE"
        );
        assert_eq!(
            severities[7].get_string(constants::SEVERITIES_KEY_NAME),
            "HACK"
        );

        let version = store.documents(&format!("{COMMON}.{}", constants::VERSION_COLL));
        assert_eq!(version.len(), 1);
        assert_eq!(
            version[0].get_int(constants::VERSION_KEY_VERSION),
            constants::SCHEMA_VERSION as i32
        );
    }

    #[test]
    fn a_reinit_does_not_reseed() {
        let store = Arc::new(InMemoryStore::new());
        {
            let mut storage = storage(&store);
            assert!(storage.init(&config()));
        }

        let mut second = storage(&store);
        assert!(second.init(&config()));

        assert_eq!(
            store
                .documents(&format!("{COMMON}.{}", constants::SOURCES_COLL))
                .len(),
            2
        );
        assert_eq!(
            store
                .documents(&format!("{COMMON}.{}", constants::SEVERITIES_COLL))
                .len(),
            8
        );
        assert_eq!(
            store
                .documents(&format!("{COMMON}.{}", constants::VERSION_COLL))
                .len(),
            1
        );
    }

    #[test]
    fn a_wrong_schema_version_fails_the_init() {
        let store = Arc::new(InMemoryStore::new());
        {
            let mut storage = storage(&store);
            assert!(storage.init(&config()));
        }

        // A database written by a different code generation.
        let version_ns = format!("{COMMON}.{}", constants::VERSION_COLL);
        let stale = {
            let mut obj = BsonObj::new();
            obj.append(constants::VERSION_KEY_VERSION, Bson::Int(1));
            obj
        };
        store.drop_collection(&version_ns);
        store.create_collection(&version_ns, 0, false, 0).unwrap();
        store.insert(&version_ns, &stale).unwrap();

        let mut second = storage(&store);
        assert!(!second.init(&config()));
    }

    #[test]
    fn the_config_requires_host_port_user_password() {
        let store = Arc::new(InMemoryStore::new());
        let mut storage = storage(&store);

        let mut missing_host = config();
        missing_host.remove("host").unwrap();
        assert!(!storage.init(&missing_host));

        let mut bad_port = config();
        bad_port.insert("port".to_string(), "not-a-port".to_string());
        assert!(!storage.init(&bad_port));

        let mut missing_password = config();
        missing_password.remove("password").unwrap();
        assert!(!storage.init(&missing_password));

        let mut bad_optional = config();
        bad_optional.insert("flush_interval".to_string(), "later".to_string());
        assert!(!storage.init(&bad_optional));
    }

    #[test]
    fn the_config_takes_the_optional_tunables() {
        let store = Arc::new(InMemoryStore::new());
        let mut config = config();
        config.insert("flush_interval".to_string(), "250".to_string());
        config.insert("expire_logs_days".to_string(), "3".to_string());
        config.insert("max_buffered_lines".to_string(), "1".to_string());

        let mut storage = storage(&store);
        assert!(storage.init(&config));
        precreate_common(&store);
        assert_eq!(storage.flush_interval(), 250);
        assert_eq!(storage.expire_logs_seconds(), (3 * 24 - 1) * 3600);

        // maxBufferedLines 1: the first record flushes its own buffer.
        assert_eq!(log_line(&mut storage, "x=%d"), AddLogMessageResult::Success);
        assert_eq!(storage.current_buf_size(), 0);
        drain(&mut storage);

        let docs = store.documents(&entries_namespace(&store));
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].get_string(constants::ENTRIES_KEY_MESSAGE), "x=42");
    }

    #[test]
    fn a_flush_lands_reference_shaped_entries() {
        let store = Arc::new(InMemoryStore::new());
        let mut storage = inited(&store);

        assert_eq!(log_line(&mut storage, "x=%d"), AddLogMessageResult::Success);
        assert_eq!(storage.current_buf_size(), 1);
        assert_eq!(storage.user_logs()[&1000].size(), 1);

        // A second line inside the same second: the per-second counter
        // advances, the reference tables (format, host, category, component)
        // are all hits so nothing new is queued.
        assert_eq!(log_line(&mut storage, "x=%d"), AddLogMessageResult::Success);
        assert_eq!(storage.bg_task_list_size(), 5);

        assert_eq!(storage.handle_timeout(), AddLogMessageResult::Success);
        drain(&mut storage);

        assert_eq!(storage.current_buf_size(), 0);
        assert_eq!(storage.user_logs()[&1000].size(), 0);

        let entries = store.documents(&entries_namespace(&store));
        assert_eq!(entries.len(), 2);
        for (index, entry) in entries.iter().enumerate() {
            assert_eq!(
                entry.get(constants::ENTRIES_KEY_TIMESTAMP),
                Some(&Bson::Date(FIXED_MILLIS as i64))
            );
            assert_eq!(
                entry.get_int(constants::ENTRIES_KEY_COUNTER),
                index as i32 + 1
            );
            assert_eq!(entry.get_int(constants::ENTRIES_KEY_CATEGORY), 1);
            assert_eq!(
                entry.get_int(constants::ENTRIES_KEY_SOURCE),
                MessageSource::Cpp.to_byte() as i32
            );
            assert_eq!(
                entry.get_int(constants::ENTRIES_KEY_SEVERITY),
                message_priority::INFO as i32
            );
            assert_eq!(entry.get_int(constants::ENTRIES_KEY_HOST), 0x7f00_0001i32);
            assert_eq!(entry.get_int(constants::ENTRIES_KEY_PID), 4321);
            assert_eq!(entry.get_int(constants::ENTRIES_KEY_FMT_STR), 1);
            assert_eq!(entry.get_int(constants::ENTRIES_KEY_APP_ID), 0);
            assert_eq!(entry.get_string(constants::ENTRIES_KEY_MESSAGE), "x=42");
            assert!(entry.has_field(constants::ENTRIES_KEY_META_DATA));
        }

        // The reference tables the queue filled along the way.
        let fmts = store.documents(&format!("{COMMON}.{}", constants::FMT_STRS_COLL));
        assert_eq!(fmts.len(), 1);
        assert_eq!(fmts[0].get_int(constants::FMT_STRS_KEY_ID), 1);
        assert_eq!(fmts[0].get_string(constants::FMT_STRS_KEY_FMT_STR), "x=%d");

        let categories = store.documents(&format!("{COMMON}.{}", constants::CATEGORIES_COLL));
        assert_eq!(categories.len(), 1);
        assert_eq!(categories[0].get_int(constants::CATEGORIES_KEY_ID), 1);
        assert_eq!(
            categories[0].get_string(constants::CATEGORIES_KEY_NAME),
            "CellApp"
        );

        let components = store.documents(&format!("{COMMON}.{}", constants::COMPONENTS_COLL));
        assert_eq!(components.len(), 1);
        assert_eq!(
            components[0].get_string(constants::COMPONENTS_KEY_NAME),
            "cellapp"
        );
        assert_eq!(
            entries[0].get_int(constants::ENTRIES_KEY_COMPONENT),
            components[0].get_int(constants::COMPONENTS_KEY_ID)
        );

        let hosts = store.documents(&format!("{COMMON}.{}", constants::HOST_NAMES_COLL));
        assert_eq!(hosts.len(), 1);
        assert_eq!(
            hosts[0].get_int(constants::HOST_NAMES_KEY_IP),
            0x7f00_0001i32
        );
        assert_eq!(
            hosts[0].get_string(constants::HOST_NAMES_KEY_HOSTNAME),
            "127.0.0.1"
        );

        // The user database: the uid record and the component record.
        let uid = store.documents(&format!("{USER_DB}.{}", constants::USER_COLL_UID));
        assert_eq!(uid.len(), 1);
        assert_eq!(uid[0].get_int(constants::USER_UID_KEY), 1000);

        let written = store.documents(&format!("{USER_DB}.{}", constants::COMPONENTS_COLL_NAME));
        assert_eq!(written.len(), 1);
        assert_eq!(written[0].get_int(constants::COMPONENTS_KEY_PID), 4321);
        assert_eq!(written[0].get_int(constants::COMPONENTS_KEY_APPID), 0);
    }

    #[test]
    fn a_tools_start_message_also_lands_a_server_start_record() {
        let store = Arc::new(InMemoryStore::new());
        let mut storage = inited(&store);

        let message = ComponentMessage {
            version: MESSAGE_LOGGER_VERSION,
            logger_id: LOGGER_ID.to_string(),
            uid: 1000,
            pid: 4321,
            component_name: constants::BW_TOOLS_COMPONENT_NAME.to_string(),
        };
        let address = Address::new(0x7f00_0001, 30001);
        let bytes = wire_message(
            &info_header("Tools"),
            constants::BW_TOOLS_SERVER_START_MSG,
            |_| {},
        );
        let mut input = MemoryIStream::new(&bytes);
        assert_eq!(
            storage.add_log_message(&message, &address, &mut input),
            AddLogMessageResult::Success
        );
        assert_eq!(storage.handle_timeout(), AddLogMessageResult::Success);
        drain(&mut storage);

        let starts = store.documents(&format!("{USER_DB}.{}", constants::SERVER_STARTS_COLL_NAME));
        assert_eq!(starts.len(), 1);
        assert_eq!(
            starts[0].get(constants::SERVER_STARTS_KEY_TIME),
            Some(&Bson::Date(FIXED_MILLIS as i64))
        );
    }

    #[test]
    fn set_app_instance_id_updates_the_component_record() {
        let store = Arc::new(InMemoryStore::new());
        let mut storage = inited(&store);

        assert_eq!(log_line(&mut storage, "x=%d"), AddLogMessageResult::Success);
        assert_eq!(storage.handle_timeout(), AddLogMessageResult::Success);
        drain(&mut storage);

        let address = Address::new(0x7f00_0001, 30001);
        assert_eq!(storage.get_app_id(&address), 0);

        assert!(storage.set_app_instance_id(&address, 42));
        assert_eq!(storage.get_app_id(&address), 42);
        drain(&mut storage);

        let written = store.documents(&format!("{USER_DB}.{}", constants::COMPONENTS_COLL_NAME));
        assert_eq!(written.len(), 1);
        assert_eq!(written[0].get_int(constants::COMPONENTS_KEY_APPID), 42);

        // Records from the announced instance carry the id from now on.
        assert_eq!(log_line(&mut storage, "x=%d"), AddLogMessageResult::Success);
        assert_eq!(storage.handle_timeout(), AddLogMessageResult::Success);
        drain(&mut storage);
        let entries = store.documents(&entries_namespace(&store));
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1].get_int(constants::ENTRIES_KEY_APP_ID), 42);
    }

    #[test]
    fn typed_metadata_is_written_into_the_md_field() {
        let store = Arc::new(InMemoryStore::new());
        let mut storage = inited(&store);

        let (message, address) = app_message();
        let bytes = wire_message(&info_header("CellApp"), "uid=%d", |stream| {
            stream.write_u32(1000);
            // The mlvt walk: a STRING key tag, then the JSON payload.
            stream.write_u32(b's' as u32);
            stream.write_string("json");
            stream.write_u32(b'j' as u32);
            stream.write_string("{\"pid2\":1}");
        });
        let mut input = MemoryIStream::new(&bytes);
        assert_eq!(
            storage.add_log_message(&message, &address, &mut input),
            AddLogMessageResult::Success
        );
        assert_eq!(storage.handle_timeout(), AddLogMessageResult::Success);
        drain(&mut storage);

        let entries = store.documents(&entries_namespace(&store));
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].get_string(constants::ENTRIES_KEY_MESSAGE),
            "uid=1000"
        );
        let metadata = entries[0].get(constants::ENTRIES_KEY_META_DATA);
        assert_eq!(
            metadata.and_then(|value| match value {
                Bson::Doc(doc) => Some(doc.get_int("pid2")),
                _ => None,
            }),
            Some(1)
        );
    }

    #[test]
    fn the_priority_threshold_ignores_quiet_lines() {
        let store = Arc::new(InMemoryStore::new());
        let mut storage = inited(&store);
        storage.set_priority_threshold(message_priority::ERROR);

        assert_eq!(log_line(&mut storage, "x=%d"), AddLogMessageResult::Ignored);
        assert!(storage.user_logs().get(&1000).is_none());
        assert_eq!(storage.bg_task_list_size(), 0);
    }

    #[test]
    fn an_init_without_a_server_queues_a_manual_reconnect() {
        let store = Arc::new(InMemoryStore::new());
        store.stop_server();

        let mut storage = storage(&store);
        assert!(!storage.init(&config()));

        // Nothing is connected, so the record is ignored — and the manual
        // reconnect task is what will ever attempt the reconnect.
        assert_eq!(log_line(&mut storage, "x=%d"), AddLogMessageResult::Ignored);
        assert_eq!(storage.bg_task_list_size(), 1);

        // The server returns and a retry of the init connects: the records
        // flow from there, the queued reconnect task included.
        store.start_server();
        assert!(storage.init(&config()));
        precreate_common(&store);
        assert!(storage.is_connected());

        assert_eq!(log_line(&mut storage, "x=%d"), AddLogMessageResult::Success);
        assert_eq!(storage.handle_timeout(), AddLogMessageResult::Success);
        drain(&mut storage);
        let entries = store.documents(&entries_namespace(&store));
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].get_string(constants::ENTRIES_KEY_MESSAGE),
            "x=42"
        );
    }

    #[test]
    fn a_dropped_socket_requeues_and_recovers() {
        let store = Arc::new(InMemoryStore::new());
        let mut storage = inited(&store);

        assert_eq!(log_line(&mut storage, "x=%d"), AddLogMessageResult::Success);
        assert_eq!(storage.handle_timeout(), AddLogMessageResult::Success);

        // The socket dies mid-queue. The write requeues at HIGH, the next
        // tick's prologue reconnects, and the batch still lands.
        store.drop_connection();
        drain(&mut storage);
        assert!(storage.is_connected());

        let entries = store.documents(&entries_namespace(&store));
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].get_string(constants::ENTRIES_KEY_MESSAGE),
            "x=42"
        );
    }

    #[test]
    fn an_unrecoverable_task_reports_the_abort_from_tick() {
        let store = Arc::new(InMemoryStore::new());
        let mut storage = inited(&store);

        // The reference's server-side refusal: the collection is gone by
        // the time the category record's task runs.
        store.drop_collection(&format!("{COMMON}.{}", constants::CATEGORIES_COLL));

        assert_eq!(log_line(&mut storage, "x=%d"), AddLogMessageResult::Success);
        assert_eq!(storage.handle_timeout(), AddLogMessageResult::Success);

        // Queue order: format pair, host pair, category pair (aborts), then
        // everything after it is skipped until the queue runs dry.
        assert!(!storage.tick());
        assert!(!storage.tick());
        assert!(storage.tick());
        assert!(storage.tick());
        assert!(storage.tick());

        // The connection survived — the refusal is not a socket loss.
        assert!(storage.is_connected());

        // The flush never landed.
        assert!(
            !store
                .known_collections()
                .iter()
                .any(|ns| ns.contains(constants::USER_COLL_ENTRIES_PREFIX))
        );
    }
}
