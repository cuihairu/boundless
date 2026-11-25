//! The file close and failure path of the replay writer
//! (`ReplayDataFileWriter::close`/`finalise`/`closeWriter` and the listener
//! notifications, `replay_data_file_writer.cpp:528`). The tick chunking lives
//! in [`super::replay_tick`]; this module owns the handover to the background
//! file writer and the completion/error/destruction callbacks that a restarted
//! server and the file writer's owner observe.

use std::cell::RefCell;
use std::fmt;
use std::rc::Rc;

use foundation::stream::BinaryStream;

use crate::replay_header::{
    ReplayChecksumScheme, ReplayHeader, ReplayMetaData, write_replay_prefix,
};
use crate::replay_tick::{CompressionType, ReplayTickWriter};

/// The read-only permission a finalised replay file is chmodded to
/// (`S_IRUSR|S_IRGRP|S_IROTH`, `replay_data_file_writer.cpp:554`).
pub const READ_ONLY_MODE: u32 = 0o444;

/// The `GameTime` field rewritten in place on finalisation
/// (`replay_data_file_writer.cpp:545`).
const GAME_TIME_SIZE: usize = 4;

/// A background file operation the writer queues onto the
/// `IBackgroundFileWriter` (`background_file_writer.cpp:602`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileOp {
    /// `queueWrite`: append `data` to the file; `user_data` rides on the
    /// completion callback (`background_file_writer.cpp:838`).
    Write { data: Vec<u8>, user_data: u32 },
    /// `queueSeek` (`replay_data_file_writer.cpp:544`).
    Seek { offset: u64 },
    /// `queueWriteBlob`: overwrite in place at the current position.
    WriteBlob { data: Vec<u8> },
    /// `queueChmod`: the read-only permission handover (`:553`).
    Chmod { mode: u32 },
}

/// The background file writer the replay writer queues onto
/// (`IBackgroundFileWriter`, `background_file_writer.hpp:49`).
pub trait FileWriterBackend {
    /// The path being written.
    fn path(&self) -> &str;
    /// `queueWrite` (`background_file_writer.cpp:624`).
    fn queue_write(&mut self, data: Vec<u8>, user_data: u32);
    /// `queueSeek`.
    fn queue_seek(&mut self, offset: u64);
    /// `queueWriteBlob`.
    fn queue_write_blob(&mut self, data: Vec<u8>);
    /// `queueChmod`.
    fn queue_chmod(&mut self, mode: u32);
    /// `hasError` (`replay_data_file_writer.cpp:588`).
    fn has_error(&self) -> bool;
    /// `errorString` (`background_file_writer.cpp:829`).
    fn error_string(&self) -> String;
    /// `setListener`: attach or detach the completion listener
    /// (`background_file_writer.hpp:268`).
    fn set_listener(&mut self, attached: bool);
    /// Whether a completion listener is currently attached.
    fn listener_attached(&self) -> bool {
        true
    }
    /// The operations queued so far. The model records them so a test can
    /// assert the exact handover sequence.
    fn queued_operations(&self) -> Vec<FileOp> {
        Vec::new()
    }
}

/// A recording stand-in for `IBackgroundFileWriter`: it records every queued
/// operation and lets a test inject an error and inspect the listener state.
#[derive(Debug, Default)]
pub struct RecordingFileWriter {
    path: String,
    ops: Vec<FileOp>,
    has_error: bool,
    error_string: String,
    listener_attached: bool,
}

impl RecordingFileWriter {
    /// A writer for `path`.
    pub fn new(path: impl Into<String>) -> Self {
        RecordingFileWriter {
            path: path.into(),
            ..Default::default()
        }
    }

    /// The operations queued so far, in order.
    pub fn ops(&self) -> &[FileOp] {
        &self.ops
    }

    /// Inject a failure (`background_file_writer.cpp:797`).
    pub fn set_error(&mut self, message: impl Into<String>) {
        self.has_error = true;
        self.error_string = message.into();
    }

    /// Whether the completion listener is currently attached.
    pub fn listener_attached(&self) -> bool {
        self.listener_attached
    }
}

impl FileWriterBackend for RecordingFileWriter {
    fn path(&self) -> &str {
        &self.path
    }

