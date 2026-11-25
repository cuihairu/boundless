//! The driver seam (`driver.rs`).
//!
//! The reference holds a `mongo::DBClientConnection` and every operation
//! goes through it: `createCollection`, `insert`, `query`, `update`,
//! `dropCollection`, `dropDatabase`, `ensureIndex`, `runCommand`, and the
//! sticky `getLastError` string most call sites test instead of an
//! exception (`user_log_writer.cpp:136-146`, `log_storage.cpp:800-819`).
//! [`MongoConnection`] is that surface — one method per driver call the
//! backend makes — and [`InMemoryStore`] is the implementation the fixtures
//! run against.
//!
//! Error convention, mapping the reference's two failure shapes:
//! `Err(MongoError::Connection(_))` is the `mongo::SocketException` path —
//! the caller owns its connected flag and clears it. `Err(MongoError::General(_))`
//! is a refused command: the `runCommand` false return whose reply carries
//! `errmsg`, or a `getLastError` that came back non-empty.
//!
//! Documents crossing the seam are the wire bytes of
//! [`crate::mongodb::bson`]: the store keeps encoded objects and hands
//! decoded ones back, so the storage path is exercised through real BSON
//! rather than in-memory values.

use crate::mongodb::bson::{Bson, BsonObj};

/// What went wrong, in the reference's two flavours.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum MongoError {
    /// The socket failed: the connection is down and must be re-established.
    /// `mongo::SocketException`.
    Connection(String),
    /// A command failed or returned an error string.
    /// `DB_TASK_GENERAL_ERROR`.
    General(String),
}

/// The parsed reply of a successful `runCommand`. The reference inspects the
/// raw reply document field by field; the port narrows it to the three
/// replies the backend actually reads.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum CommandReply {
    /// An `ismaster` reply, read for its `msg` field
    /// (`initClusterStatus`, `connection_thread_data.cpp:102-149`).
    IsMaster {
        /// The `msg` field; `isdbgrid` on a sharded cluster.
        msg: String,
    },
    /// A `listDatabases` reply, read for the database names
    /// (`getUserDatabases`, `connection_thread_data.cpp:319-379`).
    ListDatabases(Vec<String>),
    /// Any other `ok: 1` reply (`enablesharding`, `shardcollection`).
    Ack,
}

/// A query, in the shape the driver's `DBClientCursor` is built from:
/// filter, sort order, return pattern, and row limit.
#[derive(Clone, Debug)]
pub struct Query {
    /// `query.filter(...)`: an empty document matches everything, as
    /// `mongo::BSONObj()` does.
    pub filter: BsonObj,
    /// `query.sort(...)`: field to signed order.
    pub sort: Option<BsonObj>,
    /// The fields to hand back (`findOne`'s `returnPattern`).
    pub return_pattern: Option<BsonObj>,
    /// The row limit; `None` or zero means unlimited.
    pub limit: Option<i32>,
}

impl Default for Query {
    fn default() -> Self {
        Self {
            filter: BsonObj::new(),
            sort: None,
            return_pattern: None,
            limit: None,
        }
    }
}

impl Query {
    /// An unrestricted query.
    pub fn new() -> Self {
        Self::default()
    }
}

/// `mongo::DBClientConnection`, narrowed to the calls the backend makes.
pub trait MongoConnection: Send + Sync {
    /// `setSoTimeout(seconds)`.
    fn set_so_timeout(&self, seconds: u32);

    /// `connect(host, port)`.
    fn connect(&self, host: &str, port: u16) -> bool;

    /// `auth(database, user, password)` — `connectToDB` authenticates
    /// against the admin database (`log_storage.cpp:644-681`).
    fn auth(&self, database: &str, user: &str, passwd: &str) -> bool;

    /// `createCollection(ns, size, capped, max)`: false when the server
    /// refuses (including over a dead socket, where the reference gets its
    /// false back from the caught driver call).
    fn create_collection(
        &self,
        namespace: &str,
        size: u32,
        capped: bool,
        max: u32,
    ) -> Result<bool, MongoError>;

    /// `insert(ns, obj)` — one document. A refused write is
    /// `Err(MongoError::General)`, mirroring the non-empty `getLastError`
    /// the reference checks; a dead socket is `Err(MongoError::Connection)`.
    fn insert(&self, namespace: &str, doc: &BsonObj) -> Result<(), MongoError>;

