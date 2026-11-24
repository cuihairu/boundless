//! The shared ("common" database) collection classes:
//! `mongodb/hostnames.{hpp,cpp}` + `hostnames_validator.{hpp,cpp}`,
//! `mongodb/categories.{hpp,cpp}` + `categories.{hpp,cpp}`,
//! `mongodb/format_strings.{hpp,cpp}` + `format_strings.{hpp,cpp}`, and
//! `mongodb/log_component_names.{hpp,cpp}` + `log_component_names.{hpp,cpp}`.
//!
//! Each class keeps its ID maps in memory and queues a
//! [`Task::WriteIDStringPair`] / [`Task::UpdateIDStringPair`] for every
//! write — the reference's tasks, so a connection loss re-queues at `HIGH`
//! and a refused write aborts all further processing.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::interpolator::LogStringInterpolator;

use crate::mongodb::constants;
use crate::mongodb::driver::MongoConnection;
use crate::mongodb::tasks::{Task, TaskManager};
use crate::mongodb::types::{HostLookup, HostResolver, ip_to_string};

/// `GetHostResult` (`hostnames.hpp:23-27`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GetHostResult {
    /// `BW_GET_HOST_ERROR`.
    Error,
    /// `BW_GET_HOST_FOUND` — answered from the map.
    Found,
    /// `BW_GET_HOST_ADDED` — resolved or degraded now, and written to db.
    Added,
}

/// `HostnamesValidatorProcessStatus` (`hostnames_validator.hpp:21-25`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ValidatorStatus {
    /// `BW_VALIDATE_HOSTNAMES_FAILED`.
    Failed,
    /// `BW_VALIDATE_HOSTNAMES_CONTINUE`.
    Continue,
    /// `BW_VALIDATE_HOSTNAMES_FINISHED`.
    Finished,
}

/// `HOSTNAMES_VALIDATOR_MAX_LOOKUP_RETRIES`
/// (`hostnames_validator.cpp:23`).
const MAX_LOOKUP_RETRIES: u32 = 10;

/// `Hostnames` + `HostnamesValidator` + `HostnamesMongoDB`
/// (`hostnames.hpp:36-67`, `hostnames_validator.hpp:47-70`,
/// `mongodb/hostnames.hpp:25-45`). One type: the validator state the base
/// class carries is private to this map anyway.
pub struct HostnamesMongoDB<C: MongoConnection + 'static> {
    task_mgr: TaskManager,
    connection: Arc<C>,
    namespace: String,
    resolver: Arc<dyn HostResolver>,
    /// `hostnames_`: ip → hostname.
    hostnames: BTreeMap<u32, String>,
    /// `validationList_`: ip → (hostname, validated).
    validation_list: BTreeMap<u32, (String, bool)>,
    /// `lookupRetries_`.
    lookup_retries: u32,
}