    fn queue_write(&mut self, data: Vec<u8>, user_data: u32) {
        self.ops.push(FileOp::Write { data, user_data });
    }

    fn queue_seek(&mut self, offset: u64) {
        self.ops.push(FileOp::Seek { offset });
    }

    fn queue_write_blob(&mut self, data: Vec<u8>) {
        self.ops.push(FileOp::WriteBlob { data });
    }

    fn queue_chmod(&mut self, mode: u32) {
        self.ops.push(FileOp::Chmod { mode });
    }

    fn has_error(&self) -> bool {
        self.has_error
    }

    fn error_string(&self) -> String {
        self.error_string.clone()
    }

    fn set_listener(&mut self, attached: bool) {
        self.listener_attached = attached;
    }

    fn listener_attached(&self) -> bool {
        self.listener_attached
    }

    fn queued_operations(&self) -> Vec<FileOp> {
        self.ops.clone()
    }
}

/// A listener for a replay writer's events
/// (`ReplayDataFileWriterListener`, `replay_data_file_writer.hpp:62`).
pub trait ReplayWriterListener {
    /// `onReplayDataFileWritingComplete` (`replay_data_file_writer.cpp:700`).
    fn on_writing_complete(&mut self, path: &str);
    /// `onReplayDataFileWritingError` (`:660`).
    fn on_writing_error(&mut self, path: &str);
    /// `onReplayDataFileWriterDestroyed` (`:712`).
    fn on_writer_destroyed(&mut self, path: &str);
}

/// A listener that counts each event, for asserting the baseline callback
/// counts.
#[derive(Debug, Default)]
pub struct CallbackCounter {
    /// `onReplayDataFileWritingComplete` count.
    pub complete: usize,
    /// `onReplayDataFileWritingError` count.
    pub error: usize,
    /// `onReplayDataFileWriterDestroyed` count.
    pub destroyed: usize,
}

impl ReplayWriterListener for CallbackCounter {
    fn on_writing_complete(&mut self, _path: &str) {
        self.complete += 1;
    }

    fn on_writing_error(&mut self, _path: &str) {
        self.error += 1;
    }

    fn on_writer_destroyed(&mut self, _path: &str) {
        self.destroyed += 1;
    }
}

/// The `RecordingRecoveryData` a restarted server resumes a file from
/// (`recording_recovery_data.hpp:42`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryData {
    /// The compression level the file's tick payloads use.
    pub compression: CompressionType,
    /// Ticks already written to the file.
    pub num_ticks: u32,
    /// The last tick whose chunk write completed.
    pub last_tick_written: u32,
    /// The file offset of the next tick to write.
    pub next_chunk_position: u64,
    /// The signature of the last chunk.
    pub last_signature: Vec<u8>,
    /// The number of ticks per chunk.
    pub num_ticks_to_sign: u32,
}

/// A write the background file writer will execute, and the file position at
/// which it begins (`background_file_writer.cpp:381`).
#[derive(Debug, Clone, Copy)]
struct QueuedWrite {
    position: u64,
    length: usize,
    user_data: u32,
}

/// The `ReplayDataFileWriter`: owns the tick chunker and hands the produced
/// chunks to a background file writer, then drives the close path and the
/// listener callbacks.
pub struct ReplayFileWriter {
    scheme: ReplayChecksumScheme,
    compression: CompressionType,
    num_ticks_to_sign: u32,
    tick_writer: ReplayTickWriter,
    backend: Box<dyn FileWriterBackend>,
    listeners: Vec<Rc<RefCell<dyn ReplayWriterListener>>>,
    path: String,
    is_finalising: bool,
    is_closed: bool,
    released_writer: bool,
    removed_from_active: bool,
    last_chunk_position: u64,
    last_chunk_length: usize,
    num_ticks_written: u32,
    /// The write cursor, the file position the next queued chunk starts at.
    write_cursor: u64,
    queued_writes: Vec<QueuedWrite>,
}

impl fmt::Debug for ReplayFileWriter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReplayFileWriter")
            .field("path", &self.path)
            .field("is_finalising", &self.is_finalising)
            .field("is_closed", &self.is_closed)
            .field("removed_from_active", &self.removed_from_active)
            .field("num_ticks_written", &self.num_ticks_written)
            .finish()
    }
}

