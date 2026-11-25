//! The scenario tests: the demo's `Guard`/`Space` boot chain and the
//! reference error arms, each in its own process (the interpreter refuses
//! a second init per process, the harness rule
//! `python-host::script_types` already follows).

use std::path::PathBuf;
use std::process::Command;

use python_host::capi;
use python_host::import_paths;
use python_host::interpreter::{InitConfig, Interpreter};
use python_host::pyref::PyRef;
use python_host::script_types;

use crate::baseapp_scripts::{
    CellCreateRequest, CellDestroyRequest, Engine, EntityTypeSpec, ImportError, SpaceCreateRequest,
    install_base_faces, install_module_faces, install_proxy_face,
};
use crate::cell_data::BaseAddr;
use crate::entity_creator::IdPool;
use crate::global_bases::RegisterAck;

/// Every scenario, each in its own process.
const SCENARIOS: &[&str] = &[
    "baseapp_scripts::tests::scenario_space_boot_then_guard_attaches",
    "baseapp_scripts::tests::scenario_register_failure_drives_destroy_cell_entity",
    "baseapp_scripts::tests::scenario_the_error_arms",
    "baseapp_scripts::tests::scenario_the_import_arms",
    "baseapp_scripts::tests::scenario_cell_data_and_mailbox_faces",
    "baseapp_scripts::tests::scenario_the_construction_and_exception_faces",
    "baseapp_scripts::tests::scenario_the_space_id_and_state_arms",
    "baseapp_scripts::tests::scenario_the_unknown_and_missing_faces",
];

#[test]
fn the_scenarios_run() {
    for scenario in SCENARIOS {
        run_scenario(scenario);
    }
}

/// Runs `scenario` in a fresh process of this test binary and requires it
/// to exit normally.
fn run_scenario(scenario: &str) {
    let harness = std::env::current_exe().expect("the test binary path");
    let output = Command::new(&harness)
        .args(["--exact", scenario, "--ignored", "--test-threads=1"])
        .output()
        .expect("spawn the scenario child");
    assert!(
        output.status.success(),
        "scenario {scenario} did not exit normally: {}\n--- stdout ---\n{}\n--- stderr ---\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

/// `Interpreter::init` around one body, then `Script::fini`. `Script::init`
/// installs the bare `BigWorld.Base`/`Proxy` import faces itself; the
/// registry rule hands the first spec back, so those bare faces are
/// released (`binding::reset`) and the full-spec `BigWorld.Base` is built
/// before `script_types::install` re-attaches the role bases over it
/// (`Proxy` re-derives the full-spec `Base`; a heap-type subclass with no
/// own instance bytes keeps the base's `tp_basicsize`). The mailboxes and
/// `globalBases` come last.
fn interpreter_scenario(body: impl FnOnce()) {
    let root = temp_resource_root();
    let role = demo_scripts_dir();
    let mut paths = import_paths::ImportPaths::default();
    paths.add_res_path(&[&root.to_string_lossy()], import_paths::COMMON_PATH);
    paths.add_res_path(&[&root.to_string_lossy()], "scripts/base");
    paths.add_non_res_path(&role.to_string_lossy());
    let interpreter = Interpreter::init(&InitConfig {
        component: String::from("base"),
        import_paths: paths,
        ..InitConfig::default()
    })
    .expect("Script::init");
    python_host::binding::reset();
    install_base_faces().expect("the BigWorld.Base faces");
    install_proxy_face().expect("the BigWorld.Proxy face");
    // SAFETY: the interpreter is up; the module is the BigWorld singleton.
    unsafe {
        script_types::install(capi::PyImport_AddModule(c"BigWorld".as_ptr()), "base");
    }
    install_module_faces().expect("the module faces");
    // Eight ids: the widest scenario constructs six bases (one dies in
    // its `__init__`, the refusals account for the rest). The app address
    // is the mailbox every `registerGlobally` offers.
    let mut pool = IdPool::new(0, 0, 0, 0);
    pool.refill(&[101, 102, 103, 104, 105]);
    Engine::install(pool, (10, 56100));
    body();
    interpreter.fini();
}

/// A throw-away resource root: the one startup module the interpreter
/// always imports (`bw_site`) plus the extra script modules the import
/// arms exercise.
fn temp_resource_root() -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "boundless-server-runtime-baseapp-scripts-{}",
        std::process::id()
    ));
    std::fs::remove_dir_all(&root).ok();
    std::fs::create_dir_all(root.join("scripts/common/Lib")).expect("build the script tree");
    std::fs::create_dir_all(root.join("scripts/base")).expect("build the base script tree");
    std::fs::write(
        root.join("scripts/common/bw_site.py"),
        "import sys\nsys.bw_site_imported = True\n",
    )
    .expect("write bw_site");
    // The module with no class of its own name: the "Could not find
    // class %s" arm.
    std::fs::write(root.join("scripts/base/NoClass.py"), "x = 1\n").expect("write NoClass");
    // The class that does not derive the engine base: the
    // "%s should be derived from BigWorld.%s" arm.
    std::fs::write(
        root.join("scripts/base/Rebel.py"),
        "import BigWorld\n\n\nclass Rebel( object ):\n\n\tpass\n",
    )
    .expect("write Rebel");
    // The same refusal with the `BigWorld.Base` face removed from the
    // module: the class derives it, so the import succeeds once the face
    // is back.
    std::fs::write(
        root.join("scripts/base/Plain.py"),
        "import BigWorld\n\n\nclass Plain( BigWorld.Base ):\n\n\tpass\n",
    )
    .expect("write Plain");
    // A base-only type (no cell placement): the "is not cell based" arm
    // and the `cellData`-less faces.
    std::fs::write(
        root.join("scripts/base/CellOnly.py"),
        "import BigWorld\n\n\nclass CellOnly( BigWorld.Base ):\n\n\tpass\n",
    )
    .expect("write CellOnly");
    // An `__init__` that destroys: the base.cpp post-init destroyed check.
    std::fs::write(
        root.join("scripts/base/Boom.py"),
        "import BigWorld\n\n\nclass Boom( BigWorld.Base ):\n\n\tdef __init__( self ):\n\t\tself.destroy()\n",
    )
    .expect("write Boom");
    // An `onRegistered` that raises a custom exception whose `__str__`
    // raises: the swallowed-callback error arms.
    std::fs::write(
        root.join("scripts/base/Wail.py"),
        "import BigWorld\n\n\nclass SilentError( ValueError ):\n\n\tdef __str__( self ):\n\t\traise RuntimeError( 'no render' )\n\n\nclass Wail( BigWorld.Base ):\n\n\tdef onRegistered( self, succeeded ):\n\t\traise SilentError( 'wail ack' )\n",
    )
    .expect("write Wail");
    // A cell-capable type with cell properties of every flavour: the
    // `cellData` seeding covers the whole `PropValue` family.
    std::fs::write(
        root.join("scripts/base/Statue.py"),
        "import BigWorld\n\n\nclass Statue( BigWorld.Base ):\n\n\tpass\n",
    )
    .expect("write Statue");
    root
}

