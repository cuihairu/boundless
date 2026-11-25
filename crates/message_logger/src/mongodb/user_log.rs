//! Per-user log storage (`mongodb/user_log_buffer.hpp` +
//! `mongodb/user_log_writer.{hpp,cpp}`).
//!
//! One user gets one database (`bw_ml_user_<name>#<loggerID>`): a `uid`
//! collection, a capped `server_start_ups` collection, a `components`
//! collection, and rolling `entries_<YYYYMMDDHHMMSS>` collections that the
//! expiry walk drops once their last entry ages past the retention window.
//!
//! Methods return `Result<bool, MongoError>`: `Ok(false)` is a plain
//! failure the reference logs, `Err(MongoError::Connection(_))` is the
//! `mongo::SocketException` path — the caller (the connection thread data)
//! clears its connected flag on that one, exactly where the reference's
//! `setConnected( false )` calls sit.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::file_logger::{civil_from_days, days_from_civil};
use crate::mongodb::bson::{Bson, BsonObj};
use crate::mongodb::connection::get_collection_names;
use crate::mongodb::constants;
use crate::mongodb::driver::{CommandReply, MongoConnection, MongoError, Query};
use crate::mongodb::types::{BsonObjBuffer, Clock, CollList};

/// `UserLogBuffer` (`user_log_buffer.hpp:22-51`): the username a buffer
/// flushes under, plus the documents not yet handed to a flush task.
pub struct UserLogBuffer {
    username: String,
    /// `logBuffer_` — handed to the flush task wholesale, which leaves the
    /// per-user buffer empty for the next batch.
    pub log_buffer: BsonObjBuffer,
}

impl UserLogBuffer {
    /// The reference constructor: the buffer reserves
    /// `RESERVE_LOG_BUFFER_SIZE` slots up front.
    pub fn new(username: String) -> Self {
        Self {
            username,
            log_buffer: Vec::with_capacity(constants::RESERVE_LOG_BUFFER_SIZE),
        }
    }

    /// `getUsername`.
    pub fn username(&self) -> &str {
        &self.username
    }

    /// `append` — a finished BSON object, moved in.
    pub fn append(&mut self, log_bson: BsonObj) {
        self.log_buffer.push(log_bson);
    }

    /// `clear`.
    pub fn clear(&mut self) {
        self.log_buffer.clear();
    }

    /// `size`.
    pub fn size(&self) -> usize {
        self.log_buffer.len()
    }

    /// The flush-task handover: `FlushTask`'s constructor swaps the buffer
    /// out wholesale (`flush_task.cpp:29-37`).
    pub fn take(&mut self) -> BsonObjBuffer {
        std::mem::take(&mut self.log_buffer)
    }
}

/// `UserLogWriter` (`user_log_writer.hpp:22-61`): everything the reference
/// writes on the background thread for one user.
pub struct UserLogWriter<C: MongoConnection + 'static> {
    connection: Arc<C>,
    uid: u16,
    user_name: String,
    db_name: String,
    active_coll_name: String,
    /// `connectionData_.getIsCluster()`, sampled at `initDB` time — the
    /// cluster status never changes mid-run.
    is_cluster: AtomicBool,
    clock: Arc<dyn Clock>,
}