impl<C: MongoConnection + 'static> HostnamesMongoDB<C> {
    /// The reference constructor (`mongodb/hostnames.cpp:25-32`).
    pub fn new(
        task_mgr: TaskManager,
        connection: Arc<C>,
        namespace: String,
        resolver: Arc<dyn HostResolver>,
    ) -> Self {
        Self {
            task_mgr,
            connection,
            namespace,
            resolver,
            hostnames: BTreeMap::new(),
            validation_list: BTreeMap::new(),
            lookup_retries: 0,
        }
    }

    /// `HostnamesMongoDB::init` (`mongodb/hostnames.cpp:68-101`): the full
    /// scan into the map.
    pub fn init(&mut self) -> bool {
        let rows = match self.connection.query(&self.namespace, &Default::default()) {
            Ok(rows) => rows,
            Err(error) => {
                let _ = error;
                return false;
            }
        };

        for record in rows {
            let ip = record.get_int(constants::HOST_NAMES_KEY_IP) as u32;
            let hostname = record.get_string(constants::HOST_NAMES_KEY_HOSTNAME);
            self.add_hostname_to_map(ip, hostname);
        }

        true
    }

    /// `Hostnames::getHostByAddr` (`hostnames.cpp:41-101`): the map first,
    /// then the resolver — a miss degrades to the dotted quad — then the
    /// write task and the map entry.
    pub fn get_host_by_addr(&mut self, addr: u32) -> (String, GetHostResult) {
        if let Some(hostname) = self.hostnames.get(&addr) {
            return (hostname.clone(), GetHostResult::Found);
        }

        // Unable to resolve the hostname, store the IP address as a string.
        let hostname = match self.resolver.lookup(addr) {
            HostLookup::Resolved(name) => {
                // Truncate at the first dot, as the reference does on
                // `h_name`.
                match name.find('.') {
                    Some(at) => name[..at].to_string(),
                    None => name,
                }
            }
            _ => ip_to_string(addr),
        };

        if !self.write_hostname_to_db(addr, &hostname) {
            return (String::new(), GetHostResult::Error);
        }

        self.hostnames.insert(addr, hostname.clone());
        (hostname, GetHostResult::Added)
    }

    /// `Hostnames::updateHostname` (`hostnames.hpp:49-54`): only an
    /// existing entry is replaced.
    pub fn update_hostname(&mut self, addr: u32, hostname: &str) {
        if let Some(entry) = self.hostnames.get_mut(&addr) {
            *entry = hostname.to_string();
        }
    }

    /// `HostnamesMongoDB::writeHostnameToDB`
    /// (`mongodb/hostnames.cpp:36-53`): queue the insert; success is not
    /// known until the task completes.
    pub fn write_hostname_to_db(&self, addr: u32, hostname: &str) -> bool {
        // `IPAddress` is a `uint32`; the driver appends it as an `int32`,
        // so the cast wraps faithfully.
        self.task_mgr.add_background_task(Task::WriteIDStringPair {
            collection_name: self.namespace.clone(),
            id_key: constants::HOST_NAMES_KEY_IP,
            id: addr as i32,
            string_key: constants::HOST_NAMES_KEY_HOSTNAME,
            string_value: hostname.to_string(),
        });
        true
    }

    /// `HostnamesMongoDB::validateNextHostname`
    /// (`mongodb/hostnames.cpp:112-129` + the inherited
    /// `HostnamesValidator::validateNextHostname`,
    /// `hostnames_validator.cpp:59-151`): seed the list from the map when a
    /// round is not in progress, then work the first unvalidated entry.
    pub fn validate_next_hostname(&mut self) -> ValidatorStatus {
        if !self.in_progress() {
            // HostnameCopier: every known pair, unvalidated.
            let pairs: Vec<(u32, String)> = self
                .hostnames
                .iter()
                .map(|(addr, hostname)| (*addr, hostname.clone()))
                .collect();
            for (addr, hostname) in pairs {
                self.add_validate_hostname(addr, hostname, false);
            }
        }

        let result = self.validate_next_entry();

        if result != ValidatorStatus::Continue {
            self.clear();
        }

        result
    }

    /// `HostnamesValidator::inProgress`.
    fn in_progress(&self) -> bool {
        !self.validation_list.is_empty()
    }

    /// `HostnamesValidator::clear`.
    fn clear(&mut self) {
        self.validation_list.clear();
        self.lookup_retries = 0;
    }

    /// `HostnamesValidator::addValidateHostname`.
    fn add_validate_hostname(&mut self, addr: u32, hostname: String, validated: bool) {
        self.validation_list.insert(addr, (hostname, validated));
    }

    /// The body of `HostnamesValidator::validateNextHostname`
    /// (`hostnames_validator.cpp:63-151`): first `NOT_VALIDATED` entry in
    /// map order, the retry ladder, then the success bookkeeping.
    fn validate_next_entry(&mut self) -> ValidatorStatus {
        let Some((&addr, _)) = self
            .validation_list
            .iter()
            .find(|(_, (_, validated))| !*validated)
        else {
            return ValidatorStatus::Finished;
        };

        let (old_hostname, _) = self.validation_list[&addr].clone();
        let ip_string = ip_to_string(addr);

        let outcome = self.validate_one_hostname(addr, &old_hostname);

        match outcome {
            HostLookup::TryAgain => {
                if self.lookup_retries < MAX_LOOKUP_RETRIES {
                    // This will attempt the same address again in the next
                    // call.
                    self.lookup_retries += 1;
                    return ValidatorStatus::Continue;
                }

                self.lookup_retries = 0;
                return ValidatorStatus::Failed;
            }
            HostLookup::Failed => {
                self.lookup_retries = 0;
                return ValidatorStatus::Failed;
            }
            _ => {}
        }

        // Validation succeeded; reset in case of reuse.
        self.lookup_retries = 0;
        self.validation_list
            .insert(addr, (old_hostname.clone(), true));

        match outcome {
            HostLookup::Resolved(new_hostname) if new_hostname != old_hostname => {
                self.validation_list
                    .insert(addr, (new_hostname.clone(), true));
                self.hostname_changed(addr, &new_hostname);
            }
            _ => {}
        }

        let _ = ip_string;
        ValidatorStatus::Continue
    }

    /// `HostnamesValidator::validateOneHostname`
    /// (`hostnames_validator.cpp:160-256`): the resolver's answer reduced
    /// to the resolved name (first dot truncated), the degrade-to-ip on
    /// `HOST_NOT_FOUND` when the name differs, and the retry / failure
    /// signalling.
    fn validate_one_hostname(&self, addr: u32, old_name: &str) -> HostLookup {
        match self.resolver.lookup(addr) {
            HostLookup::Resolved(mut name) => {
                if let Some(at) = name.find('.') {
                    name.truncate(at);
                }

                if name != old_name {
                    HostLookup::Resolved(name)
                } else {
                    // The reference signals NOT_CHANGED by handing back the
                    // old name; the caller compares and skips the write.
                    HostLookup::Resolved(old_name.to_string())
                }
            }
            HostLookup::HostNotFound => {
                // Only need to warn/alter if the hostname was not already
                // set to the IP address.
                let ip_string = ip_to_string(addr);
                if ip_string != old_name {
                    HostLookup::Resolved(ip_string)
                } else {
                    HostLookup::Resolved(old_name.to_string())
                }
            }
            other => other,
        }
    }

    /// `HostnamesMongoDB::hostnameChanged` (`mongodb/hostnames.cpp:131-154`).
    fn hostname_changed(&mut self, addr: u32, new_hostname: &str) {
        self.update_hostname(addr, new_hostname);

        self.task_mgr.add_background_task(Task::UpdateIDStringPair {
            collection_name: self.namespace.clone(),
            id_key: constants::HOST_NAMES_KEY_IP,
            id: addr as i32,
            string_key: constants::HOST_NAMES_KEY_HOSTNAME,
            string_value: new_hostname.to_string(),
        });
    }

    /// The map, for callers that seed a user-visible hostname list.
    pub fn hostnames(&self) -> &BTreeMap<u32, String> {
        &self.hostnames
    }

    /// `Hostnames::addHostnameToMap`.
    fn add_hostname_to_map(&mut self, addr: u32, hostname: String) {
        self.hostnames.insert(addr, hostname);
    }
}