/// The generated fantasydemo fixture base scripts, in the repo.
fn demo_scripts_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/review/fixtures/fantasydemo_def/res/scripts/base")
}

/// Runs `source` and reads the `__probe` it assigns back as a string.
fn probe(source: &str) -> String {
    // SAFETY: only called from a scenario body, while the interpreter is
    // up on the thread that owns the GIL.
    unsafe {
        let program = format!("import BigWorld\n__probe = 'UNSET'\n{source}");
        let Ok(encoded) = std::ffi::CString::new(program) else {
            panic!("probe source has no embedded NUL");
        };
        assert!(
            capi::PyRun_SimpleString(encoded.as_ptr()) == 0,
            "script must run: {source}"
        );
        let main = capi::PyImport_AddModule(c"__main__".as_ptr());
        let dictionary = capi::PyModule_GetDict(main);
        let value = capi::PyDict_GetItemString(dictionary, c"__probe".as_ptr());
        let text = capi::PyObject_Str(value);
        let rendered = capi::to_string(text);
        assert!(
            !text.is_null() && rendered != "UNSET",
            "probe did not assign __probe: {source}"
        );
        capi::dec_ref(text);
        rendered
    }
}

/// `try`/`except` around one statement, probing the exception type name
/// and text (`type(e).__name__` and `str(e)`), so the assertion lands on
/// the reference's own error text.
fn probe_error(source: &str) -> (String, String) {
    let program = "__probe = ('none', '')\ntry:\n\x20{{SRC}}\n\
                   except Exception as error:\n\x20\
                   __probe = (type( error ).__name__, str( error ))"
        .replace("{{SRC}}", source);
    let rendered = probe(&program);
    let inner = rendered
        .strip_prefix('(')
        .and_then(|rest| rest.strip_suffix(')'))
        .expect("the probe tuple");
    let (kind, text) = inner.split_once(", ").expect("a two-slot probe tuple");
    // The tuple's own repr quotes each slot once.
    (
        String::from(kind.trim_matches('\'')),
        String::from(text.trim_matches('\'')),
    )
}

/// Stash an instance in `__main__` so the probes can name it.
fn stash(name: &str, object: &PyRef) {
    // SAFETY: the interpreter is up; the attribute set takes its own
    // reference of the borrowed pointer.
    unsafe {
        let Ok(encoded) = std::ffi::CString::new(name) else {
            panic!("stash name has no embedded NUL");
        };
        let main = capi::PyImport_AddModule(c"__main__".as_ptr());
        assert!(
            capi::PyObject_SetAttrString(main, encoded.as_ptr(), object.as_ptr()) == 0,
            "stash {name}"
        );
    }
}

/// Register the two demo entity types and import their script classes.
fn register_demo_types() {
    Engine::with(|engine| {
        for (name, id) in [("Guard", 1), ("Space", 2)] {
            engine.register_type(EntityTypeSpec {
                name: String::from(name),
                id,
                can_be_on_base: true,
                can_be_on_cell: true,
                is_service: false,
            });
            engine
                .import_entity_class(name)
                .unwrap_or_else(|error| panic!("{name}: {error:?}"));
        }
    });
}

/// Asserts the engine swallowed no script exception so far.
fn assert_no_script_errors() {
    Engine::with(|engine| {
        assert!(
            engine.script_errors().is_empty(),
            "script errors: {:?}",
            engine.script_errors()
        );
    });
}

/// Drops the swallowed `__init__` exceptions the reference itself prints
/// and moves past (`ScriptErrorPrint`): this scenario constructs the demo
/// scripts with `globalBases` empty, so each `Guard.__init__` dies on its
/// `globalBases["DefaultSpace"]` lookup before the refusal arms run.
fn clear_script_errors() {
    Engine::with(|engine| engine.script_errors.clear());
}

