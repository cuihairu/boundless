//! The reload action sequence, as data.
//!
//! Reference: `server/cellapp/reload_script_plan.hpp` (the `planReload` /
//! `digestMatches` pair) and its unit tests
//! `server/cellapp/unit_test/test_reload_script_plan.cpp`.
//!
//! `CellApp::reloadScript` hard-codes an ordered sequence of
//! interpreter/entity-def/UDO/mailbox/entity steps whose shape depends on
//! facts only known while it runs. The branch policy and ordering are the
//! part that has historically gone wrong (the hot-reload productionization
//! design, `docs/upgrade-plan/hot-reload-productionization.md`, records them
//! as failure modes F1/F3/F8), so the reference carries the sequence as a
//! pure function the unit tests can pin down. The port does the same, and
//! keeps it in a module of its own so the ordering facts and the migrate
//! mechanics of `crate::reload` cannot drift apart silently.
//!
//! The pinned facts (each is what makes a failed reload leave the live
//! definitions callable and the scratch references released -- the guarantee
//! BW-03-04-05 is about):
//! - on success the scratch interpreter is destroyed BEFORE the migrate chain
//!   runs (`cellapp.cpp:2351` precedes `:2353-2362`);
//! - on failure the scratch interpreter is destroyed AFTER cleanup
//!   (`cellapp.cpp:2383` follows `:2382`);
//! - the old-scripts recovery (`EntityType::reloadScript(true)`,
//!   `cellapp.cpp:2379`) runs on every failure path except a failed full
//!   reload -- a full reload has no old scripts to fall back to;
//! - a failed full reload wipes the registered data types before it can fail
//!   (`EntityType::init(true)` calls `DataType::clearStaticsForReload()`,
//!   `entity_type.cpp:262`, before parsing; the failure path never reloads
//!   them, failure mode F8), reported as `data_types_wiped`;
//! - the request's `expectedDigest` is judged right after the defs load: a
//!   mismatch takes the failure tail, so no migrate step runs;
//! - a dry run stops straight after the loads: swap back and destroy the
//!   scratch interpreter, with no cleanup (which would break the live
//!   dual-version window), no migration and no old-script recovery.
//!
//! Deltas kept visible: the reference passes C strings that may be NULL;
//! the port passes `Option<&str>` (None = the empty constraint). The action
//! names and the shapes of `PlanInputs` / `ReloadPlan` are the reference's
//! in Rust spelling. The port's own runtime evidence that a failed reload
//! leaves the old definitions callable and releases the scratch references
//! lives in `interpreter.rs` (scenario) and `crate::reload` (the mechanism);
//! this module is the ordering the host will drive.

/// One step of `CellApp::reloadScript`, in execution order
/// (`reload_script_plan.hpp:30-62`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// `Script::createInterpreter` + `swapInterpreter`.
    SwapInInterpreter,
    /// `EntityType::init(true)` or `::reloadScript()`.
    LoadEntityDefs,
    /// `UserDataObjectType::load`.
    LoadUserDataObjects,
    /// Restore the original interpreter.
    SwapBackInterpreter,
    /// `Script::destroyInterpreter` on the scratch interpreter.
    DestroyNewInterpreter,
    /// `EntityType::migrate(isFullReload)`.
    MigrateEntityTypes,
    MigrateUserDataObjectTypes,
    /// `ServerEntityMailBox::migrateMailBoxes`.
    MigrateMailBoxes,
    /// The per-entity `Entity::migrate` pass under the collect-decide policy:
    /// stop at the first failure and roll the pass back
    /// (`EntityMigration::rollout`).
    MigrateEntities,
    /// The per-entity `Entity::migratedAll`, after a fully successful
    /// migration loop (failure mode F6: the `onMigratedAll` hook was
    /// declared but never called by anything in the tree).
    MigratedAll,
    /// `EntityType::cleanupAfterReload(isFullReload)`.
    CleanupAfterReload,
    /// `EntityType::reloadScript(true)`, failure only.
    RecoverOldScripts,
}