/// `Categories` + `CategoriesMongoDB` (`categories.hpp:31-56`,
/// `mongodb/categories.hpp:24-40`). IDs are one-based, allocated
/// client-side; the next id is simply `++maxCategoryID_`.
pub struct CategoriesMongoDB<C: MongoConnection + 'static> {
    task_mgr: TaskManager,
    connection: Arc<C>,
    namespace: String,
    /// `idMap_`.
    id_map: BTreeMap<u16, String>,
    /// `nameMap_`.
    name_map: BTreeMap<String, u16>,
    /// `maxCategoryID_`.
    max_category_id: u16,
}

impl<C: MongoConnection + 'static> CategoriesMongoDB<C> {
    /// `Categories::Categories` — `maxCategoryID_` starts at zero, so the
    /// first allocated id is one.
    pub fn new(task_mgr: TaskManager, connection: Arc<C>, namespace: String) -> Self {
        Self {
            task_mgr,
            connection,
            namespace,
            id_map: BTreeMap::new(),
            name_map: BTreeMap::new(),
            max_category_id: 0,
        }
    }

    /// `CategoriesMongoDB::init` (`mongodb/categories.cpp:35-67`): the full
    /// scan into both maps.
    pub fn init(&mut self) -> bool {
        let rows = match self.connection.query(&self.namespace, &Default::default()) {
            Ok(rows) => rows,
            Err(_) => return false,
        };

        for record in rows {
            let id = record.get_int(constants::CATEGORIES_KEY_ID) as u16;
            let name = record.get_string(constants::CATEGORIES_KEY_NAME);
            self.add_category_to_map(id, name);
        }

        true
    }

    /// `Categories::resolveOrCreateNameToID` (`categories.cpp:31-58`).
    pub fn resolve_or_create_name_to_id(&mut self, category_name: &str) -> u16 {
        if let Some(&id) = self.name_map.get(category_name) {
            return id;
        }

        let new_category_id = self.next_available_category_id();

        self.id_map
            .insert(new_category_id, category_name.to_string());
        self.name_map
            .insert(category_name.to_string(), new_category_id);

        self.write_category_to_db(new_category_id, category_name);

        new_category_id
    }

    /// `Categories::resolveNameToID`.
    pub fn resolve_name_to_id(&self, category_name: &str) -> Option<u16> {
        self.name_map.get(category_name).copied()
    }

    /// `Categories::resolveIDToName`.
    pub fn resolve_id_to_name(&self, category_id: u16) -> Option<&str> {
        self.id_map.get(&category_id).map(String::as_str)
    }

    /// `Categories::getNextAvailableCategoryID`.
    fn next_available_category_id(&mut self) -> u16 {
        self.max_category_id = self.max_category_id.wrapping_add(1);
        self.max_category_id
    }

    /// `Categories::addCategoryToMap`.
    fn add_category_to_map(&mut self, id: u16, name: String) {
        self.id_map.insert(id, name.clone());
        self.name_map.insert(name, id);
        if id > self.max_category_id {
            self.max_category_id = id;
        }
    }

    /// `CategoriesMongoDB::writeCategoryToDB`
    /// (`mongodb/categories.cpp:22-33`).
    fn write_category_to_db(&self, new_category_id: u16, category_name: &str) {
        self.task_mgr.add_background_task(Task::WriteIDStringPair {
            collection_name: self.namespace.clone(),
            id_key: constants::CATEGORIES_KEY_ID,
            id: new_category_id as i32,
            string_key: constants::CATEGORIES_KEY_NAME,
            string_value: category_name.to_string(),
        });
    }
}