/// The demo boot chain (`docs/review/fixtures/fantasydemo_def/res/scripts/base`):
/// `Space.__init__` asks for the new space and registers `DefaultSpace`;
/// the ack lands the mailbox; `Guard.__init__` reads
/// `BigWorld.globalBases["DefaultSpace"].cell` and asks for its cell
/// entity on that address; the contact/destroy chain walks the base to
/// its `destroy()`.
#[test]
#[ignore]
fn scenario_space_boot_then_guard_attaches() {
    interpreter_scenario(|| {
        register_demo_types();
        // `newEntityBase` + `Space.__init__`: the space create ask and the
        // pending global registration.
        let space = Engine::new_entity("Space").expect("Space");
        Engine::with(|engine| {
            assert!(
                engine.script_errors().is_empty(),
                "script errors: {:?}",
                engine.script_errors()
            );
            assert_eq!(
                engine.space_requests(),
                &[SpaceCreateRequest {
                    entity_id: 101,
                    should_prefer_this_machine: false,
                }]
            );
            assert_eq!(engine.registers.len(), 1);
            assert_eq!(engine.registers[0].key, "DefaultSpace");
            assert_eq!(engine.pending_register_count(), 1);
            assert!(engine.globals.is_empty());
        });
        assert_no_script_errors();
        stash("__space", &space);

        // The manager's ack: `DefaultSpace` lands and `onRegistered(True)`
        // runs (the `if not succeeded` arm stays shut).
        let ack = Engine::register_ack(101, true).expect("the pending");
        assert_eq!(ack, RegisterAck::Registered);
        Engine::with(|engine| {
            assert_eq!(engine.pending_register_count(), 0);
            assert!(engine.globals.get("DefaultSpace").is_some());
        });
        assert_no_script_errors();
        assert_eq!(
            probe("__probe = BigWorld.globalBases['DefaultSpace'] is not None"),
            "True"
        );

        // `Guard.__init__`: the cellData edit, then `createCellEntity` on
        // the space base's cell address.
        let guard = Engine::new_entity("Guard").expect("Guard");
        assert_no_script_errors();
        Engine::with(|engine| {
            assert_eq!(
                engine.cell_create_requests(),
                &[CellCreateRequest {
                    entity_id: 102,
                    nearby: Some((10, 56100)),
                    space_id: None,
                }]
            );
        });
        assert_no_script_errors();
        stash("__guard", &guard);
        assert_eq!(probe("__probe = repr(__space.id)"), "101");
        assert_eq!(probe("__probe = repr(__guard.id)"), "102");
        assert_eq!(probe("__probe = repr(__guard.className)"), "'Guard'");
        assert_eq!(probe("__probe = repr(__guard.baseType)"), "1");
        assert_eq!(probe("__probe = repr(__guard.databaseID)"), "0");

        // The cell entity contacts the base: `hasCell` flips and the
        // destroy chain unblocks.
        Engine::with(|engine| {
            assert!(engine.creator.cell_contact(
                102,
                1,
                BaseAddr {
                    ip: 10,
                    port: 56201
                }
            ));
        });
        assert_eq!(probe("__probe = repr(__guard.hasCell)"), "True");
        assert_eq!(probe("__probe = __guard.cell is not None"), "True");

        // `destroyCellEntity` records the intent, `cell_lost` clears the
        // reference, and `destroy()` finally goes through.
        assert_eq!(probe("__probe = __guard.destroyCellEntity()"), "None");
        Engine::with(|engine| {
            assert_eq!(
                engine.cell_destroy_requests(),
                &[CellDestroyRequest { entity_id: 102 }]
            );
        });
        Engine::with(|engine| assert!(engine.creator.cell_lost(102, 1)));
        assert_eq!(probe("__probe = __guard.destroy()"), "None");
        Engine::with(|engine| assert!(engine.creator.is_destroyed(102)));
        assert_eq!(probe("__probe = repr(__guard.isDestroyed)"), "True");
        assert_no_script_errors();
    });
}

/// The failed registration: the callback gets `False`, `onRegistered`
/// runs `destroyCellEntity` (legal while the contact is pending), and the
/// dictionary is untouched.
#[test]
#[ignore]
fn scenario_register_failure_drives_destroy_cell_entity() {
    interpreter_scenario(|| {
        register_demo_types();
        let space = Engine::new_entity("Space").expect("Space");
        stash("__space", &space);
        // The ack is answered unborrowed: `onRegistered(False)` runs
        // `destroyCellEntity`, which re-enters the engine.
        let ack = Engine::register_ack(101, false).expect("the pending");
        assert_eq!(ack, RegisterAck::Duplicate);
        Engine::with(|engine| {
            assert!(engine.globals.is_empty());
            assert_eq!(
                engine.cell_destroy_requests(),
                &[CellDestroyRequest { entity_id: 101 }],
                "onRegistered(False) destroyed the cell entity"
            );
        });
        assert_no_script_errors();
        assert_eq!(
            probe(
                "try:\n\x20BigWorld.globalBases['DefaultSpace']\n\x20\
                 __probe = 'present'\nexcept KeyError:\n\x20__probe = 'absent'"
            ),
            "absent"
        );
    });
}

