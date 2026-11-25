//! The database work queue (`mongodb/tasks/*` and the
//! `cstdmf/bgtask_manager.hpp` semantics it leans on).
//!
//! The reference parks every database operation on a background thread that
//! drains a priority queue; each task's prologue checks the abort flag and
//! reconnects before it performs. The port runs that same queue inline: the
//! caller drives [`TaskManager::tick`] (the registered worker deviation),
//! one task per tick, highest priority first.
//!
//! On a refused write (`DB_TASK_GENERAL_ERROR`) the three record tasks
//! abort all further processing, exactly as the reference does; on a lost
//! socket (`DB_TASK_CONNECTION_ERROR`) they re-add themselves at `HIGH` so
//! a reconnected database sees them before any new log batch.

use std::sync::{Arc, Mutex};

use crate::mongodb::bson::{Bson, BsonObj};
use crate::mongodb::connection::ConnectionThreadData;
use crate::mongodb::constants;
use crate::mongodb::driver::{MongoConnection, MongoError};

/// `TaskManager`'s priorities (`bgtask_manager.hpp:278-283`).
pub mod priority {
    /// `TaskManager::MIN`.
    pub const MIN: i32 = 0;
    /// `TaskManager::LOW`.
    pub const LOW: i32 = 32;
    /// `TaskManager::MEDIUM`.
    pub const MEDIUM: i32 = 64;
    /// `TaskManager::DEFAULT`.
    pub const DEFAULT: i32 = MEDIUM;
    /// `TaskManager::HIGH`.
    pub const HIGH: i32 = 96;
    /// `TaskManager::MAX`.
    pub const MAX: i32 = 128;
}

/// One unit of deferred database work. The reference keeps each of these in
/// its own header under `mongodb/tasks/`; the port folds them into one enum
/// because the queue stores them by value either way.
#[derive(Clone, Debug)]
pub enum Task {
    /// `ReconnectTask` (`reconnect_task.hpp`): the perform is empty — its
    /// whole job is the prologue's `reconnectIfNecessary`.
    Reconnect,
    /// `FlushTask` (`flush_task.cpp:29-37`): the constructor consumes the
    /// user's buffer by swapping it out; the perform hands it to the user
    /// log writer.
    Flush {
        uid: u16,
        username: String,
        log_buffer: Vec<BsonObj>,
    },
    /// `AddServerStartTask` (`add_server_start_task.cpp`).
    AddServerStart {
        uid: u16,
        username: String,
        server_start_time: u64,
    },
    /// `RollTask` (`roll_task.cpp`).
    Roll,
    /// `WriteAppIDTask` (`write_appid_task.hpp`).
    WriteAppID {
        uid: u16,
        username: String,
        obj: BsonObj,
    },
    /// `UpdateAppIDTask` (`update_appid_task.hpp`). The uid and username
    /// ride on the record (`COMPONENTS_UID` / `COMPONENTS_USERNAME`), read
    /// at construction like the reference's constructor does; they are
    /// in-memory fields, never written.
    UpdateAppID {
        uid: u16,
        username: String,
        obj: BsonObj,
        appid: i32,
    },
    /// `WriteIDStringPairTask<IDType>` (`write_id_string_pair_task.hpp`).
    /// The reference is generic over the id type; every instantiation the
    /// backend makes goes through the driver as an `int32`, so the port
    /// stores the cast value and each caller casts faithfully.
    WriteIDStringPair {
        collection_name: String,
        id_key: &'static str,
        id: i32,
        string_key: &'static str,
        string_value: String,
    },
    /// `UpdateIDStringPairTask<IDType>` (`update_id_string_pair_task.hpp`).
    UpdateIDStringPair {
        collection_name: String,
        id_key: &'static str,
        id: i32,
        string_key: &'static str,
        string_value: String,
    },
}

/// A task queued at a priority. The list is kept sorted descending by
/// priority, stably — equal priorities run in the order they were queued,
/// as the reference's `TaskManager` does.
#[derive(Clone, Default)]
pub struct TaskManager {
    queue: Arc<Mutex<Vec<(i32, Task)>>>,
}

impl TaskManager {
    /// An empty queue.
    pub fn new() -> Self {
        Self::default()
    }

    /// `addBackgroundTask( task )` — the `DEFAULT` priority.
    pub fn add_background_task(&self, task: Task) {
        self.add_background_task_at(task, priority::DEFAULT);
    }

