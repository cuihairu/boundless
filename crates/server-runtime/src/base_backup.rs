//! BW-07-04-02: transmit and apply a Base backup.
//!
//! A live BaseApp sends a copy of each base entity to its backup targets
//! (`BackupSender::tick` → `BackupSender::backupBase`,
//! `backup_sender.cpp:109`/`:267`): the destination for an entity is the
//! address its id hashes to in `entityToAppHash_` (a `BackupHash`,
//! `backup_sender.cpp:271`), and while the app is retiring that destination is
//! resolved through the `BackupHashChain` for dead apps (`backup_sender.cpp:315`).
//! The payload is `Base::writeBackupData` (`base.cpp:2312`): a header of id,
//! type id, template name and database id followed by the compressed body
//! `writeBackupDataInternal` (`base.cpp:2349`) — cell address, the pending
//! flags, the space id, timers, the base attributes and the cell data.
//!
//! The receiving BaseApp (`BaseApp::backupBaseEntity`, `baseapp.cpp:2531`) keeps
//! the bytes in a `BackedUpBaseApp` keyed by the sender app and entity id. When
//! the BaseApp that owned the entity dies, its backups are applied:
//! `BackedUpEntities::restore` (`backed_up_base_app.cpp:176`) hands each stored
//! stream to `BaseApp::createBaseFromStream` (`baseapp.cpp:2589`), which
//! validates the type, builds a base from the backup and restores the
//! attributes. After the takeover every restored entity carries the same
//! properties as the one that was backed up.
//!
//! This module models the sender side (choosing the destination), the backup
//! payload (its stream format) and the receiver (storing backups and restoring
//! them after a death). The shipped payload writes the body uncompressed; the
//! wrapped face (`write_backup_data_wrapped` / `read_backup_data_wrapped`)
//! carries the body through the reference's `CompressionOStream` frame — the
//! plaintext header, then the type byte and, for a zip type, the zlib
//! payload. That is the transport the reference picks per entity type
//! (`internalNetworkCompressionType`, default `NONE`).

use crate::backup_hash::{BackupHash, BackupHashChain};
use crate::baseappmgr::Addr;
use crate::cell_data::BaseAddr;
use crate::entity_creator::{BaseEntity, EntityId, EntityType, PropValue};
use crate::replay_recording::{CompressError, compress_frame, decompress_frame};
use foundation::stream::{BinaryStream, StreamError};

/// A `backupBaseEntity` message (`BaseAppIntInterface::backupBaseEntity`,
/// `baseapp.cpp:2531`): the destination BaseApp, the id and the backup bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupMessage {
    /// The destination BaseApp.
    pub dest: Addr,
    /// Whether the entity is being offloaded to `dest` (the onload path) or
    /// merely backed up to it.
    pub is_offload: bool,
    /// The entity id (`data >> entityID` on the receiving side).
    pub entity_id: EntityId,
    /// The `writeBackupData` bytes (the header plus the body).
    pub data: Vec<u8>,
}

/// The failure of applying a backup (`createBaseFromStream`,
/// `baseapp.cpp:2604`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackupError {
    /// The stream was truncated or malformed.
    Stream(StreamError),
    /// The backup's type is not registered (`createBaseFromStream`).
    UnknownType {
        /// The type name the backup claimed.
        type_name: String,
    },
    /// The backup's type id or name disagrees with the registered type.
    TypeMismatch {
        /// The registered type's name.
        type_name: String,
    },
    /// The type cannot live on the base (`!pType->canBeOnBase()`).
    NotOnBase {
        /// The type name the backup claimed.
        type_name: String,
    },
    /// A property value carried an unknown tag.
    UnknownPropTag {
        /// The tag byte.
        tag: u8,
    },
    /// The compression frame around the body failed (`CompressionIStream`
    /// reading a broken frame).
    Compression(CompressError),
}

impl From<StreamError> for BackupError {
    fn from(err: StreamError) -> Self {
        BackupError::Stream(err)
    }
}

impl From<CompressError> for BackupError {
    fn from(err: CompressError) -> Self {
        BackupError::Compression(err)
    }
}

/// The tag of a `PropValue` in a backup stream.
const PROP_INT: u8 = 0;
const PROP_FLOAT: u8 = 1;
const PROP_STR: u8 = 2;
const PROP_BOOL: u8 = 3;
const PROP_VEC3: u8 = 4;