impl<C: MongoConnection + 'static> UserLogWriter<C> {
    /// The reference constructor (`user_log_writer.cpp:31-41`): the
    /// database name is `USER_DB_PREFIX + userName + LOGGER_ID_SEPARATOR +
    /// loggerID`.
    pub fn new(
        connection: Arc<C>,
        uid: u16,
        user_name: String,
        logger_id: String,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            connection,
            uid,
            db_name: format!(
                "{}{}{}{}",
                constants::USER_DB_PREFIX,
                user_name,
                constants::LOGGER_ID_SEPARATOR,
                logger_id
            ),
            user_name,
            active_coll_name: String::new(),
            is_cluster: AtomicBool::new(false),
            clock,
        }
    }

    /// `getUserName`.
    pub fn user_name(&self) -> &str {
        &self.user_name
    }

    /// `getUserDBName`.
    pub fn db_name(&self) -> &str {
        &self.db_name
    }

    /// The uid this writer stores.
    pub fn uid(&self) -> u16 {
        self.uid
    }

    /// Record the cluster status the connection thread data learned at
    /// startup.
    pub fn set_is_cluster(&self, is_cluster: bool) {
        self.is_cluster.store(is_cluster, Ordering::Relaxed);
    }

    /// `initDB` (`user_log_writer.cpp:49-104`): the uid collection, the
    /// capped server-start collection, the components collection — then
    /// `enablesharding` on a cluster. Any failure drops the half-made
    /// database, over a dead socket too (where the drop fails harmlessly,
    /// as the reference's does).
    pub fn init_db(&mut self, is_cluster: bool) -> Result<bool, MongoError> {
        // just in case
        if self.user_name.is_empty() {
            return Ok(false);
        }
        self.set_is_cluster(is_cluster);

        let mut attempt = || -> Result<bool, MongoError> {
            let mut ok = self.init_uid_collection()?
                && self.init_server_starts_coll()?
                && self.init_components_coll()?;

            if ok && is_cluster {
                // Shard only after the collections exist, so the database
                // does.
                ok = self.shard_database()?;
            }

            Ok(ok)
        };

        let outcome = attempt();
        let ok = match outcome {
            Ok(ok) => ok,
            // A refused command inside one of the inits is the reference's
            // `std::exception` path: fail, no flag touched.
            Err(MongoError::General(_)) => false,
            // The socket died: fail here, and re-raise so the caller clears
            // its connected flag (`setConnected( false )`).
            Err(error @ MongoError::Connection(_)) => {
                let _ = self.connection.drop_database(&self.db_name);
                return Err(error);
            }
        };

        if !ok {
            let _ = self.connection.drop_database(&self.db_name);
        }

        Ok(ok)
    }

    /// `flush` (`user_log_writer.cpp:113-169`): the batch lands in the
    /// active collection as one insert, and the batch clears regardless —
    /// losing logs beats blowing up memory.
    pub fn flush(&mut self, log_buffer: &mut BsonObjBuffer) -> Result<bool, MongoError> {
        // nothing to flush, just return
        if log_buffer.is_empty() {
            return Ok(true);
        }

        // Init collection if no collection is active
        if self.active_coll_name.is_empty() && !self.init_active_coll()? {
            log_buffer.clear();
            return Ok(false);
        }

        let batch: BsonObjBuffer = std::mem::take(log_buffer);
        match self.connection.insert_batch(&self.active_coll_name, &batch) {
            Ok(()) => Ok(true),
            Err(error @ MongoError::Connection(_)) => Err(error),
            Err(_) => Ok(false),
        }
        // `batch` drops here either way: the reference clears the buffer no
        // matter what.
    }

    /// `writeComponent` (`user_log_writer.cpp:176-259`): insert the
    /// component record unless a record with the same host/pid/component
    /// triple is already there. The collection is created on demand.
    pub fn write_component(&mut self, obj: &BsonObj) -> Result<bool, MongoError> {
        let component_coll_name = format!("{}.{}", self.db_name, constants::COMPONENTS_COLL_NAME);

        let coll_list = match self.get_coll_list() {
            Some(list) => list,
            None => return Ok(false),
        };

        if !coll_list.iter().any(|name| name == &component_coll_name)
            && !self
                .connection
                .create_collection(&component_coll_name, 0, false, 0)?
        {
            return Ok(false);
        }

        // The shape of the existing record to look for.
        let mut filter = BsonObj::new();
        filter.append(
            constants::COMPONENTS_KEY_HOST,
            Bson::Int(obj.get_int(constants::COMPONENTS_KEY_HOST)),
        );
        filter.append(
            constants::COMPONENTS_KEY_PID,
            Bson::Int(obj.get_int(constants::COMPONENTS_KEY_PID)),
        );
        filter.append(
            constants::COMPONENTS_KEY_COMPONENT,
            Bson::Int(obj.get_int(constants::COMPONENTS_KEY_COMPONENT)),
        );

        let mut query = Query::new();
        query.filter = filter;
        let rows = self.connection.query(&component_coll_name, &query)?;

        // check if the component already has the info in the collection
        if rows.is_empty() {
            self.connection.insert(&component_coll_name, obj)?;
        }

        Ok(true)
    }

    /// `onServerStart` (`user_log_writer.cpp:265-307`): one startup record
    /// into the capped collection.
    pub fn on_server_start(&mut self, timestamp: u64) -> Result<bool, MongoError> {
        let startup_coll_name = format!("{}.{}", self.db_name, constants::SERVER_STARTS_COLL_NAME);

        let mut record = BsonObj::new();
        record.append(
            constants::SERVER_STARTS_KEY_TIME,
            Bson::Date(timestamp as i64),
        );

        self.connection.insert(&startup_coll_name, &record)?;
        Ok(true)
    }

    /// `rollAndPurge` (`user_log_writer.cpp:314-322`): forget the active
    /// collection — the next flush opens a fresh one, and only when new logs
    /// arrive, so a user that stops logging stops allocating collections —
    /// then run the expiry walk.
    pub fn roll_and_purge(&mut self, seconds: u64) -> Result<bool, MongoError> {
        self.active_coll_name.clear();
        self.purge_expired_colls(seconds)
    }

    /// `purgeExpiredColls` (`user_log_writer.cpp:329-382`): every
    /// `entries_<suffix>` collection whose last entry is older than
    /// `seconds` goes away.
    pub fn purge_expired_colls(&mut self, seconds: u64) -> Result<bool, MongoError> {
        let coll_list = match self.get_coll_list() {
            Some(list) => list,
            None => return Ok(false),
        };

        let entry_coll_name_prefix =
            format!("{}.{}", self.db_name, constants::USER_COLL_ENTRIES_PREFIX);
        let entry_coll_name_length = entry_coll_name_prefix.len() + constants::COLL_SUFFIX_LENGTH;

        let mut ok = true;
        for coll_namespace in coll_list {
            if coll_namespace.len() != entry_coll_name_length
                || !coll_namespace.starts_with(&entry_coll_name_prefix)
            {
                continue;
            }

            let time_suffix = &coll_namespace[entry_coll_name_prefix.len()..];

            // drop the collection if it's an entry collection and expired
            if self.is_collection_expired(&coll_namespace, time_suffix, seconds)? {
                ok = ok && self.drop_collection(&coll_namespace);
            }
        }

        Ok(ok)
    }

    /// `initUIDCollection` (`user_log_writer.cpp:389-418`).
    fn init_uid_collection(&mut self) -> Result<bool, MongoError> {
        let uid_coll_namespace = format!("{}.{}", self.db_name, constants::USER_COLL_UID);

        if !self
            .connection
            .create_collection(&uid_coll_namespace, 0, false, 0)?
        {
            return Ok(false);
        }

        // insert uid
        let mut uid_obj = BsonObj::new();
        uid_obj.append(constants::USER_UID_KEY, Bson::Int(self.uid as i32));

        self.connection.insert(&uid_coll_namespace, &uid_obj)?;
        Ok(true)
    }

    /// `initServerStartsColl` (`user_log_writer.cpp:427-445`): a capped
    /// collection — `SERVER_STARTS_SIZE_LIMIT` bytes, at most
    /// `SERVER_STARTS_COUNT_LIMIT` records.
    fn init_server_starts_coll(&mut self) -> Result<bool, MongoError> {
        let startups_coll_name = format!("{}.{}", self.db_name, constants::SERVER_STARTS_COLL_NAME);

        self.connection.create_collection(
            &startups_coll_name,
            constants::SERVER_STARTS_SIZE_LIMIT,
            true,
            constants::SERVER_STARTS_COUNT_LIMIT,
        )
    }

    /// `initComponentsColl` (`user_log_writer.cpp:453-466`).
    fn init_components_coll(&mut self) -> Result<bool, MongoError> {
        let component_coll_name = format!("{}.{}", self.db_name, constants::COMPONENTS_COLL_NAME);

        self.connection
            .create_collection(&component_coll_name, 0, false, 0)
    }

    /// `shardDatabase` (`user_log_writer.cpp:476-496`): enable sharding of
    /// the user database. A refused command is the `runCommand` false
    /// return, not a socket loss.
    fn shard_database(&self) -> Result<bool, MongoError> {
        let mut shard_obj = BsonObj::new();
        shard_obj.append("enablesharding", Bson::Str(self.db_name.clone()));

        match self
            .connection
            .run_command(constants::ADMIN_DB_NAME, &shard_obj)
        {
            Ok(CommandReply::Ack) => Ok(true),
            Ok(_) => Ok(false),
            Err(MongoError::General(_)) => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// `initActiveColl` (`user_log_writer.cpp:505-593`): create the next
    /// entries collection, index it, shard it on a cluster, and make it the
    /// flush target. Failures after the create drop the collection again.
    fn init_active_coll(&mut self) -> Result<bool, MongoError> {
        let new_coll_name = format!("{}.{}", self.db_name, self.new_coll_name());

        if new_coll_name == self.active_coll_name {
            // Same second as the collection already active.
            return Ok(true);
        }

        // create collection
        if !self
            .connection
            .create_collection(&new_coll_name, 0, false, 0)?
        {
            return Ok(false);
        }

        // create index on the collection
        if !self.create_log_index(&new_coll_name) {
            let _ = self.drop_collection(&new_coll_name);
            return Ok(false);
        }

        // enable sharding of the collection if connected to a cluster. The
        // shard key has two parts: the low-cardinality field from
        // getShardKey that groups records and distributes evenly, and the
        // high-cardinality timestamp for easy chunk splits.
        if self.is_cluster.load(Ordering::Relaxed) {
            let mut key = BsonObj::new();
            key.append(constants::ENTRIES_KEY_SHARD_KEY, Bson::Int(1));
            key.append(constants::ENTRIES_KEY_TIMESTAMP, Bson::Int(1));

            let mut shard_obj = BsonObj::new();
            shard_obj.append("shardcollection", Bson::Str(new_coll_name.clone()));
            shard_obj.append("key", Bson::Doc(key));

            let sharded = match self
                .connection
                .run_command(constants::ADMIN_DB_NAME, &shard_obj)
            {
                Ok(CommandReply::Ack) => true,
                Ok(_) => false,
                Err(MongoError::General(_)) => false,
                Err(error) => return Err(error),
            };

            if !sharded {
                let _ = self.drop_collection(&new_coll_name);
                return Ok(false);
            }
        }

        self.active_coll_name = new_coll_name;

        Ok(true)
    }

    /// `createLogIndex` (`user_log_writer.cpp:601-609`): `{ts:1, cnt:1}` —
    /// keeps query results in insertion order and bounds query scans.
    fn create_log_index(&self, log_coll_namespace: &str) -> bool {
        let mut keys = BsonObj::new();
        keys.append(constants::ENTRIES_KEY_TIMESTAMP, Bson::Int(1));
        keys.append(constants::ENTRIES_KEY_COUNTER, Bson::Int(1));

        self.connection.ensure_index(log_coll_namespace, &keys)
    }

    /// `getNewCollName` (`user_log_writer.cpp:616-623`): the entries
    /// collection name of the current UTC instant.
    fn new_coll_name(&self) -> String {
        format!(
            "{}{}",
            constants::USER_COLL_ENTRIES_PREFIX,
            self.time_stamp_suffix()
        )
    }

    /// `getTimeStampSuffix` (`user_log_writer.cpp:630-643`): `YYYYMMDDHHMMSS`
    /// over UTC (`gmtime_r`).
    fn time_stamp_suffix(&self) -> String {
        let (year, month, day, hour, minute, second) = self.civil_now();
        format!("{year:04}{month:02}{day:02}{hour:02}{minute:02}{second:02}")
    }

    /// The UTC civil fields `gmtime_r` would fill.
    fn civil_now(&self) -> (i64, u32, u32, u32, u32, u32) {
        let now = self.clock.now_secs();
        let days = now.div_euclid(86_400);
        let secs_of_day = now.rem_euclid(86_400);
        let (year, month, day) = civil_from_days(days);
        (
            year,
            month,
            day,
            (secs_of_day / 3600) as u32,
            ((secs_of_day % 3600) / 60) as u32,
            (secs_of_day % 60) as u32,
        )
    }

    /// `getCollList` (`user_log_writer.cpp:649-660`).
    fn get_coll_list(&self) -> Option<CollList> {
        get_collection_names(self.connection.as_ref(), &self.db_name)
    }

    /// `isCollectionExpired` (`user_log_writer.cpp:667-757`): a collection
    /// is expired when its suffix is at least `seconds` old *and* its last
    /// entry is older than `seconds`. The suffix check saves the query for
    /// collections still inside the window; the entry check keeps a
    /// collection written to recently alive past its nominal date.
    fn is_collection_expired(
        &self,
        coll_ns: &str,
        time_suffix: &str,
        seconds: u64,
    ) -> Result<bool, MongoError> {
        let now = self.clock.now_secs();

        let Some(coll_time) = parse_suffix(time_suffix) else {
            return Ok(false);
        };

        if now.saturating_sub(coll_time) < seconds as i64 {
            return Ok(false);
        }

        // Check the last record in the collection now. Sort ts and cnt
        // descending; only return those fields so the index covers the
        // query.
        let mut sort = BsonObj::new();
        sort.append(constants::ENTRIES_KEY_TIMESTAMP, Bson::Int(-1));
        sort.append(constants::ENTRIES_KEY_COUNTER, Bson::Int(-1));

        let mut pattern = BsonObj::new();
        pattern.append(constants::ENTRIES_KEY_TIMESTAMP, Bson::Int(1));
        pattern.append(constants::ENTRIES_KEY_COUNTER, Bson::Int(1));

        let mut query = Query::new();
        query.sort = Some(sort);
        query.return_pattern = Some(pattern);
        query.limit = Some(1);

        let rows = self.connection.query(coll_ns, &query)?;

        let Some(last_entry) = rows.into_iter().next() else {
            // Empty: nothing keeps it alive.
            return Ok(true);
        };

        let log_time = last_entry.get_date(constants::ENTRIES_KEY_TIMESTAMP) / 1000;
        Ok(now.saturating_sub(log_time) > seconds as i64)
    }

    /// `dropCollection` (`user_log_writer.cpp:763-801`).
    fn drop_collection(&self, coll_ns: &str) -> bool {
        self.connection.drop_collection(coll_ns)
    }
}

/// `sscanf( timeSuffix, "%04d%02d%02d%02d%02d%02d" )` + `timegm`
/// (`user_log_writer.cpp:675-691`): the UTC epoch seconds a suffix names.
fn parse_suffix(time_suffix: &str) -> Option<i64> {
    let bytes = time_suffix.as_bytes();
    if bytes.len() != constants::COLL_SUFFIX_LENGTH || !bytes.iter().all(u8::is_ascii_digit) {
        return None;
    }

    let pair = |from: usize| -> Option<i64> { time_suffix[from..from + 2].parse().ok() };
    let year: i64 = time_suffix[..4].parse().ok()?;
    let month = pair(4)?;
    let day = pair(6)?;
    let hour = pair(8)?;
    let minute = pair(10)?;
    let second = pair(12)?;

    Some(days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mongodb::driver::InMemoryStore;
    use crate::mongodb::types::WallClock;

    fn writer(store: &Arc<InMemoryStore>) -> UserLogWriter<InMemoryStore> {
        UserLogWriter::new(
            Arc::clone(store),
            1000,
            "root".to_string(),
            String::new(),
            Arc::new(WallClock),
        )
    }

    #[test]
    fn a_suffix_parses_back_to_its_utc_instant() {
        assert_eq!(parse_suffix("20140820101400"), Some(1_408_529_640));
        assert_eq!(parse_suffix("19700101000000"), Some(0));
        assert_eq!(parse_suffix("2014"), None);
        assert_eq!(parse_suffix("2014-8-2010:1400"), None);
        assert_eq!(parse_suffix("2014082010140a"), None);
    }

    #[test]
    fn the_suffix_round_trips_through_the_clock() {
        let store = Arc::new(InMemoryStore::new());
        let suffix = writer(&store).time_stamp_suffix();
        let parsed = parse_suffix(&suffix).expect("round trip");
        let now = WallClock.now_secs();
        assert!((now - parsed).abs() < 2, "{suffix} vs {now}");
    }

    #[test]
    fn init_db_builds_the_user_collections_or_refuses_an_empty_name() {
        let store = Arc::new(InMemoryStore::new());
        let mut good = writer(&store);
        assert!(good.init_db(false).unwrap());
        let db = good.db_name().to_string();

        assert_eq!(
            good.db_name(),
            format!(
                "{}root{}",
                constants::USER_DB_PREFIX,
                constants::LOGGER_ID_SEPARATOR
            )
        );

        let names = store.collection_names(&db).unwrap();
        assert!(names.contains(&format!("{db}.{}", constants::USER_COLL_UID)));
        assert!(names.contains(&format!("{db}.{}", constants::SERVER_STARTS_COLL_NAME)));
        assert!(names.contains(&format!("{db}.{}", constants::COMPONENTS_COLL_NAME)));

        // The uid record is in.
        let uid_docs = store.documents(&format!("{db}.{}", constants::USER_COLL_UID));
        assert_eq!(uid_docs.len(), 1);
        assert_eq!(uid_docs[0].get_int(constants::USER_UID_KEY), 1000);

        let mut empty = UserLogWriter::new(
            Arc::clone(&store),
            1,
            String::new(),
            String::new(),
            Arc::new(WallClock),
        );
        assert_eq!(empty.init_db(false), Ok(false));
    }

    #[test]
    fn a_failed_init_drops_the_half_made_database() {
        let store = Arc::new(InMemoryStore::new());
        let mut writer = writer(&store);
        let db = writer.db_name().to_string();

        // The uid collection lands, then creation breaks.
        assert!(
            store
                .create_collection(&format!("{db}.{}", constants::USER_COLL_UID), 0, false, 0)
                .unwrap()
        );
        store.set_fail_next_creates(1);
        assert_eq!(writer.init_db(false), Ok(false));

        // The rollback ran while connected: nothing of this user survives.
        assert!(store.collection_names(&db).unwrap().is_empty());
    }

    #[test]
    fn a_socket_loss_during_init_rolls_back_and_raises() {
        let store = Arc::new(InMemoryStore::new());
        let mut writer = writer(&store);
        let db = writer.db_name().to_string();
        store
            .create_collection(&format!("{db}.{}", constants::USER_COLL_UID), 0, false, 0)
            .unwrap();
        store.drop_connection();

        assert!(matches!(
            writer.init_db(false),
            Err(MongoError::Connection(_))
        ));
        // The connection comes back to reveal the state the outage left:
        // the dropDatabase also ran into the dead socket, so the uid
        // collection survives, as the reference's failed rollback does.
        store.restore_connection();
        assert_eq!(
            store.collection_names(&db).unwrap(),
            vec![format!("{db}.{}", constants::USER_COLL_UID)]
        );
    }

    #[test]
    fn server_starts_land_in_the_capped_collection() {
        let store = Arc::new(InMemoryStore::new());
        let mut writer = writer(&store);
        assert!(writer.init_db(false).unwrap());

        assert!(writer.on_server_start(1_408_529_640_000).unwrap());
        let db = writer.db_name().to_string();
        let docs = store.documents(&format!("{db}.{}", constants::SERVER_STARTS_COLL_NAME));
        assert_eq!(docs.len(), 1);
        assert_eq!(
            docs[0].get_date(constants::SERVER_STARTS_KEY_TIME),
            1_408_529_640_000
        );
    }

    #[test]
    fn components_deduplicate_on_the_host_pid_component_triple() {
        let store = Arc::new(InMemoryStore::new());
        let mut writer = writer(&store);
        assert!(writer.init_db(false).unwrap());

        let record = {
            let mut obj = BsonObj::new();
            obj.append(constants::COMPONENTS_KEY_HOST, Bson::Int(1));
            obj.append(constants::COMPONENTS_KEY_PID, Bson::Int(42));
            obj.append(constants::COMPONENTS_KEY_COMPONENT, Bson::Int(3));
            obj.append(constants::COMPONENTS_KEY_APPID, Bson::Int(0));
            obj
        };

        assert!(writer.write_component(&record).unwrap());
        // Same triple again: no second row.
        assert!(writer.write_component(&record).unwrap());

        let db = writer.db_name().to_string();
        let components_ns = format!("{db}.{}", constants::COMPONENTS_COLL_NAME);
        assert_eq!(store.documents(&components_ns).len(), 1);

        // A different pid writes a second row.
        let other = {
            let mut obj = BsonObj::new();
            obj.append(constants::COMPONENTS_KEY_HOST, Bson::Int(1));
            obj.append(constants::COMPONENTS_KEY_PID, Bson::Int(43));
            obj.append(constants::COMPONENTS_KEY_COMPONENT, Bson::Int(3));
            obj.append(constants::COMPONENTS_KEY_APPID, Bson::Int(0));
            obj
        };
        assert!(writer.write_component(&other).unwrap());
        assert_eq!(store.documents(&components_ns).len(), 2);
    }

    #[test]
    fn a_flush_opens_a_dated_collection_and_lands_the_batch() {
        let store = Arc::new(InMemoryStore::new());
        let mut writer = writer(&store);
        assert!(writer.init_db(false).unwrap());

        let mut buffer: BsonObjBuffer = Vec::new();
        let mut doc = BsonObj::new();
        doc.append("msg", Bson::Str("hello".into()));
        buffer.push(doc);

        assert!(writer.flush(&mut buffer).unwrap());
        // The batch clears either way.
        assert!(buffer.is_empty());

        let db = writer.db_name().to_string();
        let names = store.collection_names(&db).unwrap();
        assert_eq!(names.len(), 4); // uid, server starts, components, entries_N
        let entries: Vec<_> = names
            .into_iter()
            .filter(|name| name.contains(constants::USER_COLL_ENTRIES_PREFIX))
            .collect();
        assert_eq!(entries.len(), 1);
        assert_eq!(store.documents(&entries[0]).len(), 1);

        // An empty buffer is a no-op flush.
        assert!(writer.flush(&mut Vec::new()).unwrap());
    }

    #[test]
    fn rolling_reopens_the_active_collection_on_the_next_flush() {
        let store = Arc::new(InMemoryStore::new());
        let mut writer = writer(&store);
        assert!(writer.init_db(false).unwrap());

        let mut buffer: BsonObjBuffer = Vec::new();
        let mut doc = BsonObj::new();
        doc.append("msg", Bson::Str("one".into()));
        buffer.push(doc);
        writer.flush(&mut buffer).unwrap();

        // A roll with a far-off expiry drops nothing; the next flush runs
        // initActiveColl again. Inside the same second the name matches the
        // old collection, which createCollection accepts idempotently.
        assert_eq!(writer.roll_and_purge(601_200), Ok(true));
        let mut second = BsonObj::new();
        second.append("msg", Bson::Str("two".into()));
        buffer.push(second);
        assert!(writer.flush(&mut buffer).unwrap());

        let db = writer.db_name().to_string();
        let entries: Vec<_> = store
            .collection_names(&db)
            .unwrap()
            .into_iter()
            .filter(|name| name.contains(constants::USER_COLL_ENTRIES_PREFIX))
            .collect();
        let total: usize = entries.iter().map(|ns| store.documents(ns).len()).sum();
        assert_eq!(total, 2);
    }

    #[test]
    fn expiry_walks_suffix_then_last_entry() {
        let store = Arc::new(InMemoryStore::new());
        let mut writer = writer(&store);
        assert!(writer.init_db(false).unwrap());
        let db = writer.db_name().to_string();
        let now = WallClock.now_secs();
        let expire_seconds = 6 * 86_400; // one week minus an hour, roughly

        // A stale collection with an old last entry expires.
        let stale_suffix = suffix_for(now - 90 * 86_400);
        let stale_ns = format!(
            "{}.{}{}",
            db,
            constants::USER_COLL_ENTRIES_PREFIX,
            stale_suffix
        );
        store.create_collection(&stale_ns, 0, false, 0).unwrap();
        let mut old = BsonObj::new();
        old.append(
            constants::ENTRIES_KEY_TIMESTAMP,
            Bson::Date((now - 90 * 86_400) * 1000),
        );
        store.insert(&stale_ns, &old).unwrap();

        // A collection whose suffix is inside the window is never even
        // queried.
        let young_suffix = suffix_for(now - 86_400);
        let young_ns = format!(
            "{}.{}{}",
            db,
            constants::USER_COLL_ENTRIES_PREFIX,
            young_suffix
        );
        store.create_collection(&young_ns, 0, false, 0).unwrap();
        let mut recent = BsonObj::new();
        recent.append(
            constants::ENTRIES_KEY_TIMESTAMP,
            Bson::Date((now - 3600) * 1000),
        );
        store.insert(&young_ns, &recent).unwrap();

        assert!(writer.purge_expired_colls(expire_seconds).unwrap());
        assert!(store.documents(&stale_ns).is_empty(), "stale dropped");
        assert_eq!(store.documents(&young_ns).len(), 1, "young kept");
    }

    #[test]
    fn a_recently_written_old_collection_survives_expiry() {
        let store = Arc::new(InMemoryStore::new());
        let mut writer = writer(&store);
        assert!(writer.init_db(false).unwrap());
        let db = writer.db_name().to_string();
        let now = WallClock.now_secs();

        // Created 90 days ago but written to an hour ago: the entry check
        // keeps it.
        let ns = format!(
            "{}.{}{}",
            db,
            constants::USER_COLL_ENTRIES_PREFIX,
            suffix_for(now - 90 * 86_400)
        );
        store.create_collection(&ns, 0, false, 0).unwrap();
        let mut entry = BsonObj::new();
        entry.append(
            constants::ENTRIES_KEY_TIMESTAMP,
            Bson::Date((now - 3600) * 1000),
        );
        store.insert(&ns, &entry).unwrap();

        assert_eq!(writer.purge_expired_colls(6 * 86_400), Ok(true));
        assert_eq!(store.documents(&ns).len(), 1, "recent write keeps it");
    }

    #[test]
    fn sharding_runs_when_the_store_is_a_cluster() {
        let store = Arc::new(InMemoryStore::new_clustered());
        let mut writer = writer(&store);
        assert!(writer.init_db(true).unwrap());

        let mut buffer: BsonObjBuffer = Vec::new();
        let mut doc = BsonObj::new();
        doc.append("msg", Bson::Str("hello".into()));
        buffer.push(doc);
        assert!(writer.flush(&mut buffer).unwrap());

        // The shardcollection command answered Ack, and a second flush into
        // the already-active collection runs the same-second shortcut.
        assert!(writer.flush(&mut buffer).is_ok());
    }

    fn suffix_for(epoch_secs: i64) -> String {
        let days = epoch_secs.div_euclid(86_400);
        let secs = epoch_secs.rem_euclid(86_400);
        let (year, month, day) = civil_from_days(days);
        format!(
            "{year:04}{month:02}{day:02}{:02}{:02}{:02}",
            secs / 3600,
            (secs % 3600) / 60,
            secs % 60
        )
    }
}
