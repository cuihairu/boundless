//! The script class bases: `BigWorld.Entity`, `BigWorld.Base` and
//! `BigWorld.Proxy`.
//!
//! Every demo script's `class X(...)` statement names one of these, and
//! each app registers its set onto `BigWorld` right after the module comes
//! up -- the client sets `Entity` as an attribute of the module it just
//! fetched (`client/script_bigworld.cpp:986-988`), the cell app does the
//! same in its `initScript`
//! (`server/cellapp/cellapp.cpp:3888-3893`), the bots tool adds it through
//! the module object wrapper (`server/tools/bots/entity_type.cpp:110-111`),
//! and the base app registers its set -- `Base` and `Proxy`, plus `Service`
//! and `UserDataObject` which are not ported -- in one `PyObject_SetAttr`
//! chain (`server/baseapp/script_bigworld.cpp:2140-2143`). The database app
//! registers nothing (`server/dbapp/dbapp.cpp:326` only imports).
//!
//! The types themselves are `Py_Header` classes
//! (`lib/pyscript/pyobject_base.hpp`): client
//! `Py_Header( PyEntity, PyObjectPlus )` (`client/py_entity.hpp:34`),
//! cell `Py_Header( Entity, PyObjectPlus )` (`server/cellapp/entity.hpp:143`),
//! base `Py_Header( BW_NAMESPACE Base, PyObjectPlus )`
//! (`server/baseapp/base.hpp:60`) and
//! `Py_Header( Proxy, Base )` (`server/baseapp/proxy.hpp:76`) -- the macro
//! carries `Py_TPFLAGS_BASETYPE` (`pyobject_base.hpp:59`) so a script
//! `class` statement accepts them, and sets `tp_base` to the declared
//! super (`pyobject_base.hpp:67`), which is why `Proxy` stands on `Base`.
//! `Entity`'s two spellings are one script-visible class per app; the port
//! has one `BigWorld.Entity` whose name carries the module prefix the way
//! the port's other registered types do.
//!
//! Deltas kept visible:
//! - `PyObjectPlus`, the common root of every `Py_Header` class, is not
//!   ported, so `Entity` and `Base` fall to the default `object` base; the
//!   engine-owned half of the hierarchy (`Entity`'s C++ body, `Base`'s
//!   mailbox plumbing) belongs to the entity-type faces (BW-14-01-04 and
//!   the def wiring), not the import face this module delivers.
//! - `Py_TPFLAGS_HAVE_GC` is not set on the port's types: the reference's
//!   macro sets it because the engine allocates the instances. The port
//!   allocates none of its own on this face -- scripts only subclass --
//!   and the script classes' own instances are tracked by CPython itself.
//! - Direct construction stays refused. The reference's `_pyNew`
//!   (`pyobject_plus.cpp:148`) refuses it for the same reason: the engine
//!   owns every instance, the scripts only define subclasses. The engine
//!   never goes through `tp_new` for script classes either -- it allocates
//!   straight on the script class with `PyType_GenericAlloc` and calls
//!   `__init__` itself (`server/baseapp/entity_type.cpp:569`,
//!   `server/baseapp/base.cpp:794`), which is why a script class over the
//!   base keeps the inherited refusal without blocking engine
//!   construction. That allocation face belongs to the def-driven work
//!   (BW-14-01-04), not the import face this module delivers.

use crate::binding::{self, TypeSpec};
use crate::capi;
use crate::manifest;
use crate::pyref::PyRef;

/// `install(module, component)` -- the step each app runs right after its
/// `BigWorld` module comes up: the app set decides which bases join the
/// module, exactly the registration split of the reference's four sites.
///
/// The type objects are the process-wide `s_type_` rule
/// (`binding::create_type` dedupes on the name), so a repeat call
/// re-attaches what exists, the way a `Script::fini`/`init` cycle sees the
/// same static types again.
///
/// # Safety
///
/// The interpreter must be up and this thread must own the GIL; `module`
/// must be the `BigWorld` module object.
pub unsafe fn install(module: *mut capi::PyObject, component: &str) {
    // The base app registers `Base` before `Proxy` -- the same statement
    // chain (`script_bigworld.cpp:2140-2143`) -- and `Proxy` needs the
    // finished `Base` type object for its `tp_base`.
    if manifest::carries_role(manifest::Scope::BaseApp, component) {
        let base = type_object("BigWorld.Base", crate::entity_runtime::shape_base);
        // SAFETY: the module is live and the names are NUL-terminated keys.
        unsafe {
            set_attr(module, c"Base", &base);
            let proxy = type_object("BigWorld.Proxy", |spec| spec.subclassable().extends(&base));
            set_attr(module, c"Proxy", &proxy);
        }
    }
    // The entity apps register `Entity` -- the client
    // (`script_bigworld.cpp:987`), the cell app (`cellapp.cpp:3889`) and
    // the bots tool (`entity_type.cpp:110`) each from their own source.
    if manifest::carries_role(manifest::Scope::EntityApps, component) {
        let entity = type_object("BigWorld.Entity", crate::entity_runtime::shape_entity);
        // SAFETY: the module is live.
        unsafe {
            set_attr(module, c"Entity", &entity);
        }
    }
}