    /// `insert(ns, BSONObjBuffer)` — a whole flush batch
    /// (`user_log_writer.cpp:136`).
    fn insert_batch(&self, namespace: &str, docs: &[BsonObj]) -> Result<(), MongoError>;

    /// `update(ns, existingRecord, newRecord)`
    /// (`connection_thread_data.cpp:482-521`): replace the one row that
    /// equals `existing` on every field it names.
    fn update(
        &self,
        namespace: &str,
        existing: &BsonObj,
        new_record: &BsonObj,
    ) -> Result<(), MongoError>;

    /// `query(ns, filter, sort, returnPattern, limit)`: the matching rows in
    /// sorted order.
    fn query(&self, namespace: &str, query: &Query) -> Result<Vec<BsonObj>, MongoError>;

    /// `dropCollection(ns)`.
    fn drop_collection(&self, namespace: &str) -> bool;

    /// `dropDatabase(db)`.
    fn drop_database(&self, database: &str) -> bool;

    /// The reference's `getCollectionNames` workaround
    /// (`connection_thread_data.cpp:639-684`, BWT-28917): the fully
    /// qualified `db.collection` names of one database, minus any collection
    /// whose name holds a `$`.
    fn collection_names(&self, database: &str) -> Result<Vec<String>, MongoError>;

    /// `ensureIndex(ns, keyPattern)`.
    fn ensure_index(&self, namespace: &str, keys: &BsonObj) -> bool;

    /// `runCommand(db, cmd)`: `Ok` is a parsed `ok: 1` reply,
    /// `Err(MongoError::General)` is the `ok: 0` reply (the `runCommand`
    /// false return, its `errmsg` carried in the error), and
    /// `Err(MongoError::Connection)` is the socket loss.
    fn run_command(&self, database: &str, command: &BsonObj) -> Result<CommandReply, MongoError>;

    /// Whether the connection is live.
    fn is_connected(&self) -> bool;

    /// `getLastError()`: the sticky error text of the last write, empty when
    /// the last write succeeded.
    fn last_error(&self) -> String;
}

/// A database's collections in the store.
#[derive(Default, Debug)]
struct Collection {
    /// Encoded documents, in insertion order.
    docs: Vec<Vec<u8>>,
    /// A capped collection's byte budget (`SERVER_STARTS_SIZE_LIMIT`),
    /// zero when uncapped.
    byte_size: u32,
    /// A capped collection's row budget (`SERVER_STARTS_COUNT_LIMIT`).
    max_docs: u32,
}

/// One in-memory database.
#[derive(Default, Debug)]
struct Database {
    collections: std::collections::BTreeMap<String, Collection>,
}

impl Database {
    fn collection(&self, name: &str) -> Option<&Collection> {
        self.collections.get(name)
    }
}

/// The store a backend runs against when no real server is reachable.
pub struct InMemoryStore {
    state: std::sync::Mutex<State>,
}

#[derive(Default, Debug)]
struct State {
    databases: std::collections::BTreeMap<String, Database>,
    connected: bool,
    /// The whole server reachable or not (`stop_server`). A dropped socket
    /// (`connected == false`, `server_up == true`) is repaired by the next
    /// `connect`; a stopped server refuses every `connect`.
    server_up: bool,
    last_error: String,
    /// `ismaster` answers with a `msg` containing this when the server is a
    /// mongos (`initClusterStatus`, `connection_thread_data.cpp:102-149`).
    isdbgrid: bool,
    /// Fixture seam: the next this many `createCollection` calls return
    /// false, the way a full disk or a permissions problem would.
    fail_next_creates: u32,
}

impl InMemoryStore {
    /// An empty store with a live connection — `new` + `connect` + `auth`
    /// collapsed, so fixtures can talk to it directly.
    pub fn new() -> Self {
        Self {
            state: std::sync::Mutex::new(State {
                connected: true,
                server_up: true,
                ..State::default()
            }),
        }
    }

    /// A store whose `ismaster` reports a sharded cluster.
    pub fn new_clustered() -> Self {
        let store = Self::new();
        store.state.lock().unwrap().isdbgrid = true;
        store
    }