/// The `checkAssociatedCellEntity`/`destroy` refusal arms with the
/// reference's own texts, the double-`destroyCellEntity` silence, the
/// keyword-only refusals, and the space-id create path.
#[test]
#[ignore]
fn scenario_the_error_arms() {
    interpreter_scenario(|| {
        register_demo_types();

        // Guard 101: its cell entity contacted it -- `createCellEntity`
        // refuses, `destroy` refuses with the redirect.
        let guard = Engine::new_entity("Guard").expect("Guard");
        stash("__guard", &guard);
        clear_script_errors();
        Engine::with(|engine| {
            assert!(engine.creator.cell_contact(
                101,
                1,
                BaseAddr {
                    ip: 10,
                    port: 56201
                }
            ));
        });
        let (kind, text) = probe_error("__guard.createCellEntity( None )");
        assert_eq!(kind, "ValueError");
        assert_eq!(
            text,
            "Base.createCellEntity: Base 101 already has an associated cell entity."
        );
        let (kind, text) = probe_error("__guard.destroy()");
        assert_eq!(kind, "ValueError");
        assert_eq!(text, "Still has cell entity. Use Base.destroyCellEntity");

        // The keyword-only spellings stay positional-only the way
        // `METH_VARARGS` binds them.
        let (kind, _) = probe_error("__guard.createInNewSpace( True, 1 )");
        assert_eq!(kind, "TypeError", "two positionals is an argument error");
        let (kind, text) = probe_error("__guard.destroy( True )");
        assert_eq!(kind, "TypeError");
        assert_eq!(text, "Only expecting keyword arguments");

        // `destroyCellEntity` goes through once; the second call is the
        // reference's silent no-op ("Don't let people do this twice").
        assert_eq!(probe("__probe = __guard.destroyCellEntity()"), "None");
        assert_eq!(probe("__probe = __guard.destroyCellEntity()"), "None");
        Engine::with(|engine| {
            assert_eq!(
                engine.cell_destroy_requests(),
                &[CellDestroyRequest { entity_id: 101 }],
                "the second destroyCellEntity is a no-op"
            );
        });
        // The cell reference is still held: `destroy` keeps refusing.
        let (kind, text) = probe_error("__guard.destroy()");
        assert_eq!(kind, "ValueError");
        assert_eq!(text, "Still has cell entity. Use Base.destroyCellEntity");

        // `cell_lost` clears the reference and `destroy()` goes through;
        // the double destroy and the post-destroy cell create hit the
        // destroyed arms.
        Engine::with(|engine| assert!(engine.creator.cell_lost(101, 1)));
        assert_eq!(probe("__probe = __guard.destroy()"), "None");
        let (kind, text) = probe_error("__guard.destroy()");
        assert_eq!(kind, "ValueError");
        assert_eq!(text, "Base entity already destroyed");
        let (kind, text) = probe_error("__guard.createCellEntity( None )");
        assert_eq!(kind, "ValueError");
        assert_eq!(text, "Base.createCellEntity: Base 101 is destroyed");

        // Guard 102: `createInNewSpace` rides the create-pending guard.
        let second = Engine::new_entity("Guard").expect("Guard 2");
        stash("__guard2", &second);
        clear_script_errors();
        assert_eq!(probe("__probe = __guard2.createInNewSpace( True )"), "None");
        Engine::with(|engine| {
            assert_eq!(
                engine.space_requests(),
                &[SpaceCreateRequest {
                    entity_id: 102,
                    should_prefer_this_machine: true,
                }]
            );
        });
        let (kind, text) = probe_error("__guard2.createInNewSpace()");
        assert_eq!(kind, "ValueError");
        assert_eq!(
            text,
            "Base.createEntityInNewSpace: Base 102 is in the process of creating \
             an associated cell entity."
                .replace('\n', " ")
                .trim()
                .to_string()
        );

        // The create fails; the null-mailbox path then reads the seeded
        // `cellData["spaceID"]` -- 0 is the invalid-space refusal, a set
        // value rides the space.
        Engine::with(|engine| {
            assert_eq!(
                engine.creator.cell_creation_result(102, false),
                crate::entity_creator::CellCreationResult::Failed
            );
        });
        let (kind, text) = probe_error("__guard2.createCellEntity( None )");
        assert_eq!(kind, "ValueError");
        assert_eq!(
            text,
            "Base.createCellEntity: Entity 102 does not have a valid spaceID in its cellData"
        );
        assert_eq!(probe("__probe = __guard2.cellData['spaceID'] = 7"), "7");
        assert_eq!(probe("__probe = __guard2.createCellEntity( None )"), "None");
        Engine::with(|engine| {
            assert_eq!(
                engine.cell_create_requests(),
                &[CellCreateRequest {
                    entity_id: 102,
                    nearby: None,
                    space_id: Some(7),
                }]
            );
        });
        assert_no_script_errors();
    });
}

/// The `importEntityClass` arms: the no-module `Proxy` fallback, the
/// missing-class error, the wrong-base error, the construction refusal,
/// and the class hierarchy the fixture scripts see.
#[test]
#[ignore]
fn scenario_the_import_arms() {
    interpreter_scenario(|| {
        Engine::with(|engine| {
            // `Guard` imports its module's class.
            engine.register_type(EntityTypeSpec {
                name: String::from("Guard"),
                id: 1,
                can_be_on_base: true,
                can_be_on_cell: true,
                is_service: false,
            });
            engine.import_entity_class("Guard").expect("Guard");

            // `Ghost` has no script module: the `Proxy` stand-in, the
            // fallback the reference logs as INFO.
            engine.register_type(EntityTypeSpec {
                name: String::from("Ghost"),
                id: 2,
                can_be_on_base: true,
                can_be_on_cell: false,
                is_service: false,
            });
            engine.import_entity_class("Ghost").expect("the fallback");
            assert!(engine.class_is_proxy("Ghost"));

            // `NoClass` (the module exists, the class does not) and
            // `Rebel` (the class derives `object`) are the reference's
            // own error paths.
            assert_eq!(
                engine.import_entity_class("NoClass"),
                Err(ImportError::ClassMissing(String::from("NoClass")))
            );
            assert_eq!(
                engine.import_entity_class("Rebel"),
                Err(ImportError::WrongBase {
                    name: String::from("Rebel")
                })
            );
        });

        // The class hierarchy the scripts see: `Guard` stands on
        // `BigWorld.Base`; the fallback IS the `Proxy` type itself (no
        // module ever exists for it).
        assert_eq!(
            probe("import Guard\n__probe = repr(Guard.Guard.__bases__[0].__name__)"),
            "'Base'"
        );
        Engine::with(|engine| {
            let ghost = engine.class_of("Ghost").expect("the fallback class");
            stash("__ghost_class", ghost);
        });
        assert_eq!(
            probe("__probe = repr(__ghost_class is BigWorld.Proxy)"),
            "True"
        );
        assert_eq!(
            probe("__probe = repr(BigWorld.Proxy.__bases__[0].__name__)"),
            "'Base'"
        );

        // A direct construction of the engine base is refused (the engine
        // allocates through `PyType_GenericAlloc`, the reference's
        // placement-new).
        let (kind, _) = probe_error("BigWorld.Base()");
        assert_eq!(kind, "TypeError");
    });
}

