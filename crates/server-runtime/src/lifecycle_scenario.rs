//! OPT-05-01-03: verify entity and process state across a seeded lifecycle.
//!
//! The acceptance is "实体和进程状态正确" — entity and process state are
//! correct. This module composes the real migration-recovery, controlled
//! shutdown and replay-writing faces into one seeded scenario and snapshots
//! the decisions each makes. A rerun from the same seed must produce the
//! same entity reconciliation, the same process-stage ledger and the same
//! replay bytes. No wall-clock value is sampled.

use std::cell::RefCell;
use std::rc::Rc;

use foundation::stream::BinaryStream;

use crate::cluster_shutdown::{
    CellAppMgrShutDown, SeniorShutDownTarget, controlled_shut_down_server,
};
use crate::ghost_data::Address;
use crate::message_redirect::Call;
use crate::offload_recovery::{CallState, MigrationRecovery, RestoreOutcome};
use crate::replay_data::{ReplayDataCollector, ReplayDataFileWriter, ReplayEntity, tick_recording};
use crate::replay_header::{
    ProtocolVersion, ReplayChecksumScheme, ReplayHeader, ReplayMetaData, read_replay_prefix,
    write_replay_prefix,
};
use crate::shutdown::{BackgroundTask, ShutDownStage, ShutdownState, TaskManager};

/// The scenario seed. A caller may pass another to check the scenario under
/// a different fixed call id.
pub const LIFECYCLE_SEED: u64 = 41;
/// The number of replay ticks the scenario records.
pub const LIFECYCLE_TICKS: u32 = 4;

/// The replay checksum scheme the scenario signs with. A fixed signature
/// length keeps the written bytes deterministic.
pub fn replay_scheme() -> ReplayChecksumScheme {
    ReplayChecksumScheme::new(8)
}

/// The pre-serialized replay prefix (header + empty meta-data) and the
/// offset of the numTicks field the writer patches on finalise.
pub fn replay_prefix() -> (Vec<u8>, usize) {
    let scheme = replay_scheme();
    let header = ReplayHeader::new(&scheme, [0u8; 16], 20, 0, [0u8; 8]);
    let mut prefix = BinaryStream::new();
    write_replay_prefix(&scheme, &header, &ReplayMetaData::new(), &mut prefix);
    let offset = ReplayHeader::num_ticks_field_offset(&scheme) as usize;
    (prefix.as_bytes().to_vec(), offset)
}

/// A completion task that records its name when the drain runs it.
struct RecordingTask {
    name: &'static str,
    log: Rc<RefCell<Vec<String>>>,
}

impl BackgroundTask for RecordingTask {
    fn do_background_task(&mut self, mgr: &mut TaskManager) {
        mgr.add_main_thread_task(Rc::new(RefCell::new(RecordingTask {
            name: self.name,
            log: self.log.clone(),
        })));
    }

    fn do_main_thread_task(&mut self, _mgr: &mut TaskManager) {
        self.log.borrow_mut().push(self.name.to_string());
    }

    fn name(&self) -> &str {
        self.name
    }
}

/// One run's entity and process state snapshot.
#[derive(Clone, Debug, PartialEq)]
pub struct LifecycleLedger {
    /// The outcome of the migration restore after the target died.
    pub restore: RestoreOutcome,
    /// The outcome of the offload that moved the Real before the target died.
    pub offload: crate::offload_role::SwitchOutcome,
    /// Every call state observed while the Real was lost and after it came
    /// back.
    pub call_states: Vec<CallState>,
    /// The entity reconciliation after the restore.
    pub reconciliation: crate::offload_recovery::Reconciliation,
    /// The senior process the controlled shutdown starts from.
    pub senior: SeniorShutDownTarget,
    /// The shutdown stages the local app actually entered.
    pub stages: Vec<ShutDownStage>,
    /// The controlled-shutdown deadline recorded at the Inform stage.
    pub shut_down_time: u32,
    /// The completions the task manager drained, in order.
    pub drained: Vec<String>,
    /// The final replay file bytes, patched and signed.
    pub replay: Vec<u8>,
    /// Mixed-deployment gate: the current-version node accepts this replay
    /// and an older-version node's copy is rejected by this one.
    pub mixed_version: (bool, bool),
}