    /// Turn the connection off, as a dropped socket would. The next
    /// `connect` repairs it.
    pub fn drop_connection(&self) {
        self.state.lock().unwrap().connected = false;
    }

    /// Take the whole server down: `connect` refuses until
    /// [`Self::start_server`], the way an unreachable host does.
    pub fn stop_server(&self) {
        let mut state = self.state.lock().unwrap();
        state.server_up = false;
        state.connected = false;
    }

    /// Bring the server back up.
    pub fn start_server(&self) {
        self.state.lock().unwrap().server_up = true;
    }

    /// Turn the connection back on.
    pub fn restore_connection(&self) {
        self.state.lock().unwrap().connected = true;
    }

    /// Make the next `n` `createCollection` calls fail.
    pub fn set_fail_next_creates(&self, n: u32) {
        self.state.lock().unwrap().fail_next_creates = n;
    }

    /// The encoded bytes the store holds for one collection — how the
    /// fixtures inspect what a backend actually wrote.
    pub fn raw_documents(&self, namespace: &str) -> Vec<Vec<u8>> {
        let (db, coll) = split_namespace(namespace);
        let state = self.state.lock().unwrap();
        state
            .databases
            .get(&db)
            .and_then(|database| database.collection(&coll))
            .map(|c| c.docs.clone())
            .unwrap_or_default()
    }

    /// The decoded documents of one collection.
    pub fn documents(&self, namespace: &str) -> Vec<BsonObj> {
        self.raw_documents(namespace)
            .into_iter()
            .filter_map(|bytes| BsonObj::decode(&bytes))
            .collect()
    }

    /// The names of every collection the store knows, fully qualified.
    pub fn known_collections(&self) -> Vec<String> {
        let state = self.state.lock().unwrap();
        let mut out = Vec::new();
        for (db, database) in &state.databases {
            for coll in database.collections.keys() {
                out.push(format!("{db}.{coll}"));
            }
        }
        out
    }
}

impl Default for InMemoryStore {
    fn default() -> Self {
        Self::new()
    }
}

/// Split `db.collection` on its first dot, the way the driver does.
fn split_namespace(namespace: &str) -> (String, String) {
    match namespace.find('.') {
        Some(at) => (namespace[..at].to_string(), namespace[at + 1..].to_string()),
        None => (namespace.to_string(), String::new()),
    }
}

/// Whether one stored document matches a filter document: every field the
/// filter names must equal the row's, the driver's equality semantics.
fn matches(doc: &BsonObj, filter: &BsonObj) -> bool {
    filter
        .fields()
        .iter()
        .all(|(key, want)| doc.get(key) == Some(want))
}

/// Apply a sort pattern: fields in the order given, each ascending or
/// descending by its signed order value.
fn sort_docs(docs: &mut [BsonObj], pattern: &BsonObj) {
    // The driver sorts by the whole pattern at once; a stable sort applied
    // least-significant field first reproduces that.
    for (key, order) in pattern.fields().iter().rev() {
        let descending = matches!(order, Bson::Int(n) if *n < 0);
        docs.sort_by(|a, b| {
            let cmp = cmp_values(a.get(key), b.get(key));
            if descending { cmp.reverse() } else { cmp }
        });
    }
}

/// Total order over the value kinds the backend sorts on.
fn cmp_values(lhs: Option<&Bson>, rhs: Option<&Bson>) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (lhs, rhs) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(a), Some(b)) => match (a, b) {
            (Bson::Int(x), Bson::Int(y)) => x.cmp(y),
            (Bson::Long(x), Bson::Long(y)) => x.cmp(y),
            (Bson::Int(x), Bson::Long(y)) => (*x as i64).cmp(y),
            (Bson::Long(x), Bson::Int(y)) => x.cmp(&(*y as i64)),
            (Bson::Date(x), Bson::Date(y)) => x.cmp(y),
            (Bson::Str(x), Bson::Str(y)) => x.cmp(y),
            (Bson::Bool(x), Bson::Bool(y)) => x.cmp(y),
            _ => json_rank(a).cmp(&json_rank(b)),
        },
    }
}