/// The `cellData` get/set pair, the dict check, and the mailbox faces
/// (`globalBases` subscript, `.cell`, the `KeyError`).
#[test]
#[ignore]
fn scenario_cell_data_and_mailbox_faces() {
    interpreter_scenario(|| {
        register_demo_types();
        let space = Engine::new_entity("Space").expect("Space");
        stash("__space", &space);
        Engine::register_ack(101, true);

        // The `Guard` script edits its `cellData` dict in `__init__`; the
        // engine sees the same object.
        let guard = Engine::new_entity("Guard").expect("Guard");
        stash("__guard", &guard);
        assert_eq!(
            probe("__probe = repr(sorted(__guard.cellData.keys()))"),
            "['direction', 'position', 'spaceID']"
        );
        assert_eq!(
            probe("__guard.cellData['score'] = 5\n__probe = repr(__guard.cellData['score'])"),
            "5"
        );
        Engine::with(|engine| {
            let dict = engine.cell_data_of(102).expect("the cell data");
            // SAFETY: a live dict; the borrowed read is immediate.
            let value = unsafe { capi::PyDict_GetItemString(dict.as_ptr(), c"score".as_ptr()) };
            assert!(
                !value.is_null(),
                "the script edit landed in the engine's dict"
            );
        });

        // The set side: a dict replaces, a non-dict is the reference's
        // TypeError.
        assert_eq!(
            probe("__guard.cellData = {'k': 1}\n__probe = len(__guard.cellData)"),
            "1"
        );
        let (kind, text) = probe_error("__guard.cellData = 3");
        assert_eq!(kind, "TypeError");
        assert_eq!(text, "cellData must be set to a dictionary");
        Engine::with(|engine| {
            let dict = engine.cell_data_of(102).expect("the replaced cell data");
            // SAFETY: a live dict; the borrowed read is immediate.
            let value = unsafe { capi::PyDict_GetItemString(dict.as_ptr(), c"k".as_ptr()) };
            assert!(!value.is_null(), "the replacement landed");
        });

        // The mailbox faces: the entry, its `.cell`, and the KeyError.
        assert_eq!(
            probe("__probe = BigWorld.globalBases['DefaultSpace'].cell is not None"),
            "True"
        );
        let (kind, _) = probe_error("BigWorld.globalBases['Missing']");
        assert_eq!(kind, "KeyError");
        assert_no_script_errors();
    });
}

