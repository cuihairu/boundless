//! `PluginLoader` port (`tools/plugin_system/plugin_loader.{hpp,cpp}` plus
//! the `plugin_main.cpp` pattern every converter plugin follows): the
//! registration face that loads a tool's plugins and lets each one register
//! its converters with the compiler.
//!
//! Reference quirks preserved:
//! - `initPlugins` reads a plugin list file (`<exe>_plugins.txt`) and loads
//!   each whitespace-separated name in order; a missing file is an error
//!   message and a no-op;
//! - `loadPlugin` resolves the plugin by name, runs its `PluginInit` and
//!   only records the plugin when the init succeeded; a failed init runs
//!   `PluginFini` before the plugin is dropped;
//! - `finiPlugins` unloads in reverse load order, and `unloadPlugin` keeps
//!   the plugin alive when its fini reports failure;
//! - a plugin registers its converters through the loader it is handed
//!   (`compiler->registerConverter( info )` — the `PluginLoader&` the init
//!   receives *is* the compiler, via the JIT compiler's multiple
//!   inheritance), using the `INIT_CONVERTER_INFO` fields: name, unique
//!   type id, version and the `CONVERTER_FLAGS` set.
//!
//! Port deviation (structural, behaviour preserved): the reference loads
//! Windows DLLs (`LoadLibrary` + `GetProcAddress("PluginInit")`). Rust has
//! no dynamic-library plugin boundary in this port, so a plugin is an
//! in-process entry in the [`PluginLoader`] registry — `register_plugin`
//! plays the DLL's exported symbol and `load_plugin` runs it. The
//! `PluginLoader&`-is-a-compiler face becomes the [`ConverterRegistrar`]
//! trait, which [`AssetCompiler`](crate::asset_compiler::AssetCompiler)
//! implements.

use std::cell::RefCell;

use crate::converter_info::ConverterInfo;

/// `ConverterRegistrar`: the `registerConverter` face the plugin init
/// receives (the reference's `PluginLoader&` narrowed to its
/// `AssetCompiler` base).
pub trait ConverterRegistrar {
    /// `registerConverter( converterInfo )`.
    fn register_converter(&mut self, info: ConverterInfo);
}

/// `PluginInitFunc`: `bool PluginInit( PluginLoader & pluginLoader )`.
pub type PluginInitFunc = fn(&mut dyn ConverterRegistrar) -> bool;
/// `PluginFiniFunc`: `bool PluginFini( PluginLoader & pluginLoader )`.
pub type PluginFiniFunc = fn(&mut dyn ConverterRegistrar) -> bool;

/// A plugin entry in the registry (the DLL's exported symbols).
pub struct PluginEntry {
    /// The plugin's name (the file stem the list file references).
    pub name: String,
    /// `PluginInit`.
    pub init: PluginInitFunc,
    /// `PluginFini` (optional in the reference — a plugin without the
    /// export is unloadable but still loadable).
    pub fini: Option<PluginFiniFunc>,
}

/// A loaded plugin (the reference's `HMODULE` handle plus its name).
struct LoadedPlugin {
    name: String,
    fini: Option<PluginFiniFunc>,
}

/// `PluginLoader`: the plugin registry and loader (see the module docs).
#[derive(Default)]
pub struct PluginLoader {
    /// The in-process plugin registry (the set of available DLLs).
    entries: Vec<PluginEntry>,
    /// `plugins_`: the loaded handles, in load order.
    plugins: Vec<LoadedPlugin>,
    /// `pluginNames_`.
    plugin_names: Vec<String>,
    /// The init failure messages (the reference logs `ERROR_MSG`s; kept
    /// here so the driver can surface them).
    errors: RefCell<Vec<String>>,
}

impl PluginLoader {
    /// `PluginLoader()`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an in-process plugin (the DLL build step).
    pub fn register_plugin(
        &mut self,
        name: impl Into<String>,
        init: PluginInitFunc,
        fini: Option<PluginFiniFunc>,
    ) {
        self.entries.push(PluginEntry {
            name: name.into(),
            init,
            fini,
        });
    }

    /// `initPlugins( configFile )`: load every plugin named in the
    /// whitespace-separated list file (the reference's
    /// `<exe>_plugins.txt`). A missing file is an error and a no-op.
    pub fn init_plugins(&mut self, config_file: &str, registrar: &mut dyn ConverterRegistrar) {
        let Ok(contents) = std::fs::read_to_string(config_file) else {
            self.errors
                .borrow_mut()
                .push(format!("Could not open plugin list file {config_file}"));
            return;
        };
        for name in contents.split_whitespace() {
            self.load_plugin(name, registrar);
        }
    }