impl ReplayFileWriter {
    /// The new-file constructor (`replay_data_file_writer.cpp:44`): write the
    /// header and meta-data prefix through the background writer, priming the
    /// tick chunker off the meta-data signature.
    pub fn new(
        scheme: ReplayChecksumScheme,
        backend: Box<dyn FileWriterBackend>,
        compression: CompressionType,
        header: ReplayHeader,
        num_ticks_to_sign: u32,
        meta_data: ReplayMetaData,
    ) -> Self {
        let path = backend.path().to_string();

        let mut out = BinaryStream::new();
        let prefix = write_replay_prefix(&scheme, &header, &meta_data, &mut out);
        let prefix_bytes = out.as_bytes().to_vec();

        let tick_writer = ReplayTickWriter::new(
            scheme,
            compression,
            num_ticks_to_sign,
            prefix.meta_data_signature,
        );

        let mut writer = ReplayFileWriter {
            scheme,
            compression,
            num_ticks_to_sign,
            tick_writer,
            backend,
            listeners: Vec::new(),
            path,
            is_finalising: false,
            is_closed: false,
            released_writer: false,
            removed_from_active: false,
            last_chunk_position: 0,
            last_chunk_length: 0,
            num_ticks_written: 0,
            write_cursor: prefix_bytes.len() as u64,
            queued_writes: Vec::new(),
        };

        // `commonInit`: the writer is the background writer's listener
        // (`replay_data_file_writer.cpp:145`).
        writer.backend.set_listener(true);
        // The header+meta-data prefix write (`replay_data_file_writer.cpp:90`).
        let prefix_length = prefix_bytes.len();
        writer.backend.queue_write(prefix_bytes, 0);
        writer.queued_writes.push(QueuedWrite {
            position: 0,
            length: prefix_length,
            user_data: 0,
        });
        writer
    }

    /// Register a listener (`addListener`, `replay_data_file_writer.cpp:432`).
    pub fn add_listener(&mut self, listener: Rc<RefCell<dyn ReplayWriterListener>>) {
        self.listeners.push(listener);
    }

    /// `path()`.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// `isFinalising()`.
    pub fn is_finalising(&self) -> bool {
        self.is_finalising
    }

    /// `closeWriter` has been called (`closeWriter`, `:566`).
    pub fn is_closed(&self) -> bool {
        self.is_closed
    }

    /// The writer has been released and left the active set.
    pub fn removed_from_active(&self) -> bool {
        self.removed_from_active
    }

    /// Whether the background writer's completion listener is attached
    /// (`closeWriter` detaches it, `replay_data_file_writer.cpp:569`).
    pub fn listener_attached(&self) -> bool {
        self.backend.listener_attached()
    }

    /// The background writer has been released (`pWriter_ = NULL`,
    /// `replay_data_file_writer.cpp:575`). On failure it is kept, so the error
    /// string can still be read.
    pub fn writer_released(&self) -> bool {
        self.released_writer
    }

    /// `hasError()` (`:588`).
    pub fn has_error(&self) -> bool {
        self.backend.has_error()
    }

    /// The background writer's error string (`:595`).
    pub fn error_string(&self) -> String {
        self.backend.error_string()
    }

    /// `numTicksWritten_`.
    pub fn num_ticks_written(&self) -> u32 {
        self.num_ticks_written
    }

    /// `lastTickWritten_`.
    pub fn last_tick_written(&self) -> u32 {
        self.tick_writer.last_tick_written()
    }

    /// The operations queued so far, for asserting the handover sequence.
    pub fn queued_operations(&self) -> Vec<FileOp> {
        self.backend.queued_operations()
    }

    /// `ReplayDataFileWriter::addTickData` (`replay_data_file_writer.cpp:179`):
    /// buffer a cell's data for `tick`, then queue any chunk the completed
    /// tick released.
    pub fn add_tick_data(&mut self, tick: u32, num_cells: u32, blob: &[u8]) {
        self.tick_writer.add_tick_data(tick, num_cells, blob);
        self.queue_new_chunks();
    }

    /// `close(shouldFinalise)` (`replay_data_file_writer.cpp:520`).
    pub fn close(&mut self, should_finalise: bool) {
        if should_finalise {
            self.finalise();
        } else {
            self.close_writer();
        }
    }