/// The construction and exception faces the other scenarios don't reach:
/// the cell-side constructor, the destroyed-in-`__init__` check, the id
/// exhaustion, the `PropValue` seeding, the ack-callback exception arms,
/// the `prepareForCellCreate` pendings, and the engine-less-instance
/// refusals (a cell-side instance carries no engine id -- the cellapp
/// row owns that face).
#[test]
#[ignore]
fn scenario_the_construction_and_exception_faces() {
    interpreter_scenario(|| {
        register_demo_types();
        // Statue rides the creator directly: the entity defs give it cell
        // properties of every flavour (the defaults add the `Vec3`
        // position/direction and the `Int` spaceID).
        Engine::with(|engine| {
            engine.creator.add_type(crate::entity_creator::EntityType {
                id: 3,
                name: String::from("Statue"),
                can_be_on_base: true,
                is_service: false,
                is_proxy: false,
                can_be_on_cell: true,
                properties: vec![
                    crate::entity_creator::PropDesc::new(
                        "ratio",
                        false,
                        true,
                        false,
                        false,
                        crate::entity_creator::PropValue::Float(1.5),
                    ),
                    crate::entity_creator::PropDesc::new(
                        "label",
                        false,
                        true,
                        false,
                        false,
                        crate::entity_creator::PropValue::Str(String::from("marble")),
                    ),
                    crate::entity_creator::PropDesc::new(
                        "alive",
                        false,
                        true,
                        false,
                        false,
                        crate::entity_creator::PropValue::Bool(true),
                    ),
                ],
                methods: Vec::new(),
            });
            engine.register_type(EntityTypeSpec {
                name: String::from("Boom"),
                id: 4,
                can_be_on_base: true,
                can_be_on_cell: true,
                is_service: false,
            });
            engine.register_type(EntityTypeSpec {
                name: String::from("Wail"),
                id: 5,
                can_be_on_base: true,
                can_be_on_cell: false,
                is_service: false,
            });
            engine.register_type(EntityTypeSpec {
                name: String::from("CellOnly"),
                id: 6,
                can_be_on_base: false,
                can_be_on_cell: true,
                is_service: false,
            });
            engine.register_type(EntityTypeSpec {
                name: String::from("Ghost"),
                id: 7,
                can_be_on_base: true,
                can_be_on_cell: false,
                is_service: false,
            });
            for name in ["Statue", "Boom", "Wail", "CellOnly", "Ghost"] {
                engine
                    .import_entity_class(name)
                    .unwrap_or_else(|error| panic!("{name}: {error:?}"));
            }
        });

        // `Boom.__init__` destroys itself: the base.cpp post-init check
        // refuses the creation.
        assert_eq!(
            Engine::new_entity("Boom"),
            Err(crate::baseapp_scripts::NewEntityError::DestroyedInInit)
        );
        Engine::with(|engine| assert!(engine.creator.is_destroyed(101)));

        // The cell-side constructor: a script with no base-face contact
        // constructs clean; a script whose `__init__` dies leaves the
        // swallowed error with no engine id; an unknown type refuses.
        let cell_statue = Engine::construct_cell_entity("Statue").expect("the cell statue");
        stash("__cell_statue", &cell_statue);
        let cell_guard = Engine::construct_cell_entity("Guard").expect("the cell guard");
        Engine::with(|engine| {
            assert_eq!(engine.script_errors().len(), 1);
            assert_eq!(engine.script_errors()[0].entity_id, 0);
            // The demo `Guard.__init__` touches `cellData` before the
            // `globalBases` lookup: on an id-less instance the getter's
            // null return surfaces as the interpreter's own SystemError,
            // not the KeyError the base-side guard dies on.
            assert!(
                !engine.script_errors()[0].text.is_empty(),
                "script errors: {:?}",
                engine.script_errors()
            );
            engine.script_errors.clear();
        });
        drop(cell_guard);
        assert_eq!(
            Engine::construct_cell_entity("Missing"),
            Err(ImportError::ClassMissing(String::from("Missing")))
        );

        // The cell-side instance carries no engine id: every base face
        // refuses. The null returns surface as `SystemError`s (the
        // defensive arm sets no exception -- the reference never reaches
        // this shape, the cellapp row owns the faces).
        for attribute in [
            "id",
            "isDestroyed",
            "baseType",
            "className",
            "cell",
            "hasCell",
            "cellData",
            "databaseID",
        ] {
            let (kind, _) = probe_error(&format!("__cell_statue.{attribute}"));
            assert_eq!(
                kind, "SystemError",
                "attribute {attribute} on an id-less instance"
            );
        }
        let (kind, _) = probe_error("__cell_statue.cellData = {}");
        assert_eq!(kind, "SystemError", "set cellData on an id-less instance");
        for (call, name) in [
            ("__cell_statue.createInNewSpace()", "createInNewSpace"),
            (
                "__cell_statue.registerGlobally( 'k', lambda: None )",
                "registerGlobally",
            ),
            ("__cell_statue.destroyCellEntity()", "destroyCellEntity"),
            ("__cell_statue.destroy()", "destroy"),
            ("__cell_statue.createCellEntity( None )", "createCellEntity"),
        ] {
            let (kind, _) = probe_error(call);
            assert_eq!(kind, "SystemError", "method {name} on an id-less instance");
        }

        // The remaining creates: Statue carries the property flavours,
        // Wail drives the ack-callback arms, the ghosts exhaust the pool.
        let statue = Engine::new_entity("Statue").expect("Statue");
        stash("__statue", &statue);
        assert_eq!(
            probe("__probe = repr(sorted(__statue.cellData.keys()))"),
            "['alive', 'direction', 'label', 'position', 'ratio', 'spaceID']",
            "the defaults and the property flavours all seeded"
        );
        let wail = Engine::new_entity("Wail").expect("Wail");
        stash("__wail", &wail);
        let ghost = Engine::new_entity("Ghost").expect("Ghost");
        stash("__ghost", &ghost);

        // The `cellData` faces on Ghost (base-only, no properties): the
        // seeding still builds the dict, empty. The setter accepts a
        // dict and refuses anything else.
        assert_eq!(probe("__probe = repr(__ghost.cellData)"), "{}");
        assert_eq!(probe("__probe = __ghost.cellData = {'k': 1}"), "{'k': 1}");
        let (kind, _) = probe_error("__ghost.cellData = 3");
        assert_eq!(kind, "TypeError");
        // A dict without `spaceID`: the zero default is the invalid-space
        // refusal.
        let (kind, text) = probe_error("__ghost.createCellEntity( None )");
        assert_eq!(kind, "ValueError");
        assert_eq!(
            text,
            "Base.createCellEntity: Entity 104 does not have a valid spaceID in its cellData"
        );
        // No arguments at all: the parse failure.
        let (kind, _) = probe_error("__ghost.createCellEntity()");
        assert_eq!(kind, "TypeError");
        // Base-ineligible: the "not cell based" arm.
        let (kind, text) = probe_error("__ghost.createInNewSpace()");
        assert_eq!(kind, "ValueError");
        assert_eq!(
            text,
            "Base.createEntityInNewSpace: Base 104 is not cell based"
        );
        // `destroyCellEntity` with no cell ever created: the
        // havingEntityGood refusal.
        let (kind, text) = probe_error("__ghost.destroyCellEntity()");
        assert_eq!(kind, "ValueError");
        assert_eq!(
            text,
            "Base.destroyCellEntity: Base 104 does not have an associated cell entity."
        );
        // `registerGlobally` argument arms: one positional, then a
        // non-callable callback.
        let (kind, _) = probe_error("__ghost.registerGlobally( 1 )");
        assert_eq!(kind, "TypeError");
        let (kind, text) = probe_error("__ghost.registerGlobally( 'k', 5 )");
        assert_eq!(kind, "TypeError");
        assert_eq!(text, "The second argument must be callable");

        // The registry subscript with an unprintable key: the KeyError
        // carries the reference's own placeholder text.
        assert_eq!(
            probe(
                "__probe = None\nclass BadStr:\n\x20def __str__( self ):\n\x20\x20\
                 raise RuntimeError( 'no' )\ntry:\n\x20\
                 BigWorld.globalBases[BadStr()]\nexcept KeyError as error:\n\x20\
                 __probe = str( error )"
            ),
            "'<unprintable key>'"
        );
        // A key that fails to render registers under the empty text.
        probe("__probe = __wail.registerGlobally( BadStr(), __wail.onRegistered )");
        // The ack callback raises a `__str__`-broken exception: the
        // reference prints and moves past; the port records the
        // unrenderable fallback.
        assert_eq!(
            Engine::register_ack(103, true),
            Some(RegisterAck::Registered)
        );
        Engine::with(|engine| {
            assert_eq!(
                engine.script_errors().len(),
                1,
                "script errors: {:?}",
                engine.script_errors()
            );
            assert_eq!(engine.script_errors()[0].text, "unrenderable script error");
            engine.script_errors.clear();
        });
        // The exception ack rides the same swallowed-callback shape.
        probe("__probe = __wail.registerGlobally( 'WailKey', __wail.onRegistered )");
        assert!(Engine::register_exception(103).is_some());
        Engine::with(|engine| {
            assert_eq!(engine.script_errors().len(), 1);
            assert_eq!(engine.script_errors()[0].text, "unrenderable script error");
            engine.script_errors.clear();
        });

        // The `prepareForCellCreate` pendings on a cell-capable base: the
        // ip-zero contact answers "not created yet", the real contact
        // blocks a second create, and the contact wait refuses both
        // create faces with the reference's texts.
        Engine::with(|engine| {
            assert!(
                engine
                    .creator
                    .cell_contact(102, 1, BaseAddr { ip: 0, port: 0 })
            );
        });
        let (kind, text) = probe_error("__statue.cell");
        assert_eq!(kind, "AttributeError");
        assert_eq!(text, "Cell not created yet");
        Engine::with(|engine| {
            assert!(engine.creator.cell_contact(
                102,
                1,
                BaseAddr {
                    ip: 10,
                    port: 56201
                }
            ));
        });
        assert_eq!(probe("__probe = __statue.cell is not None"), "True");
        let (kind, text) = probe_error("__statue.createInNewSpace()");
        assert_eq!(kind, "ValueError");
        assert_eq!(
            text,
            "Base.createEntityInNewSpace: Base 102 already has an associated cell entity."
        );
        // The mailbox type check sits behind the associated-cell check,
        // so the non-mailbox argument rides a base with no cell (Ghost).
        let (kind, text) = probe_error("__ghost.createCellEntity( 1 )");
        assert_eq!(kind, "TypeError");
        assert_eq!(text, "createCellEntity() expects a cell mailbox");

        // Wail has no cell at all: the not-created arm.
        let (kind, text) = probe_error("__wail.cell");
        assert_eq!(kind, "AttributeError");
        assert_eq!(text, "Cell not created yet");

        // A guard with both pendings: the create-pending refusal, then
        // the contact wait through both faces.
        let guard = Engine::new_entity("Guard").expect("Guard");
        stash("__guard2", &guard);
        clear_script_errors();
        // The pool of five is now spent: the exhaustion and the
        // base-ineligible refusals ride last so every id sees use.
        assert_eq!(
            Engine::new_entity("Ghost"),
            Err(crate::baseapp_scripts::NewEntityError::NoId),
            "the pool of five is spent"
        );
        assert_eq!(
            Engine::new_entity("CellOnly"),
            Err(crate::baseapp_scripts::NewEntityError::InvalidType(
                String::from("CellOnly")
            )),
            "a base-ineligible type refuses the creation"
        );
        assert_eq!(probe("__probe = __guard2.createInNewSpace()"), "None");
        let (kind, text) = probe_error("__guard2.createCellEntity( None )");
        assert_eq!(kind, "ValueError");
        assert_eq!(
            text,
            "Base.createCellEntity: Base 105 is in the process of creating an associated cell entity."
        );
        Engine::with(|engine| {
            assert_eq!(
                engine.creator.cell_creation_result(105, true),
                crate::entity_creator::CellCreationResult::Created
            );
        });
        let (kind, text) = probe_error("__guard2.createInNewSpace()");
        assert_eq!(kind, "ValueError");
        assert_eq!(
            text,
            "Base.createEntityInNewSpace: Base 105 is waiting for contact from its created associated cell entity.\\n"
        );
        let (kind, text) = probe_error("__guard2.createCellEntity( None )");
        assert_eq!(kind, "ValueError");
        assert_eq!(
            text,
            "Base.createCellEntity: Base 105 is waiting for contact from its created associated cell entity.\\n"
        );

        // The destroyed Statue: the cell goes first (`destroy` refuses a
        // base that still holds one), then `.cell` raises with the
        // reference's own text, `destroyCellEntity` answers the destroyed
        // arm, and the `isDestroyed` flag reads true.
        assert_eq!(probe("__probe = __statue.destroyCellEntity()"), "None");
        Engine::with(|engine| {
            assert!(engine.creator.cell_lost(102, 1));
        });
        assert_eq!(probe("__probe = __statue.destroy()"), "None");
        let (kind, text) = probe_error("__statue.cell");
        assert_eq!(kind, "AttributeError");
        assert_eq!(text, "Entity 102 is destroyed");
        let (kind, text) = probe_error("__statue.destroyCellEntity()");
        assert_eq!(kind, "ValueError");
        assert_eq!(text, "Base.destroyCellEntity: Base 102 is destroyed");
        assert_eq!(probe("__probe = repr(__statue.isDestroyed)"), "True");

        // The engine state drops (the runner's next scenario is a fresh
        // process, the reference's engine owns its own lifetime).
        Engine::reset();
    });
}

