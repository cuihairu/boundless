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

use std::collections::BTreeMap;