    /// `finalise()` (`replay_data_file_writer.cpp:534`): sign the last chunk,
    /// then queue the header tick-count rewrite and the read-only chmod.
    pub fn finalise(&mut self) {
        self.is_finalising = true;
        self.tick_writer.finalise();
        self.queue_new_chunks();

        let num_ticks_to_write = self.num_ticks_written
            + (self.tick_writer.last_tick_pending_write() - self.tick_writer.last_tick_written());
        let offset = ReplayHeader::num_ticks_field_offset(&self.scheme);
        self.backend.queue_seek(offset);
        self.backend
            .queue_write_blob(num_ticks_to_write.to_le_bytes().to_vec());
        self.backend.queue_chmod(READ_ONLY_MODE);

        // The trailing operations each complete with a zero user data, and
        // the first of them closes the writer when the chunks are already on
        // disk (`background_file_writer.cpp:838`).
        let file_end = self.write_cursor;
        self.queued_writes.push(QueuedWrite {
            position: file_end,
            length: 0,
            user_data: 0,
        });
        self.queued_writes.push(QueuedWrite {
            position: offset,
            length: GAME_TIME_SIZE,
            user_data: 0,
        });
        self.queued_writes.push(QueuedWrite {
            position: offset + GAME_TIME_SIZE as u64,
            length: 0,
            user_data: 0,
        });
    }

    /// `closeWriter()` (`replay_data_file_writer.cpp:566`): detach the
    /// listener, release the writer unless it is in error, and leave the
    /// active set.
    pub fn close_writer(&mut self) {
        self.is_finalising = true;
        self.is_closed = true;
        self.backend.set_listener(false);
        if !self.backend.has_error() {
            self.released_writer = true;
        }
        self.removed_from_active = true;
    }

    /// `onBackgroundFileWritingComplete` (`replay_data_file_writer.cpp:460`):
    /// a queued write reached the file. A chunk write releases its ticks;
    /// when finalising and the tick data has all been handed over, the writer
    /// closes; every write completion notifies the listeners.
    pub fn on_background_file_writing_complete(&mut self, file_position: u64, user_data: u32) {
        if user_data != 0 {
            self.tick_writer.on_chunk_write_complete(user_data);
            self.num_ticks_written = self.tick_writer.num_ticks_written();
            self.last_chunk_position = file_position;
            self.last_chunk_length = self
                .queued_writes
                .iter()
                .find(|q| q.position == file_position)
                .map(|q| q.length)
                .unwrap_or_default();
        }
        self.queued_writes.retain(|q| q.position != file_position);

        let have_more_tick_data = self.tick_writer.has_more_tick_data_to_write();
        if self.is_finalising && !have_more_tick_data {
            self.close_writer();
        }
        self.notify_listeners_of_completion();
    }

    /// `onBackgroundFileWritingError` (`replay_data_file_writer.cpp:454`):
    /// report the failure to the listeners and leave the active set.
    pub fn on_background_file_writing_error(&mut self) {
        self.removed_from_active = true;
        self.notify_listeners_of_error();
    }

    /// `recoveryData()` (`replay_data_file_writer.cpp:727`).
    pub fn recovery_data(&self) -> RecoveryData {
        RecoveryData {
            compression: self.compression,
            num_ticks: self.num_ticks_written,
            last_tick_written: self.tick_writer.last_tick_written(),
            next_chunk_position: self.last_chunk_position + self.last_chunk_length as u64,
            last_signature: self.tick_writer.last_signature().to_vec(),
            num_ticks_to_sign: self.num_ticks_to_sign,
        }
    }

    /// The queued writes in execution order, as the background file writer
    /// would complete them: `(file_position, user_data)` for each, the file
    /// position being where the write begins.
    pub fn queued_writes(&self) -> Vec<(u64, u32)> {
        self.queued_writes
            .iter()
            .map(|q| (q.position, q.user_data))
            .collect()
    }

    /// Hand the tick chunker's freshly signed chunks to the background writer,
    /// recording where each one begins.
    fn queue_new_chunks(&mut self) {
        for (chunk, user_data) in self.tick_writer.take_chunks() {
            let position = self.write_cursor;
            self.write_cursor += chunk.len() as u64;
            self.queued_writes.push(QueuedWrite {
                position,
                length: chunk.len(),
                user_data,
            });
            self.backend.queue_write(chunk, user_data);
        }
    }