/// The remaining model arms: `createCellEntity( None )`'s space-id path
/// with a seeded `spaceID`, the cellData-less refusal, the arity arm on
/// `destroyCellEntity`, and the import refusals when the module has no
/// matching class (or the engine faces are absent).
#[test]
#[ignore]
fn scenario_the_space_id_and_state_arms() {
    interpreter_scenario(|| {
        register_demo_types();
        Engine::with(|engine| {
            engine.creator.add_type(crate::entity_creator::EntityType {
                id: 8,
                name: String::from("Spaceful"),
                can_be_on_base: true,
                is_service: false,
                is_proxy: false,
                can_be_on_cell: true,
                properties: vec![crate::entity_creator::PropDesc::new(
                    "spaceID",
                    false,
                    true,
                    false,
                    false,
                    crate::entity_creator::PropValue::Int(9),
                )],
                methods: Vec::new(),
            });
            engine.register_type(EntityTypeSpec {
                name: String::from("NoCellData"),
                id: 9,
                can_be_on_base: true,
                can_be_on_cell: false,
                is_service: false,
            });
            for name in ["Spaceful", "NoCellData", "Plain"] {
                engine
                    .import_entity_class(name)
                    .unwrap_or_else(|error| panic!("{name}: {error:?}"));
            }
        });

        // The space-id path: the seeded `spaceID` is used, the create
        // request records no mailbox address. Repeating the create while
        // the first is pending must be refused by `begin_cell_creation`
        // through the same face.
        let spaceful = Engine::new_entity("Spaceful").expect("Spaceful");
        stash("__spaceful", &spaceful);
        assert_eq!(
            probe("__probe = __spaceful.createCellEntity( None )"),
            "None"
        );
        Engine::with(|engine| {
            assert_eq!(engine.cell_create_requests().len(), 1);
            assert_eq!(engine.cell_create_requests()[0].space_id, Some(9));
            assert!(engine.cell_create_requests()[0].nearby.is_none());
        });
        let (kind, text) = probe_error("__spaceful.createCellEntity( None )");
        assert_eq!(kind, "ValueError");
        assert_eq!(
            text,
            "Base.createCellEntity: Base 101 is in the process of creating an associated cell entity."
        );

        // The no-cellData arm: a type whose cell data was never seeded
        // refuses the space-id path with the reference's own text. A
        // valid space id with a missing record reaches the create, so
        // `begin_cell_creation`'s unknown arm is covered too.
        let no_data = Engine::new_entity("NoCellData").expect("NoCellData");
        stash("__no_data", &no_data);
        let (kind, text) = probe_error("__no_data.createCellEntity( None )");
        assert_eq!(kind, "ValueError");
        assert_eq!(
            text,
            "Base.createCellEntity: Entity 102 does not have a valid spaceID in its cellData"
        );

        // The arity refusal on `destroyCellEntity`: the associated-cell
        // check runs first, so it rides a base with a cell and no pending.
        Engine::with(|engine| {
            assert!(engine.creator.cell_contact(
                101,
                9,
                BaseAddr {
                    ip: 10,
                    port: 56201
                }
            ));
        });
        let (kind, text) = probe_error("__spaceful.destroyCellEntity( 1 )");
        assert_eq!(kind, "TypeError");
        assert_eq!(text, "Base.destroyCellEntity() accepts no arguments");
        // The duplicate-destroy guard: the first call records the
        // request, the second is a silent no-op.
        Engine::with(|engine| {
            assert_eq!(engine.cell_destroy_requests().len(), 0);
        });
        assert_eq!(probe("__probe = __spaceful.destroyCellEntity()"), "None");
        assert_eq!(probe("__probe = __spaceful.destroyCellEntity()"), "None");
        Engine::with(|engine| {
            assert_eq!(engine.cell_destroy_requests().len(), 1);
        });

        // The import refusals: a module with no class of its own name,
        // a class that does not derive the engine base, and the
        // no-script fallback that stands in on `Proxy`.
        let import = |name: &str| {
            Engine::with(|engine| {
                engine.register_type(EntityTypeSpec {
                    name: String::from(name),
                    id: 10,
                    can_be_on_base: true,
                    can_be_on_cell: false,
                    is_service: false,
                });
                engine.import_entity_class(name)
            })
        };
        assert_eq!(
            import("NoClass"),
            Err(ImportError::ClassMissing(String::from("NoClass")))
        );
        assert_eq!(
            import("Rebel"),
            Err(ImportError::WrongBase {
                name: String::from("Rebel")
            })
        );
        // A type with no module at all stands in on `Proxy`.
        Engine::with(|engine| {
            engine.register_type(EntityTypeSpec {
                name: String::from("Unscripted"),
                id: 11,
                can_be_on_base: true,
                can_be_on_cell: false,
                is_service: false,
            });
            engine.import_entity_class("Unscripted").unwrap();
        });
        assert!(Engine::with(|engine| engine.class_is_proxy("Unscripted")));
        // Remove `BigWorld.Base` from the module: an import whose class
        // exists still refuses without the engine face. The face is put
        // back before `interpreter_scenario` calls `fini`.
        unsafe {
            let module = capi::PyImport_AddModule(c"BigWorld".as_ptr());
            let dictionary = capi::PyModule_GetDict(module);
            assert_eq!(capi::PyDict_DelItemString(dictionary, c"Base".as_ptr()), 0);
        }
        assert_eq!(
            Engine::with(|engine| engine.import_entity_class("Plain")),
            Err(ImportError::NoScriptFace)
        );

        Engine::reset();
    });
}