/// `FormatStrings` + `FormatStringsMongoDB` (`format_strings.hpp:26-52`,
/// `mongodb/format_strings.hpp:23-44`). The reference stores raw
/// `LogStringInterpolator*` pointers in its map; the port stores owned
/// clones — the interpolator is `Clone`, which is what makes the raw
/// pointer unnecessary.
pub struct FormatStringsMongoDB<C: MongoConnection + 'static> {
    task_mgr: TaskManager,
    connection: Arc<C>,
    namespace: String,
    /// `formatMap_`: format string → handler.
    format_map: BTreeMap<String, LogStringInterpolator>,
    /// `fmtStrIDMap_`.
    fmt_str_id_map: BTreeMap<String, u32>,
    /// `currMaxID_`.
    curr_max_id: u32,
}

impl<C: MongoConnection + 'static> FormatStringsMongoDB<C> {
    /// The reference constructor (`mongodb/format_strings.cpp:25-34`).
    pub fn new(task_mgr: TaskManager, connection: Arc<C>, namespace: String) -> Self {
        Self {
            task_mgr,
            connection,
            namespace,
            format_map: BTreeMap::new(),
            fmt_str_id_map: BTreeMap::new(),
            curr_max_id: 0,
        }
    }

    /// `FormatStringsMongoDB::init` (`mongodb/format_strings.cpp:55-107`).
    /// An invalid format string is skipped WITHOUT advancing `currMaxID_`,
    /// so its id is leaked forever — the reference's behaviour, kept.
    pub fn init(&mut self) -> bool {
        let rows = match self.connection.query(&self.namespace, &Default::default()) {
            Ok(rows) => rows,
            Err(_) => return false,
        };

        for record in rows {
            let id = record.get_int(constants::FMT_STRS_KEY_ID) as u32;
            let fmt_str = record.get_string(constants::FMT_STRS_KEY_FMT_STR);

            let handler = LogStringInterpolator::new(&fmt_str);
            if !handler.is_ok() {
                // Skipped invalid format string.
                continue;
            }

            self.add_format_string_to_map(fmt_str.clone(), handler);
            self.fmt_str_id_map.insert(fmt_str, id);
            self.curr_max_id = self.curr_max_id.max(id);
        }

        true
    }

    /// `FormatStrings::resolve` (`format_strings.cpp:30-64`): the map hit,
    /// then a fresh interpolator — an invalid string yields `None` and
    /// nothing is queued.
    pub fn resolve(&mut self, fmt: &str) -> Option<LogStringInterpolator> {
        if let Some(handler) = self.format_map.get(fmt) {
            return Some(handler.clone());
        }

        let handler = LogStringInterpolator::new(fmt);
        if !handler.is_ok() {
            // Failed to create LogStringInterpolator. This might be because
            // the format string is invalid.
            return None;
        }

        self.write_format_string_to_db(&handler);
        self.format_map.insert(fmt.to_string(), handler.clone());

        Some(handler)
    }

    /// `FormatStringsMongoDB::getIdOfFmtString`
    /// (`mongodb/format_strings.cpp:115-126`): zero on a miss.
    pub fn get_id_of_fmt_string(&self, fmt_string: &str) -> u32 {
        self.fmt_str_id_map.get(fmt_string).copied().unwrap_or(0)
    }

    /// `FormatStringsMongoDB::writeFormatStringToDB`
    /// (`mongodb/format_strings.cpp:40-52`): allocate the next id, map the
    /// string to it, queue the insert.
    fn write_format_string_to_db(&mut self, handler: &LogStringInterpolator) {
        let id = self.next_available_id();
        let fmt_string = handler.format_string().to_string();

        self.fmt_str_id_map.insert(fmt_string.clone(), id);

        self.task_mgr.add_background_task(Task::WriteIDStringPair {
            collection_name: self.namespace.clone(),
            id_key: constants::FMT_STRS_KEY_ID,
            id: id as i32,
            string_key: constants::FMT_STRS_KEY_FMT_STR,
            string_value: fmt_string,
        });
    }

    /// `FormatStringsMongoDB::getNextAvailableID`.
    fn next_available_id(&mut self) -> u32 {
        self.curr_max_id += 1;
        self.curr_max_id
    }

    /// `FormatStrings::addFormatStringToMap`.
    fn add_format_string_to_map(&mut self, fmt: String, handler: LogStringInterpolator) {
        self.format_map.insert(fmt, handler);
    }
}