/// A fallback rank for value kinds the backend never sorts on.
fn json_rank(value: &Bson) -> u8 {
    match value {
        Bson::Null => 0,
        Bson::Double(_) => 1,
        Bson::Str(_) => 2,
        Bson::Doc(_) => 3,
        Bson::Bool(_) => 4,
        Bson::Date(_) => 5,
        Bson::Int(_) | Bson::Long(_) => 6,
    }
}

/// A row narrowed to the fields a return pattern names.
fn project(doc: &BsonObj, pattern: &BsonObj) -> BsonObj {
    let mut out = BsonObj::new();
    for (key, value) in doc.fields() {
        if pattern.get(key).is_some() {
            out.append(key, value.clone());
        }
    }
    out
}

impl MongoConnection for InMemoryStore {
    fn set_so_timeout(&self, _seconds: u32) {}

    fn connect(&self, _host: &str, _port: u16) -> bool {
        let mut state = self.state.lock().unwrap();
        if !state.server_up {
            return false;
        }
        state.connected = true;
        true
    }

    fn auth(&self, _database: &str, _user: &str, _passwd: &str) -> bool {
        self.state.lock().unwrap().connected
    }

    fn create_collection(
        &self,
        namespace: &str,
        size: u32,
        capped: bool,
        max: u32,
    ) -> Result<bool, MongoError> {
        let (db, coll) = split_namespace(namespace);
        if coll.is_empty() {
            return Ok(false);
        }
        let mut state = self.state.lock().unwrap();
        if !state.connected {
            state.last_error = "connection closed".to_string();
            return Err(MongoError::Connection("connection closed".into()));
        }
        if state.fail_next_creates > 0 {
            state.fail_next_creates -= 1;
            state.last_error = "create failed".to_string();
            return Ok(false);
        }
        let database = state.databases.entry(db).or_default();
        if database.collections.contains_key(&coll) {
            // The legacy server accepts a create on an existing collection,
            // which is what makes a reopened writer's first flush able to
            // reuse the collection a previous run created.
            return Ok(true);
        }
        database.collections.insert(
            coll,
            Collection {
                docs: Vec::new(),
                byte_size: if capped { size } else { 0 },
                max_docs: if capped { max } else { 0 },
            },
        );
        Ok(true)
    }

    fn insert(&self, namespace: &str, doc: &BsonObj) -> Result<(), MongoError> {
        self.insert_batch(namespace, std::slice::from_ref(doc))
    }

    fn insert_batch(&self, namespace: &str, docs: &[BsonObj]) -> Result<(), MongoError> {
        let encoded: Vec<Vec<u8>> = docs.iter().map(BsonObj::encode).collect();
        let (db, coll) = split_namespace(namespace);
        let mut state = self.state.lock().unwrap();
        if !state.connected {
            state.last_error = "connection closed".to_string();
            return Err(MongoError::Connection("connection closed".into()));
        }
        let Some(database) = state.databases.get_mut(&db) else {
            state.last_error = format!("ns {namespace} not found");
            return Err(MongoError::General(state.last_error.clone()));
        };
        let Some(collection) = database.collections.get_mut(&coll) else {
            state.last_error = format!("collection {namespace} does not exist");
            return Err(MongoError::General(state.last_error.clone()));
        };
        // Capped collections drop their oldest rows past the budget, as a
        // MongoDB capped collection does.
        while collection.max_docs != 0 && collection.docs.len() as u32 >= collection.max_docs {
            collection.docs.remove(0);
        }
        while collection.byte_size != 0 {
            let used: usize = collection.docs.iter().map(|doc| doc.len()).sum();
            let incoming = encoded.iter().map(|doc| doc.len()).sum::<usize>() as u32;
            if used as u32 + incoming <= collection.byte_size {
                break;
            }
            if collection.docs.is_empty() {
                break;
            }
            collection.docs.remove(0);
        }
        collection.docs.extend(encoded);
        state.last_error.clear();
        Ok(())
    }