/// `BackupSender::backupBase` (`backup_sender.cpp:267`): the BaseApp to send
/// one entity's backup to, or `Mercury::Address::NONE` when there is no
/// backup target. While the app is offloading, a dead destination is resolved
/// through the backup hash chain.
pub fn backup_destination(
    hash: &BackupHash,
    chain: &BackupHashChain,
    entity_id: EntityId,
    is_offloading: bool,
) -> Addr {
    let dest = hash.address_for(entity_id);
    if is_offloading {
        chain.address_for(dest, entity_id)
    } else {
        dest
    }
}

/// `Base::writeBackupData` (`base.cpp:2312`): write the header the restore
/// reads first (`id`, `type id`, `templateID`, `databaseID`) and then the body
/// `writeBackupDataInternal` (`base.cpp:2349`).
pub fn write_backup_data(base: &BaseEntity, stream: &mut BinaryStream) {
    write_backup_header(stream, base);
    write_backup_body(stream, base);
}

/// `Base::writeBackupData` with the body behind the `CompressionOStream`
/// frame (`base.cpp:2322-2327`): the plaintext header, then the body as the
/// compression frame — the type byte first, and for a zip type a packed
/// uncompressed length, a packed compressed length and the zlib payload
/// (`zip_stream.cpp:261-286`). `compression_type` is the entity type's
/// `internalNetworkCompressionType` (`base.cpp:2325`): the `DEFAULT_*`
/// sentinels resolve to `NONE` (`compression_stream.cpp:53-58`), which
/// passes the body through behind the type byte.
pub fn write_backup_data_wrapped(
    base: &BaseEntity,
    stream: &mut BinaryStream,
    compression_type: u8,
) -> Result<(), CompressError> {
    write_backup_header(stream, base);

    let mut body = BinaryStream::new();
    write_backup_body(&mut body, base);
    let frame = compress_frame(compression_type, body.as_bytes())?;
    stream.write_bytes(&frame);
    Ok(())
}

/// The `writeBackupData` header (`base.cpp:2320`), written plaintext — the
/// restore and the `peek_type_name` probe read it before any decompression.
pub fn write_backup_header(stream: &mut BinaryStream, base: &BaseEntity) {
    stream.write_u32(base.id);
    stream.write_u32(base.type_id);
    let _ = stream.write_string(&base.type_name);
    stream.write_u64(base.db_id);
}

/// `Base::writeBackupDataInternal` (`base.cpp:2349`): the body — the cell
/// address, the pending flags, the space id, the base attributes and the
/// cell data.
fn write_backup_body(stream: &mut BinaryStream, base: &BaseEntity) {
    write_opt_addr(stream, base.cell_entity_addr);
    stream.write_bool(base.is_create_cell_pending);
    stream.write_bool(base.is_get_cell_pending);
    stream.write_u32(base.cell_space_id);
    write_props(stream, &base.base_props);
    write_props(stream, &base.cell_props);
}

/// `BaseApp::createBaseFromStream` (`baseapp.cpp:2589`) reading the header,
/// then `Base::readBackupDataInternal` (`base.cpp:2530`) reading the body.
/// `ty` is the registered type the backup's id and name are checked against.
pub fn read_backup_data(
    stream: &mut BinaryStream,
    ty: &EntityType,
) -> Result<BaseBackupData, BackupError> {
    let (id, type_id, type_name, db_id) = read_backup_header(stream)?;
    check_backup_type(type_id, &type_name, ty)?;

    let body = read_backup_body(stream)?;
    Ok(assemble_backup_data(
        id, type_id, type_name, db_id, ty, body,
    ))
}

/// `read_backup_data` for bytes stored in the wrapped form
/// (`write_backup_data_wrapped`): the same plaintext header, then the body
/// out of the `CompressionIStream` frame (`base.cpp:2521`).
pub fn read_backup_data_wrapped(
    stream: &mut BinaryStream,
    ty: &EntityType,
) -> Result<BaseBackupData, BackupError> {
    let (id, type_id, type_name, db_id) = read_backup_header(stream)?;
    check_backup_type(type_id, &type_name, ty)?;

    let body_bytes = decompress_frame(stream.unread_bytes())?;
    let mut body_stream = BinaryStream::from_slice(&body_bytes);
    let body = read_backup_body(&mut body_stream)?;
    Ok(assemble_backup_data(
        id, type_id, type_name, db_id, ty, body,
    ))
}