    /// `addBackgroundTask( task, priority )`.
    pub fn add_background_task_at(&self, task: Task, priority: i32) {
        let mut queue = self.queue.lock().unwrap();
        queue.push((priority, task));
        // Stable sort descending: insertion order survives among equals.
        queue.sort_by_key(|(priority, _)| std::cmp::Reverse(*priority));
    }

    /// `getBackgroundTaskListSize()`.
    pub fn bg_task_list_size(&self) -> usize {
        self.queue.lock().unwrap().len()
    }

    /// The highest-priority task, popped. The list is sorted descending, so
    /// the head is the next one to run.
    fn pop(&self) -> Option<Task> {
        let mut queue = self.queue.lock().unwrap();
        if queue.is_empty() {
            None
        } else {
            Some(queue.remove(0).1)
        }
    }

    /// Drain one task (`MongoDBBackgroundTask::doBackgroundTask`,
    /// `mongodb_background_task.cpp:25-45`, plus the perform of whichever
    /// task came off the queue): the abort flag drops it, the prologue
    /// reconnects, then the task performs.
    pub fn tick<C: MongoConnection + 'static>(&self, data: &mut ConnectionThreadData<C>) {
        let Some(mut task) = self.pop() else {
            return;
        };

        // Do nothing as a previous task has signalled an unrecoverable
        // error. All queued tasks will be aborted until the queue is empty.
        if data.should_abort_further_processing() {
            return;
        }

        data.reconnect_if_necessary();