    /// `loadPlugin( pluginName )`: run the plugin's init against
    /// `registrar`; a successful init records the plugin (and its name).
    pub fn load_plugin(&mut self, plugin_name: &str, registrar: &mut dyn ConverterRegistrar) {
        let Some(entry) = self
            .entries
            .iter()
            .find(|entry| entry.name == plugin_name)
            .map(|entry| (entry.init, entry.fini))
        else {
            // The reference's LoadLibrary failure: a plain skip.
            self.errors
                .borrow_mut()
                .push(format!("plugin file {plugin_name} not found"));
            return;
        };
        let (init, fini) = entry;

        if init(registrar) {
            self.plugins.push(LoadedPlugin {
                name: plugin_name.to_string(),
                fini,
            });
            self.plugin_names.push(plugin_name.to_string());
            return;
        }

        // A failed init still runs the fini (the reference's cleanup path).
        if let Some(fini) = fini {
            fini(registrar);
        }
    }

    /// `unloadPlugin`: run the plugin's fini and drop it. `false` when the
    /// fini reported failure (the plugin stays loaded, like the reference).
    pub fn unload_plugin(
        &mut self,
        plugin_name: &str,
        registrar: &mut dyn ConverterRegistrar,
    ) -> bool {
        let Some(index) = self
            .plugins
            .iter()
            .position(|plugin| plugin.name == plugin_name)
        else {
            return true;
        };

        if let Some(fini) = self.plugins[index].fini
            && !fini(registrar)
        {
            return false;
        }

        let plugin = self.plugins.remove(index);
        if let Some(name_index) = self
            .plugin_names
            .iter()
            .position(|name| *name == plugin.name)
        {
            self.plugin_names.remove(name_index);
        }
        true
    }

    /// `finiPlugins`: unload in reverse load order; stops at the first fini
    /// that reports failure (the reference returns early).
    pub fn fini_plugins(&mut self, registrar: &mut dyn ConverterRegistrar) {
        while let Some(plugin) = self.plugins.last() {
            let name = plugin.name.clone();
            if !self.unload_plugin(&name, registrar) {
                return;
            }
        }
    }

    /// `pluginNames`.
    pub fn plugin_names(&self) -> &[String] {
        &self.plugin_names
    }

    /// The accumulated init/load error messages (the reference's
    /// `ERROR_MSG` stream, captured for the driver).
    pub fn errors(&self) -> Vec<String> {
        self.errors.borrow().clone()
    }
}

/// A no-op registrar for tests and fini-only unloads (the reference's fini
/// also receives the loader; a plugin that only registers on init needs no
/// registrar at fini time).
pub struct NullRegistrar;

impl ConverterRegistrar for NullRegistrar {
    fn register_converter(&mut self, _info: ConverterInfo) {}
}