    fn notify_listeners_of_completion(&self) {
        for listener in &self.listeners {
            listener.borrow_mut().on_writing_complete(&self.path);
        }
    }

    fn notify_listeners_of_error(&self) {
        for listener in &self.listeners {
            listener.borrow_mut().on_writing_error(&self.path);
        }
    }

    fn notify_listeners_of_destruction(&self) {
        for listener in &self.listeners {
            listener.borrow_mut().on_writer_destroyed(&self.path);
        }
    }
}

impl Drop for ReplayFileWriter {
    /// Destructor (`replay_data_file_writer.cpp:160`): detach the listener and
    /// notify destruction.
    fn drop(&mut self) {
        self.backend.set_listener(false);
        self.notify_listeners_of_destruction();
    }
}

/// The active-writer registry (`s_activeWriters_`): one writer per path, so a
/// second recording of the same path is rejected and shutdown can close them
/// all (`closeAll`, `replay_data_file_writer.cpp:610`).
#[derive(Debug, Default)]
pub struct WriterSet {
    writers: Vec<ReplayFileWriter>,
}

impl WriterSet {
    /// An empty set.
    pub fn new() -> Self {
        WriterSet::default()
    }

    /// `existsForPath` (`replay_data_file_writer.cpp:601`).
    pub fn exists_for_path(&self, path: &str) -> bool {
        self.writers
            .iter()
            .any(|w| w.path() == path && !w.removed_from_active())
    }

    /// `haveAllClosed` (`replay_data_file_writer.cpp:631`): no writer is still
    /// active.
    pub fn have_all_closed(&self) -> bool {
        self.writers.iter().all(|w| w.removed_from_active())
    }

    /// `closeAll` (`replay_data_file_writer.cpp:610`): register `listener` on
    /// every writer, then close the ones not yet finalising.
    pub fn close_all(
        &mut self,
        listener: &Rc<RefCell<dyn ReplayWriterListener>>,
        should_finalise: bool,
    ) {
        for writer in &mut self.writers {
            writer.add_listener(listener.clone());
            if !writer.is_finalising() {
                writer.close(should_finalise);
            }
        }
    }

    /// The active writers.
    pub fn writers(&self) -> &[ReplayFileWriter] {
        &self.writers
    }

    /// The writer at `index`, if any.
    pub fn writer_mut(&mut self, index: usize) -> Option<&mut ReplayFileWriter> {
        self.writers.get_mut(index)
    }

    /// Register a writer (`commonInit`, `replay_data_file_writer.cpp:143`).
    pub fn insert(&mut self, writer: ReplayFileWriter) {
        self.writers.push(writer);
    }