/// The `createBaseFromStream` header read (`baseapp.cpp:2604`).
fn read_backup_header(stream: &mut BinaryStream) -> Result<(u32, u32, String, u64), StreamError> {
    let id = stream.read_u32()?;
    let type_id = stream.read_u32()?;
    let type_name = stream.read_string()?;
    let db_id = stream.read_u64()?;
    Ok((id, type_id, type_name, db_id))
}

/// The registered-type check the header read feeds (`baseapp.cpp:2604`).
fn check_backup_type(type_id: u32, type_name: &str, ty: &EntityType) -> Result<(), BackupError> {
    if type_id != ty.id || type_name != ty.name {
        return Err(BackupError::TypeMismatch {
            type_name: ty.name.clone(),
        });
    }
    Ok(())
}

/// The `readBackupDataInternal` body minus the header.
struct BackupBody {
    cell_entity_addr: Option<BaseAddr>,
    is_create_cell_pending: bool,
    is_get_cell_pending: bool,
    cell_space_id: u32,
    base_props: std::collections::HashMap<String, PropValue>,
    cell_props: std::collections::HashMap<String, PropValue>,
}

/// The `readBackupDataInternal` body read (`base.cpp:2530`).
fn read_backup_body(stream: &mut BinaryStream) -> Result<BackupBody, BackupError> {
    Ok(BackupBody {
        cell_entity_addr: read_opt_addr(stream)?,
        is_create_cell_pending: stream.read_bool()?,
        is_get_cell_pending: stream.read_bool()?,
        cell_space_id: stream.read_u32()?,
        base_props: read_props(stream)?,
        cell_props: read_props(stream)?,
    })
}

/// The parsed contents of a backup, assembled from the header and the body.
fn assemble_backup_data(
    id: EntityId,
    type_id: u32,
    type_name: String,
    db_id: u64,
    ty: &EntityType,
    body: BackupBody,
) -> BaseBackupData {
    BaseBackupData {
        id,
        type_id,
        type_name,
        db_id,
        is_proxy: ty.is_proxy,
        is_create_cell_pending: body.is_create_cell_pending,
        is_get_cell_pending: body.is_get_cell_pending,
        cell_entity_addr: body.cell_entity_addr,
        cell_space_id: body.cell_space_id,
        base_props: body.base_props,
        cell_props: body.cell_props,
    }
}

/// The plaintext header length of a `writeBackupData` payload — the split
/// the wrapped writers and readers frame the compression around.
pub fn header_len(base: &BaseEntity) -> usize {
    let mut stream = BinaryStream::new();
    write_backup_header(&mut stream, base);
    stream.length()
}

/// The parsed contents of a backup: the `createBaseFromStream` header
/// (`baseapp.cpp:2604`) plus the `readBackupDataInternal` body
/// (`base.cpp:2530`).
#[derive(Debug, Clone, PartialEq)]
pub struct BaseBackupData {
    /// The entity id.
    pub id: EntityId,
    /// The entity type id.
    pub type_id: u32,
    /// The entity type name (the `templateID` the restore looks the type up by).
    pub type_name: String,
    /// The database id.
    pub db_id: u64,
    /// `isProxy()`.
    pub is_proxy: bool,
    /// `isCreateCellPending_`.
    pub is_create_cell_pending: bool,
    /// `isGetCellPending_`.
    pub is_get_cell_pending: bool,
    /// The cell entity's address, or `None` for `Mercury::Address::NONE`.
    pub cell_entity_addr: Option<BaseAddr>,
    /// `spaceID_`.
    pub cell_space_id: u32,
    /// The base properties (`backUpAttributes`).
    pub base_props: std::collections::HashMap<String, PropValue>,
    /// The cell data (`backUpCellData`).
    pub cell_props: std::collections::HashMap<String, PropValue>,
}