    fn query(&self, namespace: &str, query: &Query) -> Result<Vec<BsonObj>, MongoError> {
        let (db, coll) = split_namespace(namespace);
        let mut state = self.state.lock().unwrap();
        if !state.connected {
            state.last_error = "connection closed".to_string();
            return Err(MongoError::Connection("connection closed".into()));
        }
        let Some(collection) = state
            .databases
            .get(&db)
            .and_then(|database| database.collection(&coll))
        else {
            // A query on an absent collection answers an empty cursor, not
            // an error, in the legacy driver.
            return Ok(Vec::new());
        };
        let mut rows: Vec<BsonObj> = collection
            .docs
            .iter()
            .filter_map(|bytes| BsonObj::decode(bytes))
            .filter(|doc| matches(doc, &query.filter))
            .collect();
        if let Some(pattern) = &query.sort {
            sort_docs(&mut rows, pattern);
        }
        if let Some(pattern) = &query.return_pattern {
            rows = rows.into_iter().map(|doc| project(&doc, pattern)).collect();
        }
        if let Some(limit) = query.limit.filter(|limit| *limit > 0) {
            rows.truncate(limit as usize);
        }
        Ok(rows)
    }

    fn update(
        &self,
        namespace: &str,
        existing: &BsonObj,
        new_record: &BsonObj,
    ) -> Result<(), MongoError> {
        let (db, coll) = split_namespace(namespace);
        let mut state = self.state.lock().unwrap();
        if !state.connected {
            state.last_error = "connection closed".to_string();
            return Err(MongoError::Connection("connection closed".into()));
        }
        let Some(collection) = state
            .databases
            .get_mut(&db)
            .and_then(|database| database.collections.get_mut(&coll))
        else {
            return Err(MongoError::General("ns not found".into()));
        };
        let encoded = new_record.encode();
        for stored in &mut collection.docs {
            if BsonObj::decode(stored).is_some_and(|doc| matches(&doc, existing)) {
                *stored = encoded;
                state.last_error.clear();
                return Ok(());
            }
        }
        state.last_error = format!("no rows match {namespace}");
        Err(MongoError::General(state.last_error.clone()))
    }

    fn drop_collection(&self, namespace: &str) -> bool {
        let (db, coll) = split_namespace(namespace);
        let mut state = self.state.lock().unwrap();
        if !state.connected {
            return false;
        }
        let dropped = state
            .databases
            .get_mut(&db)
            .and_then(|database| database.collections.remove(&coll))
            .is_some();
        if dropped {
            state.last_error.clear();
        }
        dropped
    }

    fn drop_database(&self, database_name: &str) -> bool {
        let mut state = self.state.lock().unwrap();
        if !state.connected {
            return false;
        }
        let dropped = state.databases.remove(database_name).is_some();
        if dropped {
            state.last_error.clear();
        }
        dropped
    }

    fn collection_names(&self, database: &str) -> Result<Vec<String>, MongoError> {
        let state = self.state.lock().unwrap();
        if !state.connected {
            return Err(MongoError::Connection("connection closed".into()));
        }
        let mut out = Vec::new();
        if let Some(db) = state.databases.get(database) {
            for name in db.collections.keys() {
                if !name.contains('$') {
                    out.push(format!("{database}.{name}"));
                }
            }
        }
        Ok(out)
    }

    fn ensure_index(&self, namespace: &str, _keys: &BsonObj) -> bool {
        let (db, coll) = split_namespace(namespace);
        let mut state = self.state.lock().unwrap();
        if !state.connected {
            state.last_error = "connection closed".to_string();
            return false;
        }
        state
            .databases
            .get(&db)
            .and_then(|database| database.collection(&coll))
            .is_some()
    }

    fn run_command(&self, _database: &str, command: &BsonObj) -> Result<CommandReply, MongoError> {
        let mut state = self.state.lock().unwrap();
        if !state.connected {
            state.last_error = "connection closed".to_string();
            return Err(MongoError::Connection("connection closed".into()));
        }
        if command.has_field("ismaster") {
            let marker = if state.isdbgrid {
                "isdbgrid"
            } else {
                "standalone"
            };
            return Ok(CommandReply::IsMaster {
                msg: marker.to_string(),
            });
        }
        if command.has_field("listDatabases") {
            return Ok(CommandReply::ListDatabases(
                state.databases.keys().cloned().collect(),
            ));
        }
        if command.has_field("enablesharding") || command.has_field("shardcollection") {
            return Ok(CommandReply::Ack);
        }
        // An unknown command is the ok:0 reply.
        Err(MongoError::General("no such command".into()))
    }

    fn is_connected(&self) -> bool {
        self.state.lock().unwrap().connected
    }