/// The one type object a dotted name describes, built through the shared
/// binding layer (one name is one type object for the process, the
/// `s_type_` rule) with `shape` adding the face-specific flags and base.
///
/// A creation failure is a programmer error, not a runtime face: the names
/// are engine-fixed constants with no embedded NUL, and the interpreter is
/// up by the caller's contract, so the failure panics -- the same rule the
/// `Math` module's install applies to its factory names.
fn type_object(name: &str, shape: impl FnOnce(TypeSpec) -> TypeSpec) -> PyRef {
    let spec = shape(TypeSpec::new(name));
    match binding::create_type(&spec) {
        Ok(object) => object,
        Err(error) => panic!("{name}: the script class base could not be created ({error:?})"),
    }
}

/// `PyObject_SetAttrString`, the reference's own registration call at all
/// four sites. A failure is cleared and carries on -- the attribute is
/// simply absent, the `ScriptErrorPrint` behaviour.
///
/// SAFETY: the interpreter must be up, `module` live and `name`
/// NUL-terminated.
unsafe fn set_attr(module: *mut capi::PyObject, name: &std::ffi::CStr, object: &PyRef) {
    // SAFETY: the module and the type object are live; a failed set leaves
    // a pending exception the caller clears.
    unsafe {
        if capi::PyObject_SetAttrString(module, name.as_ptr(), object.as_ptr()) != 0 {
            capi::clear_error();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import_paths;
    use crate::interpreter::{InitConfig, Interpreter};
    use crate::manifest::ported_surface_for_role;
    use std::path::PathBuf;
    use std::process::Command;

    /// Every scenario, each in its own process: the interpreter refuses a
    /// second init in one process, the same rule every harness follows.
    const SCENARIOS: &[&str] = &[
        "script_types::tests::scenario_the_cell_scripts_import_unchanged",
        "script_types::tests::scenario_the_base_scripts_import_unchanged",
        "script_types::tests::scenario_the_client_scripts_import_unchanged",
        "script_types::tests::scenario_proxy_stands_on_base_and_both_subclass",
        "script_types::tests::scenario_entity_sits_on_object",
        "script_types::tests::scenario_direct_construction_is_refused",
        "script_types::tests::scenario_a_script_class_over_the_base_behaves",
        "script_types::tests::scenario_a_role_without_the_class_rows_stays_clean",
    ];

    #[test]
    fn the_scenarios_run() {
        for scenario in SCENARIOS {
            run_scenario(scenario);
        }
    }

    /// Runs `scenario` in a fresh process of this test binary and requires
    /// it to exit normally.
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

    /// `Interpreter::init` around one body, then `Script::fini`. The path
    /// list carries the role's demo scripts from the generated fantasydemo
    /// fixture tree, so `import BigWorld` inside them sees the role's
    /// registered surface, unchanged text.
    fn interpreter_scenario(component: &str, body: impl FnOnce()) {
        let root = temp_resource_root();
        let role = demo_scripts_dir(component);
        let mut paths = import_paths::ImportPaths::default();
        paths.add_res_path(&[&root.to_string_lossy()], import_paths::COMMON_PATH);
        paths.add_non_res_path(&role.to_string_lossy());
        let interpreter = Interpreter::init(&InitConfig {
            component: String::from(component),
            import_paths: paths,
            ..InitConfig::default()
        })
        .expect("Script::init");
        body();
        interpreter.fini();
    }

    /// A throw-away resource root with the one module the interpreter's
    /// startup always imports: `bw_site`.
    fn temp_resource_root() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "boundless-python-host-script-types-{}",
            std::process::id()
        ));
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(root.join("scripts/common/Lib")).expect("build the script tree");
        std::fs::write(
            root.join("scripts/common/bw_site.py"),
            "import sys\nsys.bw_site_imported = True\n",
        )
        .expect("write bw_site");
        root
    }

    /// The generated fantasydemo fixture scripts of one role, in the repo.
    fn demo_scripts_dir(component: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/review/fixtures/fantasydemo_def/res/scripts")
            .join(component)
    }

    /// Runs `source` and reads the `__probe` it assigns back as a string.
    fn probe(source: &str) -> String {
        // SAFETY: only called from a scenario body, while the interpreter
        // is up on the thread that owns the GIL.
        unsafe {
            let Ok(encoded) = std::ffi::CString::new(source) else {
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
            capi::dec_ref(text);
            rendered
        }
    }

    /// `(script module name, registered base the fixture names)` per role:
    /// the generated fixture set's own class statements.
    fn role_scripts(component: &str) -> Vec<(&'static str, &'static str)> {
        match component {
            "cell" => vec![
                ("Avatar", "Entity"),
                ("Guard", "Entity"),
                ("Space", "Entity"),
            ],
            "base" => vec![
                ("Account", "Proxy"),
                ("Avatar", "Proxy"),
                ("Guard", "Base"),
                ("Space", "Base"),
            ],
            "client" => vec![
                ("Account", "Entity"),
                ("Avatar", "Entity"),
                ("ClientOnly", "Entity"),
                ("Guard", "Entity"),
                ("Space", "Entity"),
            ],
            other => unreachable!("a role the fixtures do not carry: {other}"),
        }
    }

    /// A role the fixtures do not carry is a caller error, named as such.
    #[test]
    #[should_panic = "a role the fixtures do not carry"]
    fn an_unknown_role_is_a_caller_error() {
        role_scripts("mystery");
    }

    /// The demo import face, per role, unchanged scripts: every fixture
    /// script imports, its class statement resolves against the registered
    /// base, and the base in its `__bases__` is the very type object the
    /// module carries. `cell` scripts name `BigWorld.Entity`, `base`
    /// scripts name `BigWorld.Proxy`/`BigWorld.Base`, `client` scripts
    /// name `BigWorld.Entity`.
    fn demo_scripts_import_unchanged(component: &str) {
        interpreter_scenario(component, || {
            for (script, base_name) in role_scripts(component) {
                // The module import is the acceptance: the script text is
                // the fixture's, verbatim.
                let answer = probe(&format!(
                    "import BigWorld, {script}\n\
                     base = BigWorld.{base_name}\n\
                     defined = {script}.{script}\n\
                     __probe = repr([\n\
                     \x20 issubclass(defined, base),\n\
                     \x20 defined.__bases__[0] is base,\n\
                     \x20 base.__name__,\n\
                     \x20 defined.__name__,\n\
                     ])"
                ));
                assert_eq!(
                    answer,
                    format!("[True, True, '{base_name}', '{script}']"),
                    "{component}/{script}.py imports unchanged and subclasses \
                     BigWorld.{base_name}"
                );
            }
        });
    }

    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_the_cell_scripts_import_unchanged() {
        demo_scripts_import_unchanged("cell");
    }

    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_the_base_scripts_import_unchanged() {
        demo_scripts_import_unchanged("base");
    }

    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_the_client_scripts_import_unchanged() {
        demo_scripts_import_unchanged("client");
    }

    /// `Proxy` stands on `Base` (`Py_Header( Proxy, Base )`,
    /// `proxy.hpp:76`) and both are acceptable bases: the subclass check a
    /// `class Account(BigWorld.Proxy)` statement performs, plus the MRO
    /// relation the statement records.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_proxy_stands_on_base_and_both_subclass() {
        interpreter_scenario("base", || {
            let answer = probe(
                "import BigWorld\n\
                 __probe = repr([\n\
                 \x20 issubclass(BigWorld.Proxy, BigWorld.Base),\n\
                 \x20 BigWorld.Proxy.__bases__,\n\
                 \x20 BigWorld.Base.__bases__,\n\
                 ])",
            );
            assert_eq!(
                answer,
                "[True, (<class 'BigWorld.Base'>,), (<class 'object'>,)]"
            );
        });
    }

    /// `Entity` sits directly on `object` (`PyObjectPlus` is not ported --
    /// the registered delta), in the cell role that registers it.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_entity_sits_on_object() {
        interpreter_scenario("cell", || {
            let answer = probe(
                "import BigWorld\n\
                 __probe = repr(BigWorld.Entity.__bases__)",
            );
            assert_eq!(answer, "(<class 'object'>,)");
        });
    }

    /// Direct construction is refused with the reference's message
    /// (`_pyNew`, `pyobject_plus.cpp:150`): the engine owns every
    /// instance.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_direct_construction_is_refused() {
        interpreter_scenario("cell", || {
            let answer = probe(
                "import BigWorld\n\
                 try:\n\
                 \x20 BigWorld.Entity()\n\
                 except TypeError as error:\n\
                 \x20 __probe = str(error)\n\
                 else:\n\
                 \x20 __probe = 'constructed'",
            );
            assert_eq!(
                answer,
                "BigWorld.Entity() Cannot directly construct objects of this type"
            );
        });
    }

    /// A script `class` statement over a ported base behaves like the
    /// reference's: the MRO records both `Base` and `Proxy`, and the
    /// fixture idiom `BigWorld.Proxy.__init__(self)` -- the statement
    /// `Account.py` makes at import -- resolves to the inherited
    /// `object.__init__` no-op. Script-side construction refuses with the
    /// refusal the class inherits (`_pyNew`, `pyobject_plus.cpp:148`), the
    /// faithful shape: the engine builds the instances itself through
    /// `PyType_GenericAlloc` straight on the script class
    /// (`server/baseapp/entity_type.cpp:569`), never through `tp_new`.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_a_script_class_over_the_base_behaves() {
        interpreter_scenario("base", || {
            let answer = probe(
                "import BigWorld\n\
                 class Account(BigWorld.Proxy):\n\
                 \x20 def __init__(self):\n\
                 \x20\x20 BigWorld.Proxy.__init__(self)\n\
                 try:\n\
                 \x20 Account()\n\
                 except TypeError as error:\n\
                 \x20 refusal = str(error)\n\
                 else:\n\
                 \x20 refusal = 'constructed'\n\
                 __probe = repr([\n\
                 \x20 issubclass(Account, BigWorld.Proxy),\n\
                 \x20 issubclass(Account, BigWorld.Base),\n\
                 \x20 BigWorld.Proxy.__init__ is object.__init__,\n\
                 ]) + ' | ' + repr(refusal)",
            );
            assert_eq!(
                answer,
                "[True, True, True] | \
                 'Account() Cannot directly construct objects of this type'"
            );
        });
    }

    /// The roles that register nothing stay clean: `database` and an
    /// unknown name carry no class rows at all, the absence the manifest's
    /// audit holds them to.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_a_role_without_the_class_rows_stays_clean() {
        for component in ["database", "mystery"] {
            interpreter_scenario(component, || {
                let answer = probe(
                    "import BigWorld\n\
                     __probe = repr([n for n in dir(BigWorld) \
                     if n in ('Entity', 'Base', 'Proxy')])",
                );
                assert_eq!(answer, "[]", "{component} registers no script class");
            });
        }
    }

    /// The manifest stays the single source of the per-role split: the
    /// class rows and the role surfaces agree, and the database app --
    /// which registers nothing in the reference -- carries no class row.
    #[test]
    fn the_manifest_rows_carry_the_registration_split() {
        let big_world = manifest::find("BigWorld").expect("BigWorld");
        let classes: Vec<_> = big_world
            .items
            .iter()
            .filter(|item| {
                item.kind == manifest::Kind::Class && item.status == manifest::Status::Ported
            })
            .map(|item| (item.name, item.scope.as_str(), item.status.as_str()))
            .collect();
        assert_eq!(
            classes,
            vec![
                ("Entity", "entity-apps", "ported"),
                ("Base", "baseapp", "ported"),
                ("Proxy", "baseapp", "ported"),
            ]
        );
        for (component, expected) in [
            ("cell", vec!["Entity"]),
            ("bot", vec!["Entity"]),
            ("client", vec!["Entity"]),
            ("base", vec!["Base", "Proxy"]),
            ("database", vec![]),
            ("editor", vec![]),
        ] {
            let mut names: Vec<_> = ported_surface_for_role(component)
                .into_iter()
                .filter(|name| matches!(*name, "Entity" | "Base" | "Proxy"))
                .collect();
            names.sort_unstable();
            assert_eq!(names, expected, "{component}");
        }
    }
}