/// The `PluginLoader&`-is-a-compiler face: a plugin's init registers its
/// converters through the loader it is handed, which in the reference is
/// the compiler itself (the JIT compiler's multiple inheritance). Here the
/// [`AssetCompiler`](crate::asset_compiler::AssetCompiler) narrows to the
/// `registerConverter` call.
impl ConverterRegistrar for crate::asset_compiler::AssetCompiler {
    fn register_converter(&mut self, info: ConverterInfo) {
        crate::asset_compiler::AssetCompiler::register_converter(self, info);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler::Compiler;
    use crate::converter::Converter;
    use crate::converter_info::ConverterFlags;
    use crate::dependency_list::DependencyList;

    /// The test converter the plugin registers (an empty one; the registry
    /// only needs the info record).
    struct TestConverter;

    impl Converter for TestConverter {
        fn create_dependencies(
            &self,
            _source_file: &str,
            _compiler: &dyn Compiler,
            _dependencies: &mut DependencyList<&dyn Compiler>,
        ) -> bool {
            true
        }
        fn convert(
            &self,
            _source_file: &str,
            _compiler: &dyn Compiler,
            _intermediate_files: &mut Vec<String>,
            _output_files: &mut Vec<String>,
        ) -> bool {
            true
        }
    }

    /// The `plugin_main.cpp` pattern: init registers the converter info,
    /// fini clears it.
    static INIT_CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    static FINI_CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    fn plugin_init(registrar: &mut dyn ConverterRegistrar) -> bool {
        INIT_CALLS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        registrar.register_converter(ConverterInfo::new(
            "test",
            0xBEEF,
            "1.0",
            ConverterFlags::DEFAULT_FLAGS,
            |_| Box::new(TestConverter),
        ));
        true
    }

    fn failing_init(_registrar: &mut dyn ConverterRegistrar) -> bool {
        false
    }

    fn succeeding_init(_registrar: &mut dyn ConverterRegistrar) -> bool {
        true
    }

    fn plugin_fini(_registrar: &mut dyn ConverterRegistrar) -> bool {
        FINI_CALLS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        true
    }

    fn failing_fini(_registrar: &mut dyn ConverterRegistrar) -> bool {
        false
    }

    /// A registrar collecting registered infos (the fixture's compiler).
    #[derive(Default)]
    struct CollectingRegistrar {
        registered: RefCell<Vec<ConverterInfo>>,
    }

    impl ConverterRegistrar for CollectingRegistrar {
        fn register_converter(&mut self, info: ConverterInfo) {
            self.registered.borrow_mut().push(info);
        }
    }

    /// Loading a listed plugin runs its init and registers the converter
    /// (the `plugin_main.cpp` pattern, queryable afterwards).
    #[test]
    fn init_plugins_registers_converters_from_the_list_file() {
        let dir = std::env::temp_dir().join(format!("bw_plugin_list_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let config = dir.join("jit_plugins.txt");
        std::fs::write(&config, "test_converter extra").unwrap();

        let mut loader = PluginLoader::new();
        loader.register_plugin("test_converter", plugin_init, Some(plugin_fini));
        let mut registrar = CollectingRegistrar::default();
        loader.init_plugins(config.to_str().unwrap(), &mut registrar);

        // The named plugin loaded; the unknown one produced an error.
        assert_eq!(loader.plugin_names(), ["test_converter".to_string()]);
        assert_eq!(registrar.registered.borrow().len(), 1);
        let info = &registrar.registered.borrow()[0];
        assert_eq!(info.name, "test");
        assert_eq!(info.type_id, 0xBEEF);
        assert_eq!(info.version, "1.0");
        assert!(
            loader
                .errors()
                .iter()
                .any(|error| error.contains("not found"))
        );

        // finiPlugins unloads in reverse order (one plugin: one fini).
        loader.fini_plugins(&mut NullRegistrar);
        assert!(loader.plugin_names().is_empty());
    }

    /// A missing list file is an error message and a no-op.
    #[test]
    fn a_missing_list_file_is_a_no_op() {
        let mut loader = PluginLoader::new();
        loader.register_plugin("test_converter", plugin_init, Some(plugin_fini));
        let mut registrar = CollectingRegistrar::default();
        // The init counter is a shared static (parallel tests move it), so
        // the assertion is a monotonic delta like the failed-init test.
        let inits_before = INIT_CALLS.load(std::sync::atomic::Ordering::SeqCst);
        loader.init_plugins("/nonexistent/path/plugins.txt", &mut registrar);
        assert!(loader.plugin_names().is_empty());
        assert_eq!(
            INIT_CALLS.load(std::sync::atomic::Ordering::SeqCst),
            inits_before
        );
        assert!(
            loader
                .errors()
                .iter()
                .any(|error| error.contains("Could not open plugin list file"))
        );
    }

    /// A failed init runs the fini and leaves the plugin unloaded (the
    /// call counters are shared statics, so the assertions are monotonic
    /// deltas — parallel tests may also move them).
    #[test]
    fn a_failed_init_runs_fini_and_stays_unloaded() {
        let mut loader = PluginLoader::new();
        loader.register_plugin("bad", failing_init, Some(plugin_fini));
        let mut registrar = CollectingRegistrar::default();

        let finis_before = FINI_CALLS.load(std::sync::atomic::Ordering::SeqCst);
        loader.load_plugin("bad", &mut registrar);
        assert!(loader.plugin_names().is_empty());
        assert!(
            FINI_CALLS.load(std::sync::atomic::Ordering::SeqCst) > finis_before,
            "the failed init still ran the fini"
        );
        assert!(registrar.registered.borrow().is_empty());

        // unloadPlugin keeps a plugin whose fini fails (false return).
        loader.register_plugin("stubborn", succeeding_init, Some(failing_fini));
        loader.load_plugin("stubborn", &mut registrar);
        assert_eq!(loader.plugin_names(), ["stubborn".to_string()]);
        assert!(!loader.unload_plugin("stubborn", &mut NullRegistrar));
        assert_eq!(
            loader.plugin_names(),
            ["stubborn".to_string()],
            "a failing fini keeps the plugin loaded"
        );
    }

    /// A successful load + manual unload round-trip.
    #[test]
    fn load_then_unload_round_trips() {
        let mut loader = PluginLoader::new();
        loader.register_plugin("test_converter", plugin_init, Some(plugin_fini));
        let mut registrar = CollectingRegistrar::default();

        loader.load_plugin("test_converter", &mut registrar);
        assert!(loader.unload_plugin("test_converter", &mut NullRegistrar));
        assert!(loader.plugin_names().is_empty());
    }
}