/// The fixed size of the plan's action array (`ACTION_COUNT`).
pub const ACTION_COUNT: usize = 12;

/// The outcomes the plan's shape depends on.
///
/// `digest_matched`: whether this load's schema digest agrees with the
/// request's `expectedDigest`; it defaults to true because a request
/// carrying no expectation places no constraint. `dry_run`: a probe load
/// that parses and reports without migrating anything; it defaults to false,
/// so a request that does not ask for one behaves as before.
#[derive(Debug, Clone, Copy)]
pub struct PlanInputs {
    pub is_full_reload: bool,
    pub defs_loaded: bool,
    pub udo_loaded: bool,
    pub digest_matched: bool,
    pub dry_run: bool,
}

impl PlanInputs {
    /// The three facts the pre-digest-barrier callers know; the barrier and
    /// the probe mode default to their pre-barrier values.
    pub fn new(is_full_reload: bool, defs_loaded: bool, udo_loaded: bool) -> Self {
        PlanInputs {
            is_full_reload,
            defs_loaded,
            udo_loaded,
            digest_matched: true,
            dry_run: false,
        }
    }
}

/// The ordered action sequence for one reload attempt.
pub struct ReloadPlan {
    pub actions: [Action; ACTION_COUNT],
    pub count: usize,
    /// Whether the attempt as a whole succeeds (both loads went through).
    pub succeeded: bool,
    /// Failure mode F8: a full reload clears the registered data types
    /// before it can fail, and nothing on the failure path reloads them.
    pub data_types_wiped: bool,
}

impl ReloadPlan {
    fn push(&mut self, action: Action) {
        self.actions[self.count] = action;
        self.count += 1;
    }

    /// The index of `action` in the plan, or -1 if it is not there. The
    /// test helper `indexOf` (`test_reload_script_plan.cpp:34-50`).
    pub fn index_of(&self, action: Action) -> i32 {
        self.actions[..self.count]
            .iter()
            .position(|candidate| *candidate == action)
            .map(|index| index as i32)
            .unwrap_or(-1)
    }

    /// The number of occurrences of `action` in the plan. The test helper
    /// `countOf` (`test_reload_script_plan.cpp:52-64`).
    pub fn count_of(&self, action: Action) -> usize {
        self.actions[..self.count]
            .iter()
            .filter(|candidate| **candidate == action)
            .count()
    }
}

/// `planReload` (`reload_script_plan.hpp:126-207`): the action sequence for
/// the given outcomes. `udo_loaded` is only consulted when the defs load
/// succeeded and the schema digest agrees -- the UDO load is never attempted
/// after either failure, so its result is not consulted on those paths. A
/// dry-run load stops after the loads and reports, whatever their result.
pub fn plan_reload(inputs: &PlanInputs) -> ReloadPlan {
    // A rejected schema digest is judged right after the defs load, so the
    // reload takes the failure path with zero migration. The full-reload
    // consequence is that the defs parse already cleared the registered data
    // types (failure mode F8), so `data_types_wiped` reflects a rejected
    // digest as well as a failed defs load.
    let defs_usable = inputs.defs_loaded && inputs.digest_matched;
    let mut plan = ReloadPlan {
        actions: [Action::SwapInInterpreter; ACTION_COUNT],
        count: 0,
        succeeded: defs_usable && inputs.udo_loaded,
        data_types_wiped: inputs.is_full_reload && (inputs.dry_run || !defs_usable),
    };
    // Head: the scratch interpreter is swapped in and the loads run inside it.
    plan.push(Action::SwapInInterpreter);
    plan.push(Action::LoadEntityDefs);
    if defs_usable {
        plan.push(Action::LoadUserDataObjects);
    }
    if inputs.dry_run {
        // A dry run stops after the loads: tear down the scratch interpreter
        // and report. No cleanup (that breaks the live old()/supersede window
        // the running entities still depend on), no migration, no recovery.
        plan.push(Action::SwapBackInterpreter);
        plan.push(Action::DestroyNewInterpreter);
    } else if defs_usable && inputs.udo_loaded {
        // Success tail: destroy the scratch interpreter before the migrate
        // chain, then clean up (`cellapp.cpp:2350-2364`).
        plan.push(Action::SwapBackInterpreter);
        plan.push(Action::DestroyNewInterpreter);
        plan.push(Action::MigrateEntityTypes);
        plan.push(Action::MigrateUserDataObjectTypes);
        plan.push(Action::MigrateMailBoxes);
        plan.push(Action::MigrateEntities);
        plan.push(Action::MigratedAll);
        plan.push(Action::CleanupAfterReload);
    } else {
        // Failure tail: restore, recover the old scripts (unless this was a
        // full reload -- it has no old scripts to fall back to), clean up,
        // and only then destroy the scratch interpreter
        // (`cellapp.cpp:2371-2383`).
        plan.push(Action::SwapBackInterpreter);
        if !inputs.is_full_reload {
            plan.push(Action::RecoverOldScripts);
        }
        plan.push(Action::CleanupAfterReload);
        plan.push(Action::DestroyNewInterpreter);
    }
    plan
}