        self.perform(&mut task, data);
    }

    /// `performBackgroundTask` for whichever variant `task` names.
    fn perform<C: MongoConnection + 'static>(
        &self,
        task: &mut Task,
        data: &mut ConnectionThreadData<C>,
    ) {
        match task {
            Task::Reconnect => {}

            Task::Flush {
                uid,
                username,
                log_buffer,
            } => {
                let Some(writer) = data.get_user_log_writer(*uid, username) else {
                    return;
                };

                match writer.flush(log_buffer) {
                    // A plain failure is the ERROR_MSG the reference logs.
                    Ok(false) | Ok(true) => {}
                    // The reference's flush catches the SocketException and
                    // clears the connected flag itself; the port hands the
                    // error up and does it here.
                    Err(MongoError::Connection(_)) => data.set_connected(false),
                    Err(_) => {}
                }
            }

            Task::AddServerStart {
                uid,
                username,
                server_start_time,
            } => {
                let Some(writer) = data.get_user_log_writer(*uid, username) else {
                    return;
                };

                match writer.on_server_start(*server_start_time) {
                    Ok(_) => {}
                    Err(MongoError::Connection(_)) => data.set_connected(false),
                    Err(_) => {}
                }
            }

            Task::Roll => {
                if !data.roll_user_logs() {
                    // Unable to roll user logs.
                }
            }

            Task::WriteAppID { uid, username, obj } => {
                let Some(writer) = data.get_user_log_writer(*uid, username) else {
                    return;
                };

                match writer.write_component(obj) {
                    Ok(_) => {}
                    Err(MongoError::Connection(_)) => data.set_connected(false),
                    Err(_) => {}
                }
            }

            Task::UpdateAppID {
                uid,
                username,
                obj,
                appid,
            } => {
                let Some(writer) = data.get_user_log_writer(*uid, username) else {
                    return;
                };

                let db_name = writer.db_name().to_string();
                let collection_name = format!("{}.{}", db_name, constants::COMPONENTS_COLL_NAME);

                // Form the structure of the existing record to be modified,
                // then its replacement with the app ID filled in.
                let (existing_record, new_record) = {
                    let mut existing = BsonObj::new();
                    existing.append(
                        constants::COMPONENTS_KEY_HOST,
                        Bson::Int(obj.get_int(constants::COMPONENTS_KEY_HOST)),
                    );
                    existing.append(
                        constants::COMPONENTS_KEY_PID,
                        Bson::Int(obj.get_int(constants::COMPONENTS_KEY_PID)),
                    );
                    existing.append(
                        constants::COMPONENTS_KEY_COMPONENT,
                        Bson::Int(obj.get_int(constants::COMPONENTS_KEY_COMPONENT)),
                    );

                    let mut new_record = existing.clone();
                    new_record.append(constants::COMPONENTS_KEY_APPID, Bson::Int(*appid));

                    (existing, new_record)
                };

                match data.update_record_in_collection(
                    &collection_name,
                    &existing_record,
                    &new_record,
                ) {
                    crate::mongodb::connection::DBTaskStatus::GeneralError => {
                        // Unrecoverable. Stop processing immediately.
                        data.abort_further_processing();
                    }
                    crate::mongodb::connection::DBTaskStatus::ConnectionError => {
                        // If the DB reconnects, ensure this task is
                        // performed before any logs are flushed.
                        self.add_background_task_at(task.clone(), priority::HIGH);
                    }
                    crate::mongodb::connection::DBTaskStatus::Success => {}
                }
            }

            Task::WriteIDStringPair {
                collection_name,
                id_key,
                id,
                string_key,
                string_value,
            } => {
                let mut record = BsonObj::new();
                record.append(id_key, Bson::Int(*id));
                record.append(string_key, Bson::Str(string_value.clone()));

                match data.add_record_to_collection(collection_name, &record) {
                    crate::mongodb::connection::DBTaskStatus::GeneralError => {
                        // Unrecoverable. Stop processing immediately.
                        data.abort_further_processing();
                    }
                    crate::mongodb::connection::DBTaskStatus::ConnectionError => {
                        self.add_background_task_at(task.clone(), priority::HIGH);
                    }
                    crate::mongodb::connection::DBTaskStatus::Success => {}
                }
            }

            Task::UpdateIDStringPair {
                collection_name,
                id_key,
                id,
                string_key,
                string_value,
            } => {
                let mut existing_record = BsonObj::new();
                existing_record.append(id_key, Bson::Int(*id));

                let mut new_record = BsonObj::new();
                new_record.append(id_key, Bson::Int(*id));
                new_record.append(string_key, Bson::Str(string_value.clone()));

                match data.update_record_in_collection(
                    collection_name,
                    &existing_record,
                    &new_record,
                ) {
                    crate::mongodb::connection::DBTaskStatus::GeneralError => {
                        // Unrecoverable. Stop processing immediately.
                        data.abort_further_processing();
                    }
                    crate::mongodb::connection::DBTaskStatus::ConnectionError => {
                        self.add_background_task_at(task.clone(), priority::HIGH);
                    }
                    crate::mongodb::connection::DBTaskStatus::Success => {}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mongodb::driver::InMemoryStore;
    use crate::mongodb::types::WallClock;
    use std::sync::Arc;

    fn data(store: &Arc<InMemoryStore>) -> ConnectionThreadData<InMemoryStore> {
        ConnectionThreadData::new(
            Arc::clone(store),
            crate::mongodb::connection::ConnectionInfo::default(),
            601_200,
            String::new(),
            Arc::new(WallClock),
        )
    }

    #[test]
    fn the_queue_runs_highest_priority_first_and_stable() {
        let store = Arc::new(InMemoryStore::new());
        let mut data = data(&store);
        assert!(data.on_start());

        let mgr = TaskManager::new();
        let start = |time: i64, priority: i32| {
            mgr.add_background_task_at(
                Task::AddServerStart {
                    uid: 1000,
                    username: "root".into(),
                    server_start_time: time as u64,
                },
                priority,
            );
        };
        start(1, priority::MAX);
        start(2, priority::DEFAULT);
        start(3, priority::LOW);
        start(4, priority::DEFAULT);
        start(5, priority::DEFAULT);

        while mgr.bg_task_list_size() > 0 {
            mgr.tick(&mut data);
        }

        // MAX first, then the DEFAULT tasks in queue order, then LOW.
        let db = format!(
            "{}root{}",
            constants::USER_DB_PREFIX,
            constants::LOGGER_ID_SEPARATOR
        );
        let docs = store.documents(&format!("{db}.{}", constants::SERVER_STARTS_COLL_NAME));
        let times: Vec<i64> = docs
            .iter()
            .map(|doc| doc.get_date(constants::SERVER_STARTS_KEY_TIME))
            .collect();
        assert_eq!(times, [1, 2, 4, 5, 3]);
    }

    #[test]
    fn a_general_error_aborts_all_further_processing() {
        let store = Arc::new(InMemoryStore::new());
        let mut data = data(&store);

        assert!(data.on_start());
        // Remove the components collection to make the update miss.
        let db = format!(
            "{}root{}",
            constants::USER_DB_PREFIX,
            constants::LOGGER_ID_SEPARATOR
        );
        store.drop_collection(&format!("{db}.{}", constants::COMPONENTS_COLL_NAME));

        let mut obj = BsonObj::new();
        obj.append(constants::COMPONENTS_UID, Bson::Int(1000));
        obj.append(constants::COMPONENTS_USERNAME, Bson::Str("root".into()));
        obj.append(constants::COMPONENTS_KEY_HOST, Bson::Int(1));
        obj.append(constants::COMPONENTS_KEY_PID, Bson::Int(7));
        obj.append(constants::COMPONENTS_KEY_COMPONENT, Bson::Int(3));

        let mgr = TaskManager::new();
        mgr.add_background_task(Task::UpdateAppID {
            uid: 1000,
            username: "root".into(),
            obj,
            appid: 5,
        });
        mgr.tick(&mut data);

        assert!(data.should_abort_further_processing());

        // Queued tasks after the abort are dropped, not performed.
        mgr.add_background_task(Task::Roll);
        mgr.tick(&mut data);
        assert_eq!(mgr.bg_task_list_size(), 0);
    }

    #[test]
    fn a_connection_error_requeues_the_record_task_at_high() {
        let store = Arc::new(InMemoryStore::new());
        let mut data = data(&store);
        assert!(data.on_start());
        // The record's collection has to exist for the write to have a
        // home once the connection comes back.
        store
            .create_collection(
                &format!(
                    "{}.{}",
                    constants::BW_COMMON_DB_NAME,
                    constants::SOURCES_COLL
                ),
                0,
                false,
                0,
            )
            .unwrap();
        data.set_max_reconnect_attempts(1);

        // Take the server down for good: every reconnect attempt fails.
        store.stop_server();

        let mut record = BsonObj::new();
        record.append(constants::SOURCES_KEY_ID, Bson::Int(0));
        record.append(constants::SOURCES_KEY_NAME, Bson::Str("CPP".into()));

        let mgr = TaskManager::new();
        mgr.add_background_task(Task::WriteIDStringPair {
            collection_name: format!(
                "{}.{}",
                constants::BW_COMMON_DB_NAME,
                constants::SOURCES_COLL
            ),
            id_key: constants::SOURCES_KEY_ID,
            id: 0,
            string_key: constants::SOURCES_KEY_NAME,
            string_value: "CPP".into(),
        });

        // First tick: the write hits the dead socket, the task re-queues.
        mgr.tick(&mut data);
        assert!(!data.is_connected());
        assert_eq!(mgr.bg_task_list_size(), 1, "re-added at HIGH");

        // Restore; the next tick's prologue reconnects and the task lands.
        store.start_server();
        store.restore_connection();
        data.set_max_reconnect_attempts(0);
        mgr.tick(&mut data);
        assert!(data.is_connected());
        assert_eq!(mgr.bg_task_list_size(), 0);

        let docs = store.documents(&format!(
            "{}.{}",
            constants::BW_COMMON_DB_NAME,
            constants::SOURCES_COLL
        ));
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].get_string(constants::SOURCES_KEY_NAME), "CPP");
    }

    #[test]
    fn a_flush_task_swaps_the_buffer_and_lands_the_batch() {
        let store = Arc::new(InMemoryStore::new());
        let mut data = data(&store);
        assert!(data.on_start());

        let mut buffer = crate::mongodb::user_log::UserLogBuffer::new("root".into());
        let mut doc = BsonObj::new();
        doc.append("msg", Bson::Str("queued".into()));
        buffer.append(doc);

        let mgr = TaskManager::new();
        mgr.add_background_task(Task::Flush {
            uid: 1000,
            username: buffer.username().to_string(),
            log_buffer: buffer.take(),
        });
        // The constructor consumed the buffer.
        assert_eq!(buffer.size(), 0);

        mgr.tick(&mut data);
        assert_eq!(mgr.bg_task_list_size(), 0);

        let db = format!(
            "{}root{}",
            constants::USER_DB_PREFIX,
            constants::LOGGER_ID_SEPARATOR
        );
        let entries: Vec<_> = store
            .collection_names(&db)
            .unwrap()
            .into_iter()
            .filter(|name| name.contains(constants::USER_COLL_ENTRIES_PREFIX))
            .collect();
        assert_eq!(entries.len(), 1);
        assert_eq!(store.documents(&entries[0]).len(), 1);
    }
}