/// The refused faces the other scenarios can't reach: an unknown base id,
/// the missing-`cellData` refusal, the create-pending refusal on the
/// space-id path, and the unknown-register ack.
#[test]
#[ignore]
fn scenario_the_unknown_and_missing_faces() {
    interpreter_scenario(|| {
        register_demo_types();
        Engine::with(|engine| {
            engine.creator.add_type(crate::entity_creator::EntityType {
                id: 12,
                name: String::from("Lonely"),
                can_be_on_base: true,
                is_service: false,
                is_proxy: false,
                can_be_on_cell: true,
                properties: vec![crate::entity_creator::PropDesc::new(
                    "spaceID",
                    false,
                    true,
                    false,
                    false,
                    crate::entity_creator::PropValue::Int(4),
                )],
                methods: Vec::new(),
            });
            engine.register_type(EntityTypeSpec {
                name: String::from("Ghost"),
                id: 13,
                can_be_on_base: true,
                can_be_on_cell: false,
                is_service: false,
            });
            engine.register_type(EntityTypeSpec {
                name: String::from("CellOnly"),
                id: 14,
                can_be_on_base: false,
                can_be_on_cell: true,
                is_service: false,
            });
            for name in ["Lonely", "Ghost", "CellOnly"] {
                engine
                    .import_entity_class(name)
                    .unwrap_or_else(|error| panic!("{name}: {error:?}"));
            }
        });

        // The missing-`cellData` refusal on the space-id path: the dict
        // is dropped from the engine, the base is live.
        let lonely = Engine::new_entity("Lonely").expect("Lonely");
        stash("__lonely", &lonely);
        Engine::with(|engine| engine.drop_cell_data(101));
        let (kind, text) = probe_error("__lonely.createCellEntity( None )");
        assert_eq!(kind, "AttributeError");
        assert_eq!(
            text,
            "Base.createCellEntity: Base 101 does not have a cellData property"
        );
        // The read side answers its own missing-dict refusal.
        let (kind, text) = probe_error("__lonely.cellData");
        assert_eq!(kind, "AttributeError");
        assert_eq!(text, "No cellData attribute");

        // A cell mailbox for the argument-typed legs: the contact puts a
        // real address on the (dict-less) base's record.
        Engine::with(|engine| {
            assert!(engine.creator.cell_contact(
                101,
                4,
                BaseAddr {
                    ip: 10,
                    port: 56201
                }
            ));
        });
        assert_eq!(
            probe("__cell_mb = __lonely.cell\n__probe = repr(__cell_mb is not None)"),
            "True"
        );

        // A base-only type with a valid space id: both create faces run
        // `begin_cell_creation` and answer its "not cell based" refusal
        // (the model's own no-cell-script arm).
        let ghost = Engine::new_entity("Ghost").expect("Ghost");
        stash("__ghost", &ghost);
        assert_eq!(
            probe("__ghost.cellData = {'spaceID': 4}\n__probe = repr(__ghost.cellData)"),
            "{'spaceID': 4}"
        );
        let (kind, text) = probe_error("__ghost.createCellEntity( None )");
        assert_eq!(kind, "ValueError");
        assert_eq!(text, "Base.createCellEntity: Base 102 is not cell based");
        let (kind, text) = probe_error("__ghost.createCellEntity( __cell_mb )");
        assert_eq!(kind, "ValueError");
        assert_eq!(text, "Base.createCellEntity: Base 102 is not cell based");

        // The unknown-register ack: no pending request answers silently.
        assert!(Engine::register_ack(9999, true).is_none());
        assert!(Engine::register_exception(9999).is_none());

        Engine::reset();
    });
}