/// `BaseApp::createBaseFromStream` → `Base::readBackupData`: apply a parsed
/// backup to build the restored entity. The type must be able to live on the
/// base.
pub fn restore_base(data: &BaseBackupData, ty: &EntityType) -> Result<BaseEntity, BackupError> {
    if !ty.can_be_on_base {
        return Err(BackupError::NotOnBase {
            type_name: ty.name.clone(),
        });
    }
    Ok(BaseEntity {
        id: data.id,
        type_id: ty.id,
        type_name: ty.name.clone(),
        db_id: data.db_id,
        is_proxy: ty.is_proxy,
        destroyed: false,
        in_destroy: false,
        base_props: data.base_props.clone().into_iter().collect(),
        cell_props: data.cell_props.clone().into_iter().collect(),
        is_create_cell_pending: data.is_create_cell_pending,
        is_get_cell_pending: data.is_get_cell_pending,
        cell_entity_addr: data.cell_entity_addr,
        cell_space_id: data.cell_space_id,
        callbacks: Vec::new(),
    })
}

/// One BaseApp's stored backups (`BackedUpBaseApp`/`BackedUpEntities`,
/// `backed_up_base_app.cpp:176`): the raw bytes per backed-up app, then per
/// entity id, in the order the backups arrived.
#[derive(Debug, Clone, Default)]
pub struct BackupReceiver {
    /// Backups by the address they came from, then by entity id.
    backups: std::collections::BTreeMap<Addr, std::collections::BTreeMap<EntityId, Vec<u8>>>,
}

impl BackupReceiver {
    /// An empty receiver.
    pub fn new() -> Self {
        Self::default()
    }

    /// `BaseApp::backupBaseEntity` (`baseapp.cpp:2531`), the non-offload path:
    /// store the backup bytes for the entity on the sender's app. A newer
    /// backup of the same entity replaces the old one.
    pub fn back_up_entity(&mut self, src_addr: Addr, message: &BackupMessage) -> EntityId {
        self.backups
            .entry(src_addr)
            .or_default()
            .insert(message.entity_id, message.data.clone());
        message.entity_id
    }

    /// `BackedUpEntities::restore` (`backed_up_base_app.cpp:176`): apply every
    /// backup stored from the given app, building a base for each entity.
    /// `lookup` resolves a type name to its `EntityType`; entities whose type
    /// is unknown or not on the base are reported instead of built.
    pub fn restore(
        &self,
        dead_addr: Addr,
        lookup: &impl Fn(&str) -> Option<EntityType>,
    ) -> Vec<Result<BaseEntity, BackupError>> {
        self.restore_inner(dead_addr, lookup, false)
    }

    /// `restore` for backups stored in the wrapped form
    /// (`write_backup_data_wrapped`): the header reads the same way, then the
    /// body comes out of the `CompressionIStream` frame (`base.cpp:2521`).
    pub fn restore_wrapped(
        &self,
        dead_addr: Addr,
        lookup: &impl Fn(&str) -> Option<EntityType>,
    ) -> Vec<Result<BaseEntity, BackupError>> {
        self.restore_inner(dead_addr, lookup, true)
    }

    /// The restore loop over the dead app's stored backups, in either the
    /// shipped or the wrapped payload form.
    fn restore_inner(
        &self,
        dead_addr: Addr,
        lookup: &impl Fn(&str) -> Option<EntityType>,
        wrapped: bool,
    ) -> Vec<Result<BaseEntity, BackupError>> {
        let Some(stored) = self.backups.get(&dead_addr) else {
            return Vec::new();
        };
        stored
            .values()
            .map(|data| {
                let type_name = peek_type_name(data)?;
                let ty = lookup(&type_name).ok_or(BackupError::UnknownType { type_name })?;
                let mut stream = stream_of(data);
                let parsed = if wrapped {
                    read_backup_data_wrapped(&mut stream, &ty)?
                } else {
                    read_backup_data(&mut stream, &ty)?
                };
                restore_base(&parsed, &ty)
            })
            .collect()
    }

    /// The stored backups, by source address then entity id.
    pub fn backups(
        &self,
    ) -> &std::collections::BTreeMap<Addr, std::collections::BTreeMap<EntityId, Vec<u8>>> {
        &self.backups
    }
}

/// A fresh stream seeded with stored backup bytes, ready to read.
fn stream_of(data: &[u8]) -> BinaryStream {
    let mut stream = BinaryStream::new();
    stream.write_bytes(data);
    stream
}

/// Peek at the type name in the header of stored backup bytes so the caller
/// can look up the `EntityType` before parsing.
fn peek_type_name(data: &[u8]) -> Result<String, BackupError> {
    let mut stream = stream_of(data);
    let _id = stream.read_u32()?;
    let _type_id = stream.read_u32()?;
    Ok(stream.read_string()?)
}

