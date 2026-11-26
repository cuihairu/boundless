//! BW-07-03-04: shared-data propagation across BaseApps and CellApps, and its
//! recovery after a manager death.
//!
//! Shared data is a key -> value map whose scope is decided by a data type
//! (`lib/server/shared_data_type.hpp`): `CELL_APP` (1, the CellAppMgr is
//! authoritative), `BASE_APP` (2, the BaseAppMgr is authoritative), `GLOBAL`
//! (3, shared by every app; the CellAppMgr is authoritative) and
//! `GLOBAL_FROM_BASE_APP` (4, a BaseApp's request to set global data).
//!
//! A BASE_APP set comes from a BaseApp (`BaseAppMgr::setSharedData`,
//! `baseappmgr.cpp:2112`): the BaseAppMgr stores the authoritative copy and
//! broadcasts `BaseAppIntInterface::setSharedData` to every BaseApp. A
//! GLOBAL_FROM_BASE_APP set is not broadcast by the BaseAppMgr — it forwards
//! `CellAppMgrInterface::setSharedData(GLOBAL, ...)` to the CellAppMgr
//! (`baseappmgr.cpp:2146`), which stores its authoritative copy, broadcasts
//! `CellAppInterface::setSharedData` to every CellApp, and relays
//! `BaseAppMgrInterface::setSharedData(GLOBAL, ...)` back to the BaseAppMgr,
//! whereupon the BaseAppMgr stores and broadcasts to every BaseApp. So global
//! data reaches BaseApps and CellApps through the same original sender, and the
//! events arrive in a fixed order (forward, cell broadcast, relay, base
//! broadcast). The delete path mirrors this (`baseappmgr.cpp:2183`,
//! `cellappmgr.cpp:1985`). The authoritative maps are recovered on a manager
//! death (`BaseAppMgr::recoverSharedDataFromStream`, `baseappmgr.cpp:1249`).

use crate::baseappmgr::{Addr, ToBaseApp};
use foundation::stream::{BinaryStream, StreamError};

/// The scope of a shared-data entry (`SharedDataType`).
pub use crate::baseappmgr::{
    SHARED_DATA_TYPE_BASE_APP, SHARED_DATA_TYPE_CELL_APP, SHARED_DATA_TYPE_GLOBAL,
    SHARED_DATA_TYPE_GLOBAL_FROM_BASE_APP,
};

/// The `setSharedData` / `delSharedData` payload (the same shape on the
/// `CellAppMgrInterface`, `CellAppInterface` and `BaseAppMgrInterface`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SharedDataMessage {
    /// `setSharedData`: the entry `(data_type, key, value)`.
    Set {
        data_type: u8,
        key: String,
        value: String,
        /// The address of the original invoker of the change.
        original_src: Addr,
    },
    /// `delSharedData`: the entry `(data_type, key)`.
    Del {
        data_type: u8,
        key: String,
        original_src: Addr,
    },
}

/// The outcome of a BaseAppMgr shared-data mutation
/// (`BaseAppMgr::setSharedData` / `delSharedData`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SharedDataOutcome {
    /// `BaseAppIntInterface::setSharedData` / `delSharedData` to every BaseApp.
    pub base_apps: Vec<(Addr, ToBaseApp)>,
    /// The `CellAppMgrInterface` forward for global-from-BaseApp / cell data
    /// (`None` for BASE_APP data or a plain GLOBAL relay).
    pub cell_app_mgr: Option<SharedDataMessage>,
}

/// The outcome of a CellAppMgr shared-data mutation (`CellAppMgr::setSharedData`
/// / `delSharedData`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CellAppSharedDataOutcome {
    /// `CellAppInterface::setSharedData` / `delSharedData` to every CellApp.
    pub cell_apps: Vec<(Addr, SharedDataMessage)>,
    /// The `BaseAppMgrInterface` relay for BASE_APP / GLOBAL data (`None` for
    /// CELL_APP data, which BaseApps cannot see).
    pub base_app_mgr: Option<SharedDataMessage>,
}

/// Read one recovery section: a count, then that many `(key, value)` pairs
/// (`recoverSharedDataFromStream`, `baseappmgr.cpp:1249`). The CellAppMgr
/// recovery uses the same shape for its CELL_APP and GLOBAL maps.
pub(crate) fn read_shared_section(
    stream: &mut BinaryStream,
) -> Result<BTreeMap<String, String>, StreamError> {
    let count = stream.read_u32()?;
    let mut entries = BTreeMap::new();
    for _ in 0..count {
        let key = stream.read_string()?;
        let value = stream.read_string()?;
        entries.insert(key, value);
    }
    Ok(entries)
}