/// `digestMatches` (`reload_script_plan.hpp:73-97`): whether an expected
/// schema digest agrees with this load's digest. An empty expected value
/// places no constraint (the caller asked for no barrier), so it always
/// agrees. The comparison is case-insensitive, because the digest travels as
/// a quoted MD5 hex string read out of a log or typed by hand.
pub fn digest_matches(expected: Option<&str>, actual: Option<&str>) -> bool {
    let expected = expected.unwrap_or("");
    let actual = actual.unwrap_or("");
    if expected.is_empty() {
        return true;
    }
    let expected = expected.as_bytes();
    let actual = actual.as_bytes();
    // Walk both strings until either terminator, so a shorter actual cannot
    // be read past its end.
    let mut index = 0;
    loop {
        match (expected.get(index), actual.get(index)) {
            (None, None) => return true,
            (None, Some(_)) | (Some(_), None) => return false,
            (Some(expected_char), Some(actual_char)) => {
                if expected_char.to_ascii_lowercase() != actual_char.to_ascii_lowercase() {
                    return false;
                }
            }
        }
        index += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A successful full reload runs the whole sequence: the scratch
    /// interpreter is swapped in, the loads run inside it, it is destroyed
    /// before the migrate chain, and cleanup runs last
    /// (`ReloadScriptPlan_FullReloadSucceeds`).
    #[test]
    fn full_reload_succeeds() {
        let plan = plan_reload(&PlanInputs::new(true, true, true));
        assert!(plan.succeeded);
        assert!(!plan.data_types_wiped);
        assert_eq!(11, plan.count);
        assert_eq!(Action::SwapInInterpreter, plan.actions[0]);
        assert_eq!(Action::LoadEntityDefs, plan.actions[1]);
        assert_eq!(Action::LoadUserDataObjects, plan.actions[2]);
        assert_eq!(Action::SwapBackInterpreter, plan.actions[3]);
        assert_eq!(Action::DestroyNewInterpreter, plan.actions[4]);
        assert_eq!(Action::MigrateEntityTypes, plan.actions[5]);
        assert_eq!(Action::MigrateUserDataObjectTypes, plan.actions[6]);
        assert_eq!(Action::MigrateMailBoxes, plan.actions[7]);
        assert_eq!(Action::MigrateEntities, plan.actions[8]);
        assert_eq!(Action::MigratedAll, plan.actions[9]);
        assert_eq!(Action::CleanupAfterReload, plan.actions[10]);
        // The ordering asymmetry that makes the migrate chain work.
        assert!(
            plan.index_of(Action::DestroyNewInterpreter)
                < plan.index_of(Action::MigrateEntityTypes)
        );
        assert_eq!(
            plan.count as i32 - 1,
            plan.index_of(Action::CleanupAfterReload)
        );
        assert_eq!(-1, plan.index_of(Action::RecoverOldScripts));
    }

    /// A scripts-only reload has the same success tail as a full reload
    /// (`ReloadScriptPlan_ScriptsOnlyReloadSucceeds`).
    #[test]
    fn scripts_only_reload_succeeds() {
        let full = plan_reload(&PlanInputs::new(true, true, true));
        let scripts_only = plan_reload(&PlanInputs::new(false, true, true));
        assert!(scripts_only.succeeded);
        assert_eq!(11, scripts_only.count);
        for index in 3..full.count {
            assert_eq!(full.actions[index], scripts_only.actions[index]);
        }
    }

    /// A failed full reload has no old scripts to recover from, and it wiped
    /// the registered data types before it failed (F8)
    /// (`ReloadScriptPlan_FullReloadDefsFail`).
    #[test]
    fn full_reload_defs_fail() {
        let plan = plan_reload(&PlanInputs::new(true, false, false));
        assert!(!plan.succeeded);
        assert!(plan.data_types_wiped);
        assert_eq!(5, plan.count);
        assert_eq!(Action::LoadEntityDefs, plan.actions[1]);
        assert_eq!(-1, plan.index_of(Action::LoadUserDataObjects));
        assert_eq!(-1, plan.index_of(Action::RecoverOldScripts));
        assert!(
            plan.index_of(Action::CleanupAfterReload)
                < plan.index_of(Action::DestroyNewInterpreter)
        );
        assert_eq!(
            plan.count as i32 - 1,
            plan.index_of(Action::DestroyNewInterpreter)
        );
    }

    /// A failed scripts-only reload recovers the old scripts between the
    /// swap back and the cleanup (`ReloadScriptPlan_ScriptsOnlyReloadDefsFail`).
    #[test]
    fn scripts_only_reload_defs_fail() {
        let plan = plan_reload(&PlanInputs::new(false, false, false));
        assert!(!plan.succeeded);
        assert!(!plan.data_types_wiped);
        assert_eq!(6, plan.count);
        assert_eq!(Action::SwapBackInterpreter, plan.actions[2]);
        assert_eq!(Action::RecoverOldScripts, plan.actions[3]);
        assert_eq!(Action::CleanupAfterReload, plan.actions[4]);
        assert_eq!(Action::DestroyNewInterpreter, plan.actions[5]);
    }

    /// A failed UDO load follows the failure path too: the migrate chain
    /// never runs (`ReloadScriptPlan_UdoLoadFails`).
    #[test]
    fn udo_load_fails() {
        for full in [true, false] {
            let plan = plan_reload(&PlanInputs::new(full, true, false));
            assert!(!plan.succeeded);
            assert!(!plan.data_types_wiped);
            assert_eq!(if full { 6 } else { 7 }, plan.count);
            assert_eq!(1, plan.count_of(Action::LoadUserDataObjects));
            assert!(plan.index_of(Action::LoadUserDataObjects) > 0);
            assert_eq!(-1, plan.index_of(Action::MigrateEntityTypes));
            assert_eq!(-1, plan.index_of(Action::MigrateUserDataObjectTypes));
            assert_eq!(-1, plan.index_of(Action::MigrateMailBoxes));
            assert_eq!(-1, plan.index_of(Action::MigrateEntities));
            assert!(
                plan.index_of(Action::CleanupAfterReload)
                    < plan.index_of(Action::DestroyNewInterpreter)
            );
        }
    }

    /// The UDO load is never attempted after a failed defs load, so its
    /// reported outcome is irrelevant on that path
    /// (`ReloadScriptPlan_UdoOutcomeIgnoredAfterDefsFailure`).
    #[test]
    fn udo_outcome_ignored_after_defs_failure() {
        let mut ignored_true = PlanInputs::new(true, false, true);
        let mut ignored_false = PlanInputs::new(true, false, false);
        ignored_true.digest_matched = false;
        ignored_false.digest_matched = false;
        let plan_true = plan_reload(&ignored_true);
        let plan_false = plan_reload(&ignored_false);
        assert_eq!(plan_false.count, plan_true.count);
        for index in 0..plan_true.count {
            assert_eq!(plan_false.actions[index], plan_true.actions[index]);
        }
        assert_eq!(plan_false.data_types_wiped, plan_true.data_types_wiped);
    }

    /// Every outcome combination fits the fixed plan array, and the whole
    /// matrix is one of the four shapes the sequence has
    /// (`ReloadScriptPlan_AllOutcomeCombinationsFit`).
    #[test]
    fn all_outcome_combinations_fit() {
        for full in [false, true] {
            for defs in [false, true] {
                for udo in [false, true] {
                    for digest in [false, true] {
                        for dry in [false, true] {
                            let mut inputs = PlanInputs::new(full, defs, udo);
                            inputs.digest_matched = digest;
                            inputs.dry_run = dry;
                            let plan = plan_reload(&inputs);
                            let defs_usable = defs && digest;
                            assert!(plan.count <= ACTION_COUNT);
                            assert_eq!(defs && udo && digest, plan.succeeded);
                            assert_eq!(full && (dry || !defs_usable), plan.data_types_wiped);
                            // head (2 + defs load) + dry-run tail (2) or
                            // success tail (8) or failure tail (3, plus the
                            // recovery on the scripts-only path).
                            let expected = 2
                                + (if defs_usable { 1 } else { 0 })
                                + if dry {
                                    2
                                } else if plan.succeeded {
                                    8
                                } else {
                                    3 + if full { 0 } else { 1 }
                                };
                            assert_eq!(expected, plan.count);
                        }
                    }
                }
            }
        }
    }

    /// The digest helper: an empty expectation places no constraint, the
    /// comparison is case-insensitive, and a shorter actual never agrees
    /// (`ReloadScriptPlan_DigestMatches`).
    #[test]
    fn digest_matches_helper() {
        let quoted = "0123456789ABCDEF0123456789ABCDEF";
        // No expectation agrees with anything, even with nothing loaded.
        assert!(digest_matches(Some(""), Some(quoted)));
        assert!(digest_matches(Some(""), Some("")));
        assert!(digest_matches(None, Some(quoted)));
        // The same digest in either case agrees.
        assert!(digest_matches(Some(quoted), Some(quoted)));
        assert!(digest_matches(
            Some("0123456789abcdef0123456789abcdef"),
            Some(quoted)
        ));
        // Anything else disagrees, including a shorter or longer actual.
        assert!(!digest_matches(
            Some("FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF"),
            Some(quoted)
        ));
        assert!(!digest_matches(Some(quoted), Some("")));
        assert!(!digest_matches(Some(quoted), None));
        assert!(!digest_matches(Some(quoted), Some(&quoted[1..])));
    }

    /// A load whose schema digest disagrees with the request takes the
    /// failure path: no migrate step runs, and on a full reload the
    /// registered data types were already wiped (F8)
    /// (`ReloadScriptPlan_DigestMismatchRunsNoMigrate`).
    #[test]
    fn digest_mismatch_runs_no_migrate() {
        let mut full = PlanInputs::new(true, true, true);
        full.digest_matched = false;
        let full_plan = plan_reload(&full);
        assert!(!full_plan.succeeded);
        assert!(full_plan.data_types_wiped);
        assert_eq!(5, full_plan.count);
        assert_eq!(-1, full_plan.index_of(Action::LoadUserDataObjects));
        assert_eq!(-1, full_plan.index_of(Action::MigrateEntityTypes));
        assert_eq!(-1, full_plan.index_of(Action::MigrateEntities));
        assert_eq!(-1, full_plan.index_of(Action::MigratedAll));
        assert_eq!(-1, full_plan.index_of(Action::RecoverOldScripts));
        assert_eq!(Action::CleanupAfterReload, full_plan.actions[3]);
        assert_eq!(Action::DestroyNewInterpreter, full_plan.actions[4]);

        let mut scripts_only = PlanInputs::new(false, true, true);
        scripts_only.digest_matched = false;
        let scripts_only_plan = plan_reload(&scripts_only);
        assert!(!scripts_only_plan.succeeded);
        assert!(!scripts_only_plan.data_types_wiped);
        assert_eq!(6, scripts_only_plan.count);
        assert_eq!(-1, scripts_only_plan.index_of(Action::LoadUserDataObjects));
        assert_eq!(-1, scripts_only_plan.index_of(Action::MigrateEntities));
        assert_eq!(Action::RecoverOldScripts, scripts_only_plan.actions[3]);
    }

    /// A load whose digest agrees is judged successful: the migrate chain
    /// follows, exactly as the pre-barrier plan
    /// (`ReloadScriptPlan_DigestMatchKeepsTheSuccessTail`).
    #[test]
    fn digest_match_keeps_the_success_tail() {
        let plan = plan_reload(&PlanInputs::new(true, true, true));
        assert!(plan.succeeded);
        assert_eq!(Action::LoadUserDataObjects, plan.actions[2]);
        assert_eq!(Action::DestroyNewInterpreter, plan.actions[4]);
        assert_eq!(Action::MigrateEntityTypes, plan.actions[5]);
    }

    /// A full dry run stops straight after the loads: swap back and destroy
    /// the scratch interpreter, with no cleanup, no migration and no
    /// old-script recovery (`ReloadScriptPlan_DryRunStopsAfterTheLoads`).
    #[test]
    fn dry_run_stops_after_the_loads() {
        let mut inputs = PlanInputs::new(true, true, true);
        inputs.dry_run = true;
        let plan = plan_reload(&inputs);
        assert!(plan.succeeded);
        assert!(plan.data_types_wiped);
        assert_eq!(5, plan.count);
        assert_eq!(Action::SwapInInterpreter, plan.actions[0]);
        assert_eq!(Action::LoadEntityDefs, plan.actions[1]);
        assert_eq!(Action::LoadUserDataObjects, plan.actions[2]);
        assert_eq!(Action::SwapBackInterpreter, plan.actions[3]);
        assert_eq!(Action::DestroyNewInterpreter, plan.actions[4]);
        assert_eq!(-1, plan.index_of(Action::CleanupAfterReload));
        assert_eq!(-1, plan.index_of(Action::MigrateEntityTypes));
        assert_eq!(-1, plan.index_of(Action::MigrateUserDataObjectTypes));
        assert_eq!(-1, plan.index_of(Action::MigrateMailBoxes));
        assert_eq!(-1, plan.index_of(Action::MigrateEntities));
        assert_eq!(-1, plan.index_of(Action::MigratedAll));
        assert_eq!(-1, plan.index_of(Action::RecoverOldScripts));
    }

    /// A dry run honours the schema-digest barrier like a real reload: a
    /// rejected digest skips the UDO load and the probe still stops after
    /// the swap back, but without the scripts-only failure path's recovery
    /// step (`ReloadScriptPlan_DryRunHonoursTheDigestBarrier`).
    #[test]
    fn dry_run_honours_the_digest_barrier() {
        let mut inputs = PlanInputs::new(false, true, true);
        inputs.digest_matched = false;
        inputs.dry_run = true;
        let plan = plan_reload(&inputs);
        assert!(!plan.succeeded);
        assert!(!plan.data_types_wiped);
        assert_eq!(4, plan.count);
        assert_eq!(Action::LoadEntityDefs, plan.actions[1]);
        assert_eq!(Action::SwapBackInterpreter, plan.actions[2]);
        assert_eq!(Action::DestroyNewInterpreter, plan.actions[3]);
        assert_eq!(-1, plan.index_of(Action::LoadUserDataObjects));
        assert_eq!(-1, plan.index_of(Action::CleanupAfterReload));
        assert_eq!(-1, plan.index_of(Action::RecoverOldScripts));
    }
}