/// Write an optional address: a presence byte then the packed address.
fn write_opt_addr(stream: &mut BinaryStream, addr: Option<BaseAddr>) {
    match addr {
        Some(addr) => {
            stream.write_bool(true);
            addr.write_to(stream);
        }
        None => {
            stream.write_bool(false);
        }
    }
}

/// Read an optional address (the packed `ip`, `port` pair of `write_to`).
fn read_opt_addr(stream: &mut BinaryStream) -> Result<Option<BaseAddr>, StreamError> {
    match stream.read_bool()? {
        true => {
            let ip = stream.read_u32()?;
            let port = stream.read_u16()?;
            Ok(Some(BaseAddr { ip, port }))
        }
        false => Ok(None),
    }
}

/// Write the property map: a count, then each `(name, tagged value)` in name
/// order, so the same state always serialises to the same bytes.
fn write_props(stream: &mut BinaryStream, props: &std::collections::HashMap<String, PropValue>) {
    let mut names: Vec<&String> = props.keys().collect();
    names.sort();
    stream.write_count(names.len() as u32);
    for name in names {
        let _ = stream.write_string(name);
        write_prop(stream, &props[name]);
    }
}

/// Read the property map.
fn read_props(
    stream: &mut BinaryStream,
) -> Result<std::collections::HashMap<String, PropValue>, BackupError> {
    let count = stream.read_count()?;
    let mut props = std::collections::HashMap::new();
    for _ in 0..count {
        let name = stream.read_string()?;
        let value = read_prop(stream)?;
        props.insert(name, value);
    }
    Ok(props)
}

/// Write one tagged property value.
fn write_prop(stream: &mut BinaryStream, value: &PropValue) {
    match value {
        PropValue::Int(int) => {
            stream.write_u8(PROP_INT);
            stream.write_i64(*int);
        }
        PropValue::Float(float) => {
            stream.write_u8(PROP_FLOAT);
            stream.write_f64(*float);
        }
        PropValue::Str(text) => {
            stream.write_u8(PROP_STR);
            let _ = stream.write_string(text);
        }
        PropValue::Bool(flag) => {
            stream.write_u8(PROP_BOOL);
            stream.write_bool(*flag);
        }
        PropValue::Vec3(vec3) => {
            stream.write_u8(PROP_VEC3);
            for component in vec3 {
                stream.write_f32(*component);
            }
        }
    }
}