    fn last_error(&self) -> String {
        self.state.lock().unwrap().last_error.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_land_as_bson_and_read_back() {
        let store = InMemoryStore::new();
        assert!(store.create_collection("db.c", 0, false, 0).unwrap());
        let mut doc = BsonObj::new();
        doc.append("host", Bson::Int(10));
        doc.append("pid", Bson::Int(44));
        assert!(store.insert("db.c", &doc).is_ok());
        assert_eq!(store.last_error(), "");

        let raw = store.raw_documents("db.c");
        assert_eq!(raw.len(), 1);
        assert_eq!(
            BsonObj::decode(&raw[0]).expect("bson"),
            store.documents("db.c")[0]
        );

        let docs = store.documents("db.c");
        assert_eq!(docs[0].get_int("host"), 10);
        assert_eq!(docs[0].get_int("pid"), 44);
    }

    #[test]
    fn queries_filter_sort_and_project() {
        let store = InMemoryStore::new();
        store.create_collection("db.e", 0, false, 0).unwrap();
        for i in [1, 3, 2] {
            let mut doc = BsonObj::new();
            doc.append("ts", Bson::Date(i * 1000));
            doc.append("cnt", Bson::Int(i as i32));
            doc.append("extra", Bson::Str("hidden".into()));
            store.insert("db.e", &doc).unwrap();
        }

        // Newest first, only the fields the pattern names.
        let mut pattern = BsonObj::new();
        pattern.append("ts", Bson::Int(-1));
        let mut return_pattern = BsonObj::new();
        return_pattern.append("ts", Bson::Int(1));
        return_pattern.append("cnt", Bson::Int(1));
        let mut query = Query::new();
        query.sort = Some(pattern);
        query.return_pattern = Some(return_pattern);
        query.limit = Some(2);
        let rows = store.query("db.e", &query).unwrap();

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].get_date("ts"), 3000);
        assert_eq!(rows[1].get_date("ts"), 2000);
        assert!(!rows[0].has_field("extra"), "projection dropped it");

        // The default query matches everything.
        let all = store.query("db.e", &Query::default()).unwrap();
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn updates_replace_the_matching_row_only() {
        let store = InMemoryStore::new();
        store.create_collection("db.c", 0, false, 0).unwrap();
        for pid in [1, 2] {
            let mut doc = BsonObj::new();
            doc.append("pid", Bson::Int(pid));
            doc.append("aid", Bson::Int(0));
            store.insert("db.c", &doc).unwrap();
        }
        let mut filter = BsonObj::new();
        filter.append("pid", Bson::Int(2));
        let mut updated = BsonObj::new();
        updated.append("pid", Bson::Int(2));
        updated.append("aid", Bson::Int(7));
        assert!(store.update("db.c", &filter, &updated).is_ok());

        let docs = store.documents("db.c");
        assert_eq!(docs[0].get_int("aid"), 0);
        assert_eq!(docs[1].get_int("aid"), 7);

        // A filter that matches nothing is the reference's
        // DB_TASK_GENERAL_ERROR case.
        let mut missing = BsonObj::new();
        missing.append("pid", Bson::Int(99));
        assert!(store.update("db.c", &missing, &BsonObj::new()).is_err());
        assert!(!store.last_error().is_empty());
    }

    #[test]
    fn capped_collections_drop_their_oldest_rows() {
        let store = InMemoryStore::new();
        assert!(store.create_collection("db.starts", 5000, true, 2).unwrap());
        for i in 0..4i64 {
            let mut doc = BsonObj::new();
            doc.append("time", Bson::Date(i));
            store.insert("db.starts", &doc).unwrap();
        }
        let docs = store.documents("db.starts");
        assert_eq!(docs.len(), 2);
        assert_eq!(docs[0].get_date("time"), 2);
        assert_eq!(docs[1].get_date("time"), 3);
    }