    /// Run each active writer's background writes to completion, the way the
    /// `BackgroundFileWriter` would (`background_file_writer.cpp:838`). A
    /// writer's listener detaches when it closes, so its callbacks stop firing
    /// from there on.
    pub fn drain_background_writes(&mut self) {
        for writer in &mut self.writers {
            loop {
                if !writer.listener_attached() {
                    break;
                }
                let Some((position, user_data)) = writer.queued_writes().first().copied() else {
                    break;
                };
                writer.on_background_file_writing_complete(position, user_data);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::replay_header::NONCE_SIZE;

    fn digest() -> crate::replay_header::Digest {
        [0xAA; 16]
    }

    fn nonce() -> [u8; NONCE_SIZE] {
        [1, 2, 3, 4, 5, 6, 7, 8]
    }

    fn meta_data() -> ReplayMetaData {
        let mut meta = ReplayMetaData::new();
        meta.add("server", "baseapp");
        meta
    }

    fn header() -> ReplayHeader {
        ReplayHeader::new(&scheme(), digest(), 20, 1_700_000_000, nonce())
    }

    fn scheme() -> ReplayChecksumScheme {
        ReplayChecksumScheme::new(8)
    }

    fn writer(path: &str) -> ReplayFileWriter {
        ReplayFileWriter::new(
            scheme(),
            Box::new(RecordingFileWriter::new(path)),
            CompressionType::None,
            header(),
            3,
            meta_data(),
        )
    }

    fn counter() -> Rc<RefCell<CallbackCounter>> {
        Rc::new(RefCell::new(CallbackCounter::default()))
    }

    /// Read back a queued operation (`ops`, `replay_data_file_writer.cpp:544`).
    fn ops(writer: &ReplayFileWriter) -> Vec<FileOp> {
        writer.queued_operations()
    }

    /// Drive the background file writer to completion, the way it would: each
    /// queued write fires the completion callback, and once the listener is
    /// detached no further callbacks arrive
    /// (`background_file_writer.cpp:840`).
    fn drive(writer: &mut ReplayFileWriter) {
        for (position, user_data) in writer.queued_writes() {
            if !writer.listener_attached() {
                break;
            }
            writer.on_background_file_writing_complete(position, user_data);
        }
    }

    fn write_payload(op: &FileOp) -> Option<&[u8]> {
        match op {
            FileOp::Write { data, .. } | FileOp::WriteBlob { data } => Some(data),
            _ => None,
        }
    }

    /// A single chunk of three ticks.
    fn fill(writer: &mut ReplayFileWriter) {
        for tick in 1..=3u32 {
            writer.add_tick_data(tick, 1, &[tick as u8]);
        }
    }

    /// Finalising rewrites the header's tick count and chmods the file
    /// read-only, at the offset of the count field.
    #[test]
    fn a_finalise_rewrites_the_tick_count_and_chmods_read_only() {
        let mut writer = writer("replay.rp");
        writer.add_listener(counter());
        fill(&mut writer);
        writer.finalise();

        let queued = ops(&writer);
        assert_eq!(queued.len(), 5, "prefix, chunk, seek, count, chmod");
        assert!(write_payload(&queued[0]).is_some(), "the header prefix");
        assert!(write_payload(&queued[1]).is_some(), "the tick chunk");

        let offset = match &queued[2] {
            FileOp::Seek { offset } => *offset,
            _ => panic!("expected the tick-count seek"),
        };
        assert_eq!(
            offset,
            4 + 16 + 1 + 4 + NONCE_SIZE as u64 + 2 + 8,
            "the header minus the count field"
        );

        assert_eq!(
            write_payload(&queued[3]).unwrap(),
            &3u32.to_le_bytes(),
            "the count of ticks written out"
        );
        assert_eq!(
            queued[4],
            FileOp::Chmod {
                mode: READ_ONLY_MODE
            }
        );
    }

    /// A write completion fires per queued write, and the last chunk write
    /// detaches the listener so the header rewrite never fires one.
    #[test]
    fn completions_fire_per_write_and_stop_when_the_listener_detaches() {
        let listener = counter();
        let mut writer = writer("replay.rp");
        writer.add_listener(listener.clone());
        fill(&mut writer);
        writer.finalise();

        drive(&mut writer);

        let counts = listener.borrow();
        assert_eq!(counts.complete, 2, "the prefix and the chunk");
        assert_eq!(counts.error, 0);
        assert_eq!(counts.destroyed, 0);
        assert!(writer.is_closed());
        assert!(!writer.listener_attached());
    }

    /// Chunks that already reached the file leave the close to the header
    /// rewrite completion, and the chmod never fires a callback.
    #[test]
    fn chunks_written_before_finalise_close_on_the_header_rewrite() {
        let listener = counter();
        let mut writer = writer("replay.rp");
        writer.add_listener(listener.clone());
        fill(&mut writer);

        drive(&mut writer);
        assert!(!writer.is_closed(), "not finalising yet");

        writer.finalise();
        drive(&mut writer);

        let counts = listener.borrow();
        assert_eq!(counts.complete, 3, "prefix, chunk and the first rewrite op");
        assert_eq!(counts.error, 0);
        assert!(writer.is_closed());
        assert!(!writer.listener_attached());
    }

    /// Closing without finalising never touches the header or the permissions.
    #[test]
    fn a_close_without_finalise_leaves_the_header_alone() {
        let listener = counter();
        let mut writer = writer("replay.rp");
        writer.add_listener(listener.clone());

        writer.close(false);

        let queued = ops(&writer);
        assert_eq!(queued.len(), 1, "only the prefix was ever queued");
        assert!(matches!(queued[0], FileOp::Write { user_data: 0, .. }));
        assert!(writer.is_closed());
        assert!(writer.writer_released());
        assert!(!writer.listener_attached());
        assert_eq!(listener.borrow().complete, 0);
    }

    /// A write failure notifies once and keeps the writer around for
    /// inspection, so the error string can still be read.
    #[test]
    fn a_write_error_is_reported_once_and_keeps_the_writer() {
        let listener = counter();
        let backend = RecordingFileWriter::new("replay.rp");
        // The error rides into the writer before it is boxed.
        let failing = {
            let mut b = backend;
            b.set_error("no space left on device");
            Box::<RecordingFileWriter>::new(b)
        };
        let mut writer = ReplayFileWriter::new(
            scheme(),
            failing,
            CompressionType::None,
            header(),
            3,
            meta_data(),
        );
        writer.add_listener(listener.clone());

        writer.on_background_file_writing_error();

        let counts = listener.borrow();
        assert_eq!(counts.error, 1);
        assert_eq!(counts.complete, 0);
        assert!(writer.removed_from_active(), "the failure leaves the set");
        assert!(!writer.is_closed(), "the writer is not closed by a failure");
        assert!(
            !writer.writer_released(),
            "a failed writer is kept for inspection"
        );
        assert_eq!(writer.error_string(), "no space left on device");
    }

    /// The last chunk's signature and the position of the next tick make up the
    /// recovery data.
    #[test]
    fn the_recovery_data_reports_where_the_next_chunk_starts() {
        let mut writer = writer("replay.rp");
        fill(&mut writer);
        writer.finalise();
        drive(&mut writer);

        let queued = ops(&writer);
        let prefix_length = write_payload(&queued[0]).unwrap().len() as u64;
        let chunk = write_payload(&queued[1]).unwrap();
        let chunk_length = chunk.len() as u64;

        let recovery = writer.recovery_data();
        assert_eq!(recovery.compression, CompressionType::None);
        assert_eq!(recovery.num_ticks, 3);
        assert_eq!(recovery.last_tick_written, 3);
        assert_eq!(
            recovery.next_chunk_position,
            prefix_length + chunk_length,
            "past the chunk just written"
        );
        assert_eq!(recovery.num_ticks_to_sign, 3);
        assert_eq!(recovery.last_signature, chunk[chunk.len() - 8..]);
    }

    /// Destruction notifies the listeners once, no matter how the writer
    /// reached the end of its life.
    #[test]
    fn destruction_notifies_the_listeners_once() {
        let listener = counter();
        {
            let mut writer = writer("replay.rp");
            writer.add_listener(listener.clone());
            fill(&mut writer);
            writer.finalise();
            drive(&mut writer);
        }

        let counts = listener.borrow();
        assert_eq!(counts.destroyed, 1);
        assert_eq!(counts.complete, 2);
        assert_eq!(counts.error, 0);
    }

    /// Closing everything finalises each active writer and one listener sees
    /// every event, including from the writer that was already finalising.
    #[test]
    fn close_all_finalises_each_active_writer() {
        let counted = counter();
        let listener: Rc<RefCell<dyn ReplayWriterListener>> = Rc::clone(&counted) as _;
        let mut first = writer("/replays/a.rp");
        first.close(true);
        assert!(first.is_finalising());
        let mut second = writer("/replays/b.rp");
        fill(&mut second);

        let mut set = WriterSet::new();
        set.insert(first);
        set.insert(second);
        assert!(!set.have_all_closed());

        set.close_all(&listener, true);
        assert!(set.writers()[1].is_finalising());
        set.drain_background_writes();

        assert!(set.have_all_closed());
        assert!(!set.exists_for_path("/replays/a.rp"));
        // The listener sees the prefix plus each chunk from both writers.
        assert_eq!(counted.borrow().complete, 3, "prefix a, prefix b, chunk b");
        assert_eq!(counted.borrow().error, 0);
    }

    /// A path admits only one active writer at a time.
    #[test]
    fn a_path_admits_only_one_active_writer() {
        let mut set = WriterSet::new();
        set.insert(writer("replay.rp"));
        assert!(set.exists_for_path("replay.rp"));
        assert!(!set.exists_for_path("other.rp"));

        if let Some(writer) = set.writer_mut(0) {
            writer.add_listener(counter());
            writer.close(true);
        }
        set.drain_background_writes();

        assert!(!set.exists_for_path("replay.rp"));
    }
}