/// The batched join-sync of a manager's authoritative shared-data maps —
/// the optimization form of the join fill. The reference streams one
/// `setSharedData` message per entry into the joining app's bundle
/// (`cellappmgr.cpp:1351-1377`, `baseappmgr.cpp:1414-1430`); the batch
/// carries every entry in one message, so a join costs one message
/// instead of one per entry.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SharedDataSync {
    /// The entries in map order: `(data_type, key, value)`.
    pub entries: Vec<(u8, String, String)>,
}

impl SharedDataSync {
    /// Builds the sync from two authoritative maps, the first map's
    /// entries ahead of the second's — the reference's fill order (cell
    /// data then global data, `cellappmgr.cpp:1351`/`:1366`; base-app
    /// data then global data, `baseappmgr.cpp:1414`/`:1422`).
    pub fn from_maps(
        first_type: u8,
        first: &BTreeMap<String, String>,
        second_type: u8,
        second: &BTreeMap<String, String>,
    ) -> Self {
        let mut entries = Vec::new();
        for (key, value) in first {
            entries.push((first_type, key.clone(), value.clone()));
        }
        for (key, value) in second {
            entries.push((second_type, key.clone(), value.clone()));
        }
        Self { entries }
    }

    /// The per-entry form the batch replaces: one `setSharedData`
    /// message per entry, in the same order.
    pub fn expand(&self) -> Vec<SharedDataMessage> {
        self.entries
            .iter()
            .map(|(data_type, key, value)| SharedDataMessage::Set {
                data_type: *data_type,
                key: key.clone(),
                value: value.clone(),
                original_src: (0, 0),
            })
            .collect()
    }

    /// The wire bytes of the per-entry form: a `setSharedData` payload
    /// per entry (type byte, key string, value string).
    pub fn per_entry_wire_bytes(&self) -> usize {
        let mut stream = BinaryStream::new();
        for (data_type, key, value) in &self.entries {
            write_set_payload(&mut stream, *data_type, key, value);
        }
        stream.length()
    }

    /// The wire bytes of the batched form: one payload of an entry
    /// count, then every `(type, key, value)`.
    pub fn batch_wire_bytes(&self) -> usize {
        let mut stream = BinaryStream::new();
        stream.write_count(self.entries.len() as u32);
        for (data_type, key, value) in &self.entries {
            write_set_payload(&mut stream, *data_type, key, value);
        }
        stream.length()
    }
}

/// One `setSharedData` payload: the type byte, then the key and value
/// strings (`bundle << dataType << key << value`,
/// `baseappmgr.cpp:2155-2159`).
fn write_set_payload(stream: &mut BinaryStream, data_type: u8, key: &str, value: &str) {
    stream.write_u8(data_type);
    let _ = stream.write_string(key);
    let _ = stream.write_string(value);
}

use std::collections::BTreeMap;

#[cfg(test)]
mod tests {
    use super::*;

    /// Cell data of two keys and global data of one.
    fn sync_fixture() -> SharedDataSync {
        let mut cell = BTreeMap::new();
        cell.insert("c".to_string(), "1".to_string());
        cell.insert("a".to_string(), "2".to_string());
        let mut global = BTreeMap::new();
        global.insert("g".to_string(), "3".to_string());
        SharedDataSync::from_maps(
            SHARED_DATA_TYPE_CELL_APP,
            &cell,
            SHARED_DATA_TYPE_GLOBAL,
            &global,
        )
    }

    /// `from_maps` streams the first map's entries ahead of the second's,
    /// each map in key order — the reference's fill order
    /// (`cellappmgr.cpp:1351-1377`, `baseappmgr.cpp:1414-1430`).
    #[test]
    fn the_sync_orders_the_maps_like_the_reference_fill() {
        assert_eq!(
            sync_fixture().entries,
            vec![
                (SHARED_DATA_TYPE_CELL_APP, "a".to_string(), "2".to_string()),
                (SHARED_DATA_TYPE_CELL_APP, "c".to_string(), "1".to_string()),
                (SHARED_DATA_TYPE_GLOBAL, "g".to_string(), "3".to_string()),
            ]
        );
    }

    /// `expand` is one `setSharedData` message per entry, in sync order.
    #[test]
    fn the_sync_expands_to_one_message_per_entry() {
        let messages = sync_fixture().expand();
        assert_eq!(messages.len(), 3);
        assert_eq!(
            messages[2],
            SharedDataMessage::Set {
                data_type: SHARED_DATA_TYPE_GLOBAL,
                key: "g".to_string(),
                value: "3".to_string(),
                original_src: (0, 0),
            }
        );
    }

    /// The batch payload is the entry payloads plus the one u32 count —
    /// its message-count saving lives in the per-message header costs
    /// the payload model does not carry.
    #[test]
    fn the_batch_payload_is_the_entries_plus_a_count() {
        let sync = sync_fixture();
        assert_eq!(sync.batch_wire_bytes(), sync.per_entry_wire_bytes() + 4);
    }
}