    #[test]
    fn a_dropped_connection_fails_writes_and_reports_it() {
        let store = InMemoryStore::new();
        store.create_collection("db.c", 0, false, 0).unwrap();
        store.drop_connection();
        assert!(!store.is_connected());
        assert!(matches!(
            store.insert("db.c", &BsonObj::new()),
            Err(MongoError::Connection(_))
        ));
        assert!(matches!(
            store.create_collection("db.d", 0, false, 0),
            Err(MongoError::Connection(_))
        ));
        assert!(matches!(
            store.query("db.c", &Query::new()),
            Err(MongoError::Connection(_))
        ));
        assert!(matches!(
            store.update("db.c", &BsonObj::new(), &BsonObj::new()),
            Err(MongoError::Connection(_))
        ));
        assert!(!store.last_error().is_empty());
        assert!(store.collection_names("db").is_err());
        assert!(!store.drop_collection("db.c"));
        assert!(!store.drop_database("db"));
        let mut command = BsonObj::new();
        command.append("ismaster", Bson::Int(1));
        assert!(store.run_command("admin", &command).is_err());

        store.restore_connection();
        assert!(store.insert("db.c", &BsonObj::new()).is_ok());
        assert_eq!(store.last_error(), "");
    }

    #[test]
    fn connect_and_auth_reach_a_fresh_store() {
        let store = InMemoryStore::new();
        store.drop_connection();
        assert!(store.connect("localhost", 27017));
        assert!(store.auth("admin", "bw", "secret"));
        assert!(store.is_connected());
    }

    #[test]
    fn collection_names_skip_internal_collections() {
        let store = InMemoryStore::new();
        store.create_collection("db.keep", 0, false, 0).unwrap();
        store.create_collection("db.$cmd", 0, false, 0).unwrap();
        store.create_collection("other.keep", 0, false, 0).unwrap();
        assert_eq!(store.collection_names("db").unwrap(), ["db.keep"]);
    }

    #[test]
    fn admin_commands_answer_the_way_the_backend_reads_them() {
        let store = InMemoryStore::new();
        let mut ismaster = BsonObj::new();
        ismaster.append("ismaster", Bson::Int(1));
        assert_eq!(
            store.run_command("admin", &ismaster).unwrap(),
            CommandReply::IsMaster {
                msg: "standalone".into()
            }
        );

        let clustered = InMemoryStore::new_clustered();
        assert_eq!(
            clustered.run_command("admin", &ismaster).unwrap(),
            CommandReply::IsMaster {
                msg: "isdbgrid".into()
            }
        );

        store
            .create_collection("bw_ml_user_foo#.keep", 0, false, 0)
            .unwrap();
        let mut list = BsonObj::new();
        list.append("listDatabases", Bson::Int(1));
        assert_eq!(
            store.run_command("admin", &list).unwrap(),
            CommandReply::ListDatabases(vec!["bw_ml_user_foo#".into()])
        );

        let mut shard = BsonObj::new();
        shard.append("enablesharding", Bson::Str("bw_ml_user_foo#".into()));
        assert_eq!(
            store.run_command("admin", &shard).unwrap(),
            CommandReply::Ack
        );

        let mut unknown = BsonObj::new();
        unknown.append("noSuchCommand", Bson::Int(1));
        assert_eq!(
            store.run_command("admin", &unknown),
            Err(MongoError::General("no such command".into()))
        );
    }

    #[test]
    fn dropping_a_collection_or_database_removes_it() {
        let store = InMemoryStore::new();
        store.create_collection("db.gone", 0, false, 0).unwrap();
        store.insert("db.gone", &BsonObj::new()).unwrap();
        assert!(store.drop_collection("db.gone"));
        assert!(store.documents("db.gone").is_empty());
        assert!(!store.drop_collection("db.gone"), "already gone");

        store.create_collection("db.other", 0, false, 0).unwrap();
        assert!(store.drop_database("db"));
        assert!(store.collection_names("db").unwrap().is_empty());
        assert!(!store.drop_database("db"), "already gone");
    }

    #[test]
    fn injected_create_failures_report_through_the_seam() {
        let store = InMemoryStore::new();
        assert!(store.create_collection("db.a", 0, false, 0).unwrap());
        store.set_fail_next_creates(2);
        assert_eq!(store.create_collection("db.b", 0, false, 0), Ok(false));
        assert_eq!(store.create_collection("db.c", 0, false, 0), Ok(false));
        assert!(!store.last_error().is_empty());
        // The budget is spent; the next create succeeds again. Only the
        // two successful creates left collections behind.
        assert!(store.create_collection("db.d", 0, false, 0).unwrap());
        assert_eq!(store.collection_names("db").unwrap().len(), 2);
    }
}