/// Read one tagged property value.
fn read_prop(stream: &mut BinaryStream) -> Result<PropValue, BackupError> {
    let tag = stream.read_u8()?;
    Ok(match tag {
        PROP_INT => PropValue::Int(stream.read_i64()?),
        PROP_FLOAT => PropValue::Float(stream.read_f64()?),
        PROP_STR => PropValue::Str(stream.read_string()?),
        PROP_BOOL => PropValue::Bool(stream.read_bool()?),
        PROP_VEC3 => PropValue::Vec3([stream.read_f32()?, stream.read_f32()?, stream.read_f32()?]),
        _ => return Err(BackupError::UnknownPropTag { tag }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::replay_recording::{
        COMPRESSION_DEFAULT_EXTERNAL, COMPRESSION_DEFAULT_INTERNAL, COMPRESSION_NONE,
        COMPRESSION_ZIP_BEST_COMPRESSION, COMPRESSION_ZIP_MAX, COMPRESSION_ZIP_MIN,
    };
    use std::collections::HashMap;

    fn account_type() -> EntityType {
        EntityType {
            id: 1,
            name: "Account".to_string(),
            can_be_on_base: true,
            is_service: false,
            is_proxy: false,
            can_be_on_cell: true,
            properties: vec![],
            methods: vec![],
        }
    }

    fn lookup_of(ty: EntityType) -> impl Fn(&str) -> Option<EntityType> {
        let name = ty.name.clone();
        move |queried: &str| (queried == name).then(|| ty.clone())
    }

    fn props() -> HashMap<String, PropValue> {
        let mut props = HashMap::new();
        props.insert("health".to_string(), PropValue::Int(200));
        props.insert("name".to_string(), PropValue::Str("alice".to_string()));
        props.insert("pos".to_string(), PropValue::Vec3([1.0, 2.0, 3.0]));
        props.insert("alive".to_string(), PropValue::Bool(true));
        props.insert("speed".to_string(), PropValue::Float(2.5));
        props
    }

    fn backed_up_base() -> BaseEntity {
        BaseEntity {
            id: 42,
            type_id: 1,
            type_name: "Account".to_string(),
            db_id: 7,
            is_proxy: false,
            destroyed: false,
            in_destroy: false,
            base_props: props(),
            cell_props: props(),
            is_create_cell_pending: true,
            is_get_cell_pending: false,
            cell_entity_addr: Some(BaseAddr {
                ip: 0x7f000001,
                port: 5000,
            }),
            cell_space_id: 3,
            callbacks: Vec::new(),
        }
    }

    /// `backup_message(base, src_addr)`: serialise the base and wrap it in the
    /// message shape the reference sends.
    fn backup_message(base: &BaseEntity, src_addr: Addr) -> BackupMessage {
        let mut stream = BinaryStream::new();
        write_backup_data(base, &mut stream);
        BackupMessage {
            dest: src_addr,
            is_offload: false,
            entity_id: base.id,
            data: stream.as_bytes().to_vec(),
        }
    }

    /// `backupDestination` picks the hashed target, or NONE when the table is
    /// empty; while offloading it resolves a dead target through the chain.
    #[test]
    fn the_destination_is_the_hashed_target() {
        let mut hash = BackupHash::with_prime(0x9e377e55);
        hash.push_back((2, 2000));
        let chain = BackupHashChain::new();

        assert_eq!(backup_destination(&hash, &chain, 42, false), (2, 2000));

        let empty = BackupHash::with_prime(0x9e377e55);
        assert_eq!(
            backup_destination(&empty, &chain, 42, false),
            crate::backup_hash::HASH_NONE_ADDR
        );
    }

    #[test]
    fn offloading_resolves_a_dead_target_through_the_chain() {
        let mut hash = BackupHash::with_prime(0x9e377e55);
        hash.push_back((2, 2000)); // the hashed target is dead
        let mut chain = BackupHashChain::new();
        let mut dead_hash = BackupHash::with_prime(0x9e377e55);
        dead_hash.push_back((3, 3000)); // its backup lives here
        chain.adjust_for_dead_base_app((2, 2000), dead_hash);

        assert_eq!(backup_destination(&hash, &chain, 42, true), (3, 3000));
        assert_eq!(
            backup_destination(&hash, &chain, 42, false),
            (2, 2000),
            "a live backup target is not resolved"
        );
    }

    /// A stored backup restores to an entity whose properties match the one
    /// that was backed up: every value, flag and address round-trips.
    #[test]
    fn a_stored_backup_restores_matching_properties() {
        let base = backed_up_base();
        let mut receiver = BackupReceiver::new();
        receiver.back_up_entity((1, 1000), &backup_message(&base, (1, 1000)));

        let ty = account_type();
        let restored = receiver
            .restore((1, 1000), &lookup_of(ty))
            .remove(0)
            .unwrap();
        assert_eq!(restored.id, 42);
        assert_eq!(restored.type_id, 1);
        assert_eq!(restored.type_name, "Account");
        assert_eq!(restored.db_id, 7);
        assert!(!restored.is_proxy);
        assert!(!restored.destroyed);
        assert!(restored.is_create_cell_pending);
        assert!(!restored.is_get_cell_pending);
        assert_eq!(restored.cell_space_id, 3);
        assert_eq!(
            restored.cell_entity_addr,
            Some(BaseAddr {
                ip: 0x7f000001,
                port: 5000
            })
        );
        assert_eq!(restored.base_props, props());
        assert_eq!(restored.cell_props, props());
    }

    /// A newer backup of the same entity replaces the older one.
    #[test]
    fn a_newer_backup_replaces_the_older_one() {
        let mut first = backed_up_base();
        first
            .base_props
            .insert("health".to_string(), PropValue::Int(50));
        let mut second = backed_up_base();
        second
            .base_props
            .insert("health".to_string(), PropValue::Int(150));

        let mut receiver = BackupReceiver::new();
        receiver.back_up_entity((1, 1000), &backup_message(&first, (1, 1000)));
        receiver.back_up_entity((1, 1000), &backup_message(&second, (1, 1000)));

        let ty = account_type();
        let restored = receiver
            .restore((1, 1000), &lookup_of(ty))
            .remove(0)
            .unwrap();
        assert_eq!(
            restored.base_props.get("health"),
            Some(&PropValue::Int(150))
        );
    }

    /// A backup whose type id or name disagrees with the registered type is
    /// rejected.
    #[test]
    fn a_misnamed_type_is_rejected() {
        let base = backed_up_base();
        let mut stream = BinaryStream::new();
        write_backup_data(&base, &mut stream);

        let ty = EntityType {
            name: "Player".to_string(),
            id: 2,
            ..account_type()
        };
        assert_eq!(
            read_backup_data(&mut stream, &ty).unwrap_err(),
            BackupError::TypeMismatch {
                type_name: "Player".to_string()
            }
        );
    }

    /// A type that cannot live on the base is rejected at the restore.
    #[test]
    fn a_type_that_cannot_live_on_the_base_is_rejected() {
        let base = backed_up_base();
        let mut stream = BinaryStream::new();
        write_backup_data(&base, &mut stream);
        let ty = account_type();
        let data = read_backup_data(&mut stream, &ty).unwrap();

        let cell_only = EntityType {
            can_be_on_base: false,
            ..account_type()
        };
        assert_eq!(
            restore_base(&data, &cell_only).unwrap_err(),
            BackupError::NotOnBase {
                type_name: "Account".to_string()
            }
        );
    }

    /// A truncated backup stream fails the restore rather than restoring a
    /// half-filled entity.
    #[test]
    fn a_truncated_backup_fails_the_restore() {
        let base = backed_up_base();
        let mut stream = BinaryStream::new();
        write_backup_data(&base, &mut stream);

        let mut truncated = BinaryStream::new();
        truncated.write_u32(base.id);
        truncated.write_u32(base.type_id);
        let ty = account_type();
        assert!(matches!(
            read_backup_data(&mut truncated, &ty).unwrap_err(),
            BackupError::Stream(_)
        ));
    }

    /// A backup stored from a different sender is not restored when the given
    /// app dies.
    #[test]
    fn only_the_dead_apps_backups_are_restored() {
        let base = backed_up_base();
        let mut receiver = BackupReceiver::new();
        receiver.back_up_entity((1, 1000), &backup_message(&base, (1, 1000)));

        let ty = account_type();
        assert!(receiver.restore((9, 9), &lookup_of(ty.clone())).is_empty());
        assert_eq!(receiver.restore((1, 1000), &lookup_of(ty)).len(), 1);
    }

    /// A backup of an unregistered type reports the unknown type instead of
    /// building an entity.
    #[test]
    fn an_unknown_type_reports_instead_of_building() {
        let mut base = backed_up_base();
        base.type_name = "Ghost".to_string();
        let mut receiver = BackupReceiver::new();
        receiver.back_up_entity((1, 1000), &backup_message(&base, (1, 1000)));

        let ty = account_type();
        let mut outcomes = receiver.restore((1, 1000), &lookup_of(ty));
        assert_eq!(
            outcomes.remove(0).unwrap_err(),
            BackupError::UnknownType {
                type_name: "Ghost".to_string()
            }
        );
    }

    /// The wrapped `NONE` frame is the shipped payload with one byte added:
    /// the same plaintext header, then the `BW_COMPRESSION_NONE` type byte,
    /// then the identical uncompressed body (`compression_stream.cpp:73-76`).
    #[test]
    fn the_wrapped_none_frame_is_the_shipped_bytes_plus_the_type_byte() {
        let base = backed_up_base();
        let mut raw = BinaryStream::new();
        write_backup_data(&base, &mut raw);
        let mut wrapped = BinaryStream::new();
        write_backup_data_wrapped(&base, &mut wrapped, COMPRESSION_NONE).unwrap();

        let raw = raw.as_bytes();
        let wrapped = wrapped.as_bytes();
        let mut header = BinaryStream::new();
        write_backup_header(&mut header, &base);
        let header_len = header.length();

        assert_eq!(&wrapped[..header_len], &raw[..header_len]);
        assert_eq!(wrapped[header_len], COMPRESSION_NONE);
        assert_eq!(&wrapped[header_len + 1..], &raw[header_len..]);
    }

    /// Every zip level writes a distinct frame whose body inflates back to
    /// the shipped body, and every level parses to the same backup data.
    #[test]
    fn the_wrapped_zip_frames_round_trip_the_body() {
        let base = backed_up_base();
        let ty = account_type();

        let mut raw = BinaryStream::new();
        write_backup_data(&base, &mut raw);
        let raw_body = raw.as_bytes()[header_len(&base)..].to_vec();

        let mut none = BinaryStream::new();
        write_backup_data_wrapped(&base, &mut none, COMPRESSION_NONE).unwrap();
        let mut raw_stream = BinaryStream::from_slice(raw.as_bytes());
        let expected = read_backup_data(&mut raw_stream, &ty).unwrap();
        let mut none_stream = BinaryStream::from_slice(none.as_bytes());
        assert_eq!(
            read_backup_data_wrapped(&mut none_stream, &ty).unwrap(),
            expected,
            "the NONE frame parses like the shipped payload"
        );

        for level in COMPRESSION_ZIP_MIN..=COMPRESSION_ZIP_MAX {
            let mut stream = BinaryStream::new();
            write_backup_data_wrapped(&base, &mut stream, level).unwrap();
            let frame = stream.as_bytes();

            // The frame inflates to the body the shipped payload wrote.
            let body = decompress_frame(&frame[header_len(&base)..]).unwrap();
            assert_eq!(body, raw_body, "level {level} inflates to the body");
            assert_ne!(
                &frame[header_len(&base)..],
                &raw_body[..],
                "level {level} is a real frame, not a pass-through"
            );

            let mut stream = BinaryStream::from_slice(frame);
            let parsed = read_backup_data_wrapped(&mut stream, &ty).unwrap();
            assert_eq!(parsed, expected, "level {level} parses like NONE");
            assert_eq!(
                restore_base(&parsed, &ty).unwrap().base_props,
                base.base_props,
                "level {level} restores the same properties"
            );
        }
    }

    /// The `DEFAULT_INTERNAL` / `DEFAULT_EXTERNAL` sentinels resolve to
    /// `NONE` (`compression_stream.cpp:53-58`): their frames are byte-equal.
    #[test]
    fn the_default_sentinels_resolve_to_none() {
        let base = backed_up_base();
        for sentinel in [COMPRESSION_DEFAULT_INTERNAL, COMPRESSION_DEFAULT_EXTERNAL] {
            let mut stream = BinaryStream::new();
            write_backup_data_wrapped(&base, &mut stream, sentinel).unwrap();
            let mut none = BinaryStream::new();
            write_backup_data_wrapped(&base, &mut none, COMPRESSION_NONE).unwrap();
            assert_eq!(stream.as_bytes(), none.as_bytes());
        }
    }

    /// A broken compression frame is reported, not mis-parsed: an unknown
    /// type byte after the header fails the wrapped read, and the wrapped
    /// writer rejects a type outside the `BWCompressionType` range.
    #[test]
    fn a_broken_frame_is_reported() {
        let base = backed_up_base();
        let ty = account_type();

        let mut header = BinaryStream::new();
        write_backup_header(&mut header, &base);
        let mut stream = BinaryStream::from_slice(header.as_bytes());
        stream.write_u8(13);
        assert_eq!(
            read_backup_data_wrapped(&mut stream, &ty).unwrap_err(),
            BackupError::Compression(CompressError::InvalidCompressionType(13))
        );

        let mut stream = BinaryStream::new();
        assert_eq!(
            write_backup_data_wrapped(&base, &mut stream, 13).unwrap_err(),
            CompressError::InvalidCompressionType(13)
        );
    }

    /// A wrapped backup taken by the receiver restores through the wrapped
    /// path with the same properties, and the restore finds the type by the
    /// plaintext header exactly as the shipped path does.
    #[test]
    fn a_wrapped_backup_restores_through_the_receiver() {
        let base = backed_up_base();
        let mut stream = BinaryStream::new();
        write_backup_data_wrapped(&base, &mut stream, COMPRESSION_ZIP_BEST_COMPRESSION).unwrap();
        let message = BackupMessage {
            dest: (1, 1000),
            is_offload: false,
            entity_id: base.id,
            data: stream.as_bytes().to_vec(),
        };
        let mut receiver = BackupReceiver::new();
        receiver.back_up_entity((1, 1000), &message);

        let ty = account_type();
        let restored = receiver
            .restore_wrapped((1, 1000), &lookup_of(ty))
            .remove(0)
            .unwrap();
        assert_eq!(restored.id, 42);
        assert_eq!(restored.base_props, props());
        assert_eq!(restored.cell_props, props());
    }
}