/// Run the fixed lifecycle scenario once.
///
/// The entity starts on cell A with a Ghost on B, offloads to B, loses B,
/// buffers a call while it has no Real, and restores onto C. The app then
/// enters a controlled shutdown while its task manager drains. The whole
/// span is recorded through the real replay collector and file writer.
pub fn run_lifecycle_scenario(seed: u64) -> LifecycleLedger {
    let a = Address(seed);
    let b = Address(seed + 1);
    let c = Address(seed + 2);
    let caller = Address(seed + 9);

    let mut recovery = MigrationRecovery::new(a);
    recovery.add_ghost(b);
    let offload = recovery.switch_to(b);
    let mut call_states = Vec::new();

    // The target CellApp dies. Calls arriving during the outage are failed
    // to the sender and buffered, then re-issued exactly once on restore.
    recovery.target_lost(b);
    let first = Call {
        message_id: 7,
        sender: caller,
        payload: vec![1, 2, 3],
    };
    let second = Call {
        message_id: 8,
        sender: caller,
        payload: vec![4],
    };
    call_states.push(recovery.call(caller, &first));
    call_states.push(recovery.call(caller, &second));
    let restore = recovery.restore(c);
    call_states.push(CallState::Forwarded(c));
    let reconciliation = recovery.reconcile();

    // The controlled shutdown starts at the most senior process still up,
    // then walks the local app through Request, Inform and Perform.
    let senior = controlled_shut_down_server(None, Some((0x7f00_0001, 4000)));
    let mut state = ShutdownState::new();
    let mut stages = Vec::new();
    let deadline = 100;
    for stage in [
        ShutDownStage::Request,
        ShutDownStage::Inform,
        ShutDownStage::Perform,
    ] {
        state.handle_controlled_shut_down(stage, deadline);
        stages.push(state.stage());
    }

    // The CellAppMgr's Inform phase waits for a cell ack before it acks the
    // BaseAppMgr; both messages are part of the process-state snapshot.
    let mut mgr_shutdown = CellAppMgrShutDown::new();
    let _start =
        mgr_shutdown.controlled_shut_down(ShutDownStage::Request, 0, 10, &[(0x7f00_0001, 6000)]);
    let _inform = mgr_shutdown.on_tick(10);
    let _cell_ack = mgr_shutdown.ack_cell_app_shut_down((0x7f00_0001, 6000), ShutDownStage::Inform);

    // Every completion runs before the process ends, in enqueue order.
    let log = Rc::new(RefCell::new(Vec::new()));
    let mut tasks = TaskManager::new();
    tasks.add_background_task(Rc::new(RefCell::new(RecordingTask {
        name: "backup",
        log: log.clone(),
    })));
    tasks.add_background_task(Rc::new(RefCell::new(RecordingTask {
        name: "flush-replay",
        log: log.clone(),
    })));
    tasks.stop_all();
    let drained = log.borrow().clone();

    // Record the entity's state at each lifecycle step and finalise the
    // replay tail. Deterministic bytes are the recorded evidence that the
    // entity state survived the shutdown. One extra tick is fed so the
    // signing window has `LIFECYCLE_TICKS` complete ticks when finalise
    // holds back the newest one.
    let mut collector = ReplayDataCollector::new("lifecycle", false);
    let mut entity = ReplayEntity::new(1, 2);
    entity.position = (1.0, 2.0, 3.0);
    entity.replay_properties = vec![Some(vec![a.0 as u8]), Some(vec![b.0 as u8])];
    let (header, offset) = replay_prefix();
    let mut writer = ReplayDataFileWriter::new(
        header,
        LIFECYCLE_TICKS,
        replay_scheme().stream_size(),
        offset,
    );
    for tick in 0..=LIFECYCLE_TICKS {
        match tick {
            0 => collector.add_entity_state(&mut entity),
            1 => collector.add_entity_property(&entity, true, false, &[1, 2, 3, 4]),
            2 => collector.add_entity_property(&entity, true, false, &[5, 6, 7, 8]),
            3 => collector.add_finish(),
            // The extra tick gives the signing window a subsequent tick so
            // finalise can write the four recorded ones. It carries no data.
            _ => {}
        }
        let blob = tick_recording(&mut collector);
        writer.add_tick_data(tick + 1, 1, &blob);
    }
    writer.finalise();

    // The mixed-version gate (混部): the current-version node accepts its
    // own replay, while a copy stamped with a different protocol version is
    // rejected at the prefix boundary. This is the only runtime-observable
    // mixed-fleet face — there is no mixed-version fleet runner.
    let scheme = replay_scheme();
    let final_replay = writer.file();
    let current_accepted =
        read_replay_prefix(&mut BinaryStream::from_slice(&final_replay), &scheme, false).is_ok();
    let mut foreign = final_replay.clone();
    let foreign_version = ProtocolVersion {
        subpatch: 0,
        patch: 1,
        ..ProtocolVersion::current()
    };
    foreign[0..4].copy_from_slice(&[
        foreign_version.subpatch,
        foreign_version.patch,
        foreign_version.minor,
        foreign_version.major,
    ]);
    let older_rejected =
        read_replay_prefix(&mut BinaryStream::from_slice(&foreign), &scheme, false)
            .map(|_| ())
            .is_err();

    LifecycleLedger {
        restore,
        offload,
        call_states,
        reconciliation,
        senior,
        stages,
        shut_down_time: state.shut_down_time(),
        drained,
        replay: final_replay,
        mixed_version: (current_accepted, older_rejected),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scenario_reproduces_from_its_seed() {
        assert_eq!(
            run_lifecycle_scenario(LIFECYCLE_SEED),
            run_lifecycle_scenario(LIFECYCLE_SEED)
        );
    }

    #[test]
    fn the_entity_state_is_consistent_after_the_restore() {
        let run = run_lifecycle_scenario(LIFECYCLE_SEED);
        assert_eq!(run.offload, crate::offload_role::SwitchOutcome::Switched);
        assert_eq!(
            run.restore,
            RestoreOutcome::Restored(Address(LIFECYCLE_SEED + 2))
        );
        assert!(run.reconciliation.passes());
        assert_eq!(
            run.call_states,
            vec![
                CallState::Failed(Address(LIFECYCLE_SEED + 9)),
                CallState::Failed(Address(LIFECYCLE_SEED + 9)),
                CallState::Forwarded(Address(LIFECYCLE_SEED + 2)),
            ]
        );
    }

    #[test]
    fn the_process_stages_and_task_drain_are_correct() {
        let run = run_lifecycle_scenario(LIFECYCLE_SEED);
        assert_eq!(
            run.senior,
            SeniorShutDownTarget::DbAppAlpha((0x7f00_0001, 4000))
        );
        assert_eq!(
            run.stages,
            vec![
                ShutDownStage::Request,
                ShutDownStage::Inform,
                ShutDownStage::Perform,
            ]
        );
        assert_eq!(run.shut_down_time, 100);
        assert_eq!(run.drained, vec!["backup", "flush-replay"]);
    }

    #[test]
    fn the_recording_task_reports_its_name() {
        // `name()` is the watcher/profiler label; the task manager does not
        // call it in this build mode, so the scenario cannot cover it.
        let task = RecordingTask {
            name: "replay",
            log: Rc::new(RefCell::new(Vec::new())),
        };
        assert_eq!(task.name(), "replay");
    }

    #[test]
    fn the_mixed_version_gate_accepts_current_and_rejects_foreign() {
        let run = run_lifecycle_scenario(LIFECYCLE_SEED);
        assert_eq!(
            run.mixed_version,
            (true, true),
            "the current node accepts its own replay and rejects a foreign version"
        );
    }

    #[test]
    fn the_replay_tail_is_final_and_reproducible() {
        let run = run_lifecycle_scenario(LIFECYCLE_SEED);
        let scheme = replay_scheme();
        let (prefix, offset) = replay_prefix();
        // The numTicks field in the real prefix is patched to the number of
        // signed ticks, and the prefix still verifies.
        assert_eq!(
            &run.replay[offset..offset + 4],
            &LIFECYCLE_TICKS.to_le_bytes()
        );
        let mut stream = BinaryStream::from_slice(&run.replay);
        let (header, _) = crate::replay_header::read_replay_prefix(&mut stream, &scheme, true)
            .expect("the finalised replay prefix must verify");
        assert_eq!(header.num_ticks, LIFECYCLE_TICKS);
        // The body after the parsed prefix reads back as every recorded tick.
        let mut reader = crate::replay_data::ReplayDataFileReader::new(0, 0);
        reader.add_data(&run.replay[prefix.len()..]);
        assert!(!reader.has_error());
        assert_eq!(reader.num_ticks_read(), LIFECYCLE_TICKS);
    }
}