/// `LogComponentNames` + `LogComponentNamesMongoDB`
/// (`log_component_names.hpp:31-45`, `mongodb/log_component_names.hpp`).
pub struct LogComponentNamesMongoDB<C: MongoConnection + 'static> {
    task_mgr: TaskManager,
    connection: Arc<C>,
    namespace: String,
    /// `componentNames_`: the id of a name is its index.
    component_names: Vec<String>,
    /// `componentIDMap_`.
    component_id_map: BTreeMap<String, u32>,
    /// `currMaxID_`.
    curr_max_id: u32,
}

/// `LogComponentNames::MAX_COMPONENTS`
/// (`log_component_names.hpp:47`).
pub const MAX_COMPONENTS: usize = 31;

impl<C: MongoConnection + 'static> LogComponentNamesMongoDB<C> {
    /// The reference constructor (`mongodb/log_component_names.cpp:24-33`).
    pub fn new(task_mgr: TaskManager, connection: Arc<C>, namespace: String) -> Self {
        Self {
            task_mgr,
            connection,
            namespace,
            component_names: Vec::new(),
            component_id_map: BTreeMap::new(),
            curr_max_id: 0,
        }
    }

    /// `LogComponentNamesMongoDB::init`
    /// (`mongodb/log_component_names.cpp:63-107`).
    pub fn init(&mut self) -> bool {
        let rows = match self.connection.query(&self.namespace, &Default::default()) {
            Ok(rows) => rows,
            Err(_) => return false,
        };

        for record in rows {
            let id = record.get_int(constants::COMPONENTS_KEY_ID) as u32;
            let name = record.get_string(constants::COMPONENTS_KEY_NAME);

            self.component_names.push(name.clone());
            self.component_id_map.insert(name, id);
            self.curr_max_id = self.curr_max_id.max(id);
        }

        true
    }

    /// `LogComponentNames::getAppTypeIDFromName`
    /// (`log_component_names.cpp:28-60`): the id is the position in the
    /// list; an unknown name is appended and written to db. The reference
    /// only `CRITICAL_MSG`s past `MAX_COMPONENTS` and keeps going — the
    /// port does the same.
    pub fn get_app_type_id_from_name(&mut self, component_name: &str) -> u32 {
        let id = self.component_names.len() as u32;

        if let Some(found) = self
            .component_names
            .iter()
            .position(|name| name == component_name)
        {
            return found as u32;
        }

        if id as usize >= MAX_COMPONENTS {
            // You have registered more components than is supported.
        }

        self.component_names.push(component_name.to_string());
        self.write_component_name_to_db(component_name);

        id
    }

    /// `LogComponentNamesMongoDB::getIDOfComponentName`
    /// (`mongodb/log_component_names.cpp:114-131`): the registration is
    /// done for the side effect, then the map answers (zero on a miss).
    pub fn get_id_of_component_name(&mut self, component_name: &str) -> u32 {
        // This is just to make sure that this component name will be
        // stored into database.
        self.get_app_type_id_from_name(component_name);

        self.component_id_map
            .get(component_name)
            .copied()
            .unwrap_or(0)
    }

    /// `LogComponentNamesMongoDB::writeComponentNameToDB`
    /// (`mongodb/log_component_names.cpp:39-56`).
    fn write_component_name_to_db(&mut self, component_name: &str) {
        let id = self.next_available_id();

        self.component_id_map.insert(component_name.to_string(), id);

        self.task_mgr.add_background_task(Task::WriteIDStringPair {
            collection_name: self.namespace.clone(),
            id_key: constants::COMPONENTS_KEY_ID,
            id: id as i32,
            string_key: constants::COMPONENTS_KEY_NAME,
            string_value: component_name.to_string(),
        });
    }

    /// `LogComponentNamesMongoDB::getNextAvailableID`.
    fn next_available_id(&mut self) -> u32 {
        self.curr_max_id += 1;
        self.curr_max_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mongodb::bson::{Bson, BsonObj};
    use crate::mongodb::driver::InMemoryStore;
    use crate::mongodb::types::NoHostResolver;
    use std::sync::Arc;

    const NS: &str = "bw_ml_common#1.test";

    fn store() -> Arc<InMemoryStore> {
        Arc::new(InMemoryStore::new())
    }

    /// A resolver that answers a fixed name for one address and the
    /// reference's `TRY_AGAIN` for another.
    struct FixedResolver {
        answers: BTreeMap<u32, HostLookup>,
    }

    impl HostResolver for FixedResolver {
        fn lookup(&self, ip: u32) -> HostLookup {
            self.answers
                .get(&ip)
                .cloned()
                .unwrap_or(HostLookup::HostNotFound)
        }
    }

    #[test]
    fn hostname_lookup_caches_and_degrades_to_the_ip() {
        let store = store();
        store.create_collection(NS, 0, false, 0).unwrap();
        let mut hosts = HostnamesMongoDB::new(
            TaskManager::new(),
            Arc::clone(&store),
            NS.to_string(),
            Arc::new(NoHostResolver),
        );

        // A miss resolves to the dotted quad and queues the write.
        let (name, result) = hosts.get_host_by_addr(0x7f00_0001);
        assert_eq!(result, GetHostResult::Added);
        assert_eq!(name, "127.0.0.1");

        // The second look is a cache hit.
        let (name, result) = hosts.get_host_by_addr(0x7f00_0001);
        assert_eq!(result, GetHostResult::Found);
        assert_eq!(name, "127.0.0.1");

        // init reads back what the task wrote.
        let mut pair = BsonObj::new();
        pair.append(
            constants::HOST_NAMES_KEY_IP,
            Bson::Int(0x7f00_0001u32 as i32),
        );
        pair.append(
            constants::HOST_NAMES_KEY_HOSTNAME,
            Bson::Str("127.0.0.1".into()),
        );
        store.insert(NS, &pair).unwrap();

        let mut fresh = HostnamesMongoDB::new(
            TaskManager::new(),
            Arc::clone(&store),
            NS.to_string(),
            Arc::new(NoHostResolver),
        );
        assert!(fresh.init());
        let (name, result) = fresh.get_host_by_addr(0x7f00_0001);
        assert_eq!(result, GetHostResult::Found);
        assert_eq!(name, "127.0.0.1");
    }

    #[test]
    fn a_resolved_hostname_is_truncated_at_the_first_dot() {
        let store = store();
        let resolver = FixedResolver {
            answers: BTreeMap::from([(0x0808_0808, HostLookup::Resolved("gw.example.org".into()))]),
        };
        let mut hosts = HostnamesMongoDB::new(
            TaskManager::new(),
            Arc::clone(&store),
            NS.to_string(),
            Arc::new(resolver),
        );

        let (name, result) = hosts.get_host_by_addr(0x0808_0808);
        assert_eq!(result, GetHostResult::Added);
        assert_eq!(name, "gw");
    }

    #[test]
    fn validation_resolves_changes_and_persists_them() {
        let store = store();
        store.create_collection(NS, 0, false, 0).unwrap();
        // The row the update task will eventually replace.
        let mut record = BsonObj::new();
        record.append(
            constants::HOST_NAMES_KEY_IP,
            Bson::Int(0x0a00_0001u32 as i32),
        );
        record.append(constants::HOST_NAMES_KEY_HOSTNAME, Bson::Str("old1".into()));
        store.insert(NS, &record).unwrap();

        let resolver = FixedResolver {
            answers: BTreeMap::from([
                // Truncation at the first dot leaves a NEW name: changed.
                (
                    0x0a00_0001,
                    HostLookup::Resolved("cella1.bigworld.com".into()),
                ),
                (0x0a00_0002, HostLookup::Resolved("cella2".into())),
                (0x0a00_0003, HostLookup::TryAgain),
                (0x0a00_0004, HostLookup::Failed),
            ]),
        };
        let mut hosts = HostnamesMongoDB::new(
            TaskManager::new(),
            Arc::clone(&store),
            NS.to_string(),
            Arc::new(resolver),
        );
        hosts.add_hostname_to_map(0x0a00_0001, "old1".into());
        hosts.add_hostname_to_map(0x0a00_0002, "cella2".into());
        hosts.add_hostname_to_map(0x0a00_0003, "cella3".into());
        hosts.add_hostname_to_map(0x0a00_0004, "cella4".into());

        // 1: `cella1.bigworld.com` truncates to `cella1`, a new name — the
        // map moves and an update task is queued.
        assert_eq!(hosts.validate_next_hostname(), ValidatorStatus::Continue);
        assert_eq!(hosts.hostnames()[&0x0a00_0001], "cella1");
        // 2: identical name, no change.
        assert_eq!(hosts.validate_next_hostname(), ValidatorStatus::Continue);
        // 3: retries until the ladder is exhausted — the same entry is
        // retried `MAX_LOOKUP_RETRIES` times before the round fails.
        for _ in 0..MAX_LOOKUP_RETRIES {
            assert_eq!(hosts.validate_next_hostname(), ValidatorStatus::Continue);
        }
        assert_eq!(hosts.validate_next_hostname(), ValidatorStatus::Failed);

        // FAILED clears the round; the changed hostname is still in the map.
        assert_eq!(hosts.hostnames()[&0x0a00_0001], "cella1");
        assert_eq!(hosts.hostnames()[&0x0a00_0002], "cella2");
        assert_eq!(hosts.hostnames()[&0x0a00_0003], "cella3");

        // A fresh round re-seeds everything: entry 1 now matches its own
        // truncated name, entry 2 is unchanged, then the retry ladder
        // again.
        assert_eq!(hosts.validate_next_hostname(), ValidatorStatus::Continue);
        assert_eq!(hosts.validate_next_hostname(), ValidatorStatus::Continue);
    }

    #[test]
    fn host_not_found_degrades_to_the_ip_when_the_name_differs() {
        let store = store();
        let mut hosts = HostnamesMongoDB::new(
            TaskManager::new(),
            Arc::clone(&store),
            NS.to_string(),
            Arc::new(NoHostResolver),
        );
        hosts.add_hostname_to_map(0x0a00_0009, "machined-name".into());

        // One round: the name degrades to the dotted quad, CHANGED.
        assert_eq!(hosts.validate_next_hostname(), ValidatorStatus::Continue);
        assert_eq!(hosts.hostnames()[&0x0a00_0009], "10.0.0.9");

        // The validated entry is still in the round's list, so the next
        // call finds nothing to do: Finished — and the round clears.
        assert_eq!(hosts.validate_next_hostname(), ValidatorStatus::Finished);

        // Clearing re-seeds from the map on the following call: already
        // the ip, NOT_CHANGED, validated again.
        assert_eq!(hosts.validate_next_hostname(), ValidatorStatus::Continue);
        assert_eq!(hosts.validate_next_hostname(), ValidatorStatus::Finished);
    }

    #[test]
    fn categories_allocate_one_based_ids_and_answer_hits() {
        let store = store();
        let mut categories =
            CategoriesMongoDB::new(TaskManager::new(), Arc::clone(&store), NS.to_string());

        assert_eq!(categories.resolve_or_create_name_to_id("CellApp"), 1);
        assert_eq!(categories.resolve_or_create_name_to_id("Movement"), 2);
        assert_eq!(categories.resolve_or_create_name_to_id("CellApp"), 1);

        assert_eq!(categories.resolve_name_to_id("Movement"), Some(2));
        assert_eq!(categories.resolve_name_to_id("Nope"), None);
        assert_eq!(categories.resolve_id_to_name(1), Some("CellApp"));
        assert_eq!(categories.resolve_id_to_name(9), None);
    }

    #[test]
    fn format_strings_resolve_cache_and_skip_invalid() {
        let store = store();
        let mut formats =
            FormatStringsMongoDB::new(TaskManager::new(), Arc::clone(&store), NS.to_string());

        let first = formats.resolve("Hello %d").expect("valid");
        assert_eq!(first.format_string(), "Hello %d");
        let again = formats.resolve("Hello %d").expect("cached");
        assert_eq!(again.format_string(), "Hello %d");

        // An invalid format string is None and queues nothing.
        assert!(formats.resolve("%y").is_none());
    }

    #[test]
    fn format_string_init_maps_ids_and_leaks_invalid_ones() {
        let store = store();
        store.create_collection(NS, 0, false, 0).unwrap();
        for (id, text) in [(1i32, "one %d"), (3, "three %s"), (2, "%q invalid")] {
            let mut record = BsonObj::new();
            record.append(constants::FMT_STRS_KEY_ID, Bson::Int(id));
            record.append(constants::FMT_STRS_KEY_FMT_STR, Bson::Str(text.into()));
            store.insert(NS, &record).unwrap();
        }

        let mut formats =
            FormatStringsMongoDB::new(TaskManager::new(), Arc::clone(&store), NS.to_string());
        assert!(formats.init());

        // Valid strings resolve to their stored ids; the invalid one never
        // entered the map — and its id (2) is leaked: the next allocation
        // is max(valid ids) + 1 = 4.
        assert_eq!(formats.get_id_of_fmt_string("one %d"), 1);
        assert_eq!(formats.get_id_of_fmt_string("three %s"), 3);
        assert_eq!(formats.get_id_of_fmt_string("%q invalid"), 0);

        let handler = formats.resolve("new %d").expect("valid");
        assert_eq!(handler.format_string(), "new %d");
        assert_eq!(formats.get_id_of_fmt_string("new %d"), 4);
    }

    #[test]
    fn component_names_index_by_position_and_persist() {
        let store = store();
        let mut names =
            LogComponentNamesMongoDB::new(TaskManager::new(), Arc::clone(&store), NS.to_string());

        // First sight registers and returns the list index.
        assert_eq!(names.get_id_of_component_name("CellApp"), 1);
        assert_eq!(names.get_id_of_component_name("BaseApp"), 2);
        assert_eq!(names.get_id_of_component_name("CellApp"), 1);
    }

    #[test]
    fn component_name_init_restores_ids_from_the_database() {
        let store = store();
        store.create_collection(NS, 0, false, 0).unwrap();
        for (id, name) in [(2i32, "BaseApp"), (5, "CellApp")] {
            let mut record = BsonObj::new();
            record.append(constants::COMPONENTS_KEY_ID, Bson::Int(id));
            record.append(constants::COMPONENTS_KEY_NAME, Bson::Str(name.into()));
            store.insert(NS, &record).unwrap();
        }

        let mut names =
            LogComponentNamesMongoDB::new(TaskManager::new(), Arc::clone(&store), NS.to_string());
        assert!(names.init());

        assert_eq!(names.get_id_of_component_name("CellApp"), 5);
        assert_eq!(names.get_id_of_component_name("BaseApp"), 2);
        // An unseen name registers itself through the side effect
        // (`getIDOfComponentName` calls `getAppTypeIDFromName` first): it
        // takes the next id past the stored maximum and is persisted.
        assert_eq!(names.get_id_of_component_name("Unknown"), 6);
        assert_eq!(names.get_id_of_component_name("Unknown"), 6);
    }
}
