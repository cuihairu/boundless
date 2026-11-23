//! The reference plugin host, ported to `dlopen`/`dlsym`.
//!
//! Source mapping (`programming/bigworld/tools/plugin_system/`):
//! - `plugin_loader.hpp`: the `PluginLoader` interface (`initPlugins`,
//!   `finiPlugins`, `loadPlugin`, `unloadPlugin`, `pluginNames`).
//! - `plugin_loader.cpp`: the behaviour this module reproduces — `loadPlugin`
//!   keeps a plugin only when its init entry point exists and returns true; a
//!   missing library, a missing init entry, or an init that returns false all
//!   leave nothing loaded (and an absent init still runs the plugin's fini
//!   when the library exports one); `unloadPlugin` runs fini first and lets a
//!   fini that returns false veto the unload; `finiPlugins` unloads in
//!   reverse load order; `initPlugins` reads the plugin list
//!   `<exe dir>/<exe basename>_plugins.txt`.
//! - `plugin.hpp`: the `PLUGIN_INIT`/`PLUGIN_FINI` macros — under `_DEBUG`
//!   the exported names gain a `_d` suffix (`PluginInit_d`), selected here
//!   with [`PluginLoader::with_debug_symbols`].
//!
//! New-tree deviations, both recorded against the reference:
//! - The reference is Windows-only (`LoadLibraryW`/`GetProcAddress`). This
//!   host loads through `platform::mmap::DynamicLibrary` (`dlopen`/`dlsym`),
//!   so the `.dll` suffix becomes `.so` (BW-15-01-05; the Windows
//!   `LoadLibrary` path is BW-15-03).
//! - `loadPlugin` reports every failure as a single NULL; this port returns a
//!   distinct [`LoadError`] so each failure path is reproducible, which is the
//!   acceptance of BW-13-06-03.

use std::ffi::c_void;
use std::fmt;
use std::io;
use std::path::Path;

use platform::mmap::DynamicLibrary;

/// The init entry point a plugin exports to join the host. `host` is the
/// loader as an opaque context (the reference's `PluginLoader&`), valid for
/// the duration of the call.
pub type PluginInitFn = unsafe extern "C" fn(*mut c_void) -> bool;

/// The fini entry point a plugin exports to leave the host.
pub type PluginFiniFn = unsafe extern "C" fn(*mut c_void) -> bool;

/// The release entry point names: the reference's non-`_DEBUG` `PLUGIN_INIT`.
pub const INIT_SYMBOL: &str = "plugin_init";
pub const FINI_SYMBOL: &str = "plugin_fini";

/// The debug entry point names: the reference's `_DEBUG` `PluginInit_d`.
pub const DEBUG_INIT_SYMBOL: &str = "plugin_init_d";
pub const DEBUG_FINI_SYMBOL: &str = "plugin_fini_d";

/// A plugin the host holds loaded, released when it leaves the list. The
/// library paths live in the loader's parallel name list, mirroring the
/// reference's `plugins_`/`pluginNames_` pair.
struct LoadedPlugin {
    lib: DynamicLibrary,
}

/// Why a plugin failed to load, so each reference failure mode is distinct.
#[derive(Debug)]
pub enum LoadError {
    /// `dlopen` rejected the library (a missing file, an unsatisfied
    /// dependency, ...).
    Open(io::Error),
    /// The library exports no init entry point with the requested name.
    NoInit(io::Error),
    /// The init entry point ran and refused to join the host.
    InitVetoed,
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoadError::Open(err) => write!(f, "plugin host could not open the library: {err}"),
            LoadError::NoInit(err) => {
                write!(f, "plugin host found no init entry point: {err}")
            }
            LoadError::InitVetoed => write!(f, "plugin host was refused by the plugin's init"),
        }
    }
}

impl std::error::Error for LoadError {}

/// The host: resolves and invokes plugin entry points, keeping a plugin only
/// when its init accepts the host.
pub struct PluginLoader {
    init_symbol: String,
    fini_symbol: String,
    plugins: Vec<LoadedPlugin>,
    names: Vec<String>,
}

impl PluginLoader {
    /// A loader using the release entry point names.
    pub fn new() -> Self {
        Self::with_symbols(INIT_SYMBOL, FINI_SYMBOL)
    }

    /// A loader using the debug `_d` entry point names (the reference's
    /// `_DEBUG` `PLUGIN_INIT`/`PLUGIN_FINI`).
    pub fn with_debug_symbols() -> Self {
        Self::with_symbols(DEBUG_INIT_SYMBOL, DEBUG_FINI_SYMBOL)
    }

    /// A loader with explicit entry point names, for tests and for hosts that
    /// rename the exported symbols.
    pub fn with_symbols(init_symbol: &str, fini_symbol: &str) -> Self {
        Self {
            init_symbol: init_symbol.to_owned(),
            fini_symbol: fini_symbol.to_owned(),
            plugins: Vec::new(),
            names: Vec::new(),
        }
    }

    /// The library paths of the loaded plugins, in load order.
    pub fn plugin_names(&self) -> &[String] {
        &self.names
    }

    /// How many plugins are currently held loaded.
    pub fn len(&self) -> usize {
        self.plugins.len()
    }

    /// Whether no plugin is currently held loaded.
    pub fn is_empty(&self) -> bool {
        self.plugins.is_empty()
    }

    /// `loadPlugin(pluginName)`: open `lib_path`, resolve the init entry point
    /// and call it. On success the plugin is held and its index returned. On
    /// any failure the plugin's fini is run when it exports one and the
    /// library released, mirroring the reference's rollback.
    pub fn load_plugin(&mut self, lib_path: &str) -> Result<usize, LoadError> {
        let lib = DynamicLibrary::open(lib_path).map_err(LoadError::Open)?;
        let init = match resolve_init(&lib, &self.init_symbol) {
            Ok(init) => init,
            Err(err) => {
                self.run_fini(&lib);
                return Err(LoadError::NoInit(err));
            }
        };
        if !unsafe { init(self.context()) } {
            self.run_fini(&lib);
            return Err(LoadError::InitVetoed);
        }
        let index = self.plugins.len();
        self.plugins.push(LoadedPlugin { lib });
        self.names.push(lib_path.to_owned());
        Ok(index)
    }

    /// `unloadPlugin(hPlugin)`: run the fini entry point first; a fini that
    /// returns false vetoes the unload and the plugin stays loaded. An index
    /// that was never loaded is a no-op success, as in the reference.
    pub fn unload_plugin(&mut self, index: usize) -> bool {
        if index >= self.plugins.len() {
            return true;
        }
        let vetoed = {
            let plugin = &self.plugins[index];
            match resolve_fini(&plugin.lib, &self.fini_symbol) {
                Ok(fini) => !unsafe { fini(self.context()) },
                Err(_) => false,
            }
        };
        if vetoed {
            return false;
        }
        self.plugins.remove(index);
        self.names.remove(index);
        true
    }

    /// `finiPlugins()`: unload every loaded plugin, in reverse load order.
    /// Returns the library paths released, in the order they were released.
    pub fn fini_plugins(&mut self) -> Vec<String> {
        let mut released = Vec::new();
        for index in (0..self.plugins.len()).rev() {
            let name = self.names[index].clone();
            if self.unload_plugin(index) {
                released.push(name);
            }
        }
        released
    }

    /// `initPlugins()`: read a plugin list (whitespace-separated names) and
    /// load each name resolved against `plugin_dir` as `<name>.so`. A missing
    /// list is a report, as in the reference. Returns the number loaded and
    /// the names that failed, so both outcomes are reproducible.
    pub fn init_plugins(
        &mut self,
        list_path: &Path,
        plugin_dir: &Path,
    ) -> io::Result<(usize, Vec<String>)> {
        let contents = std::fs::read_to_string(list_path)?;
        let mut loaded = 0;
        let mut failed = Vec::new();
        for name in contents.split_ascii_whitespace() {
            let path = plugin_dir.join(format!("{name}.so"));
            let path = path.to_string_lossy();
            if self.load_plugin(&path).is_ok() {
                loaded += 1;
            } else {
                failed.push(name.to_owned());
            }
        }
        Ok((loaded, failed))
    }

    /// The loader as the opaque context handed to the plugin entry points.
    fn context(&mut self) -> *mut c_void {
        self as *mut PluginLoader as *mut c_void
    }

    /// Run the plugin's fini entry point while rolling a load back; a plugin
    /// with no fini entry point simply has its library released.
    fn run_fini(&mut self, lib: &DynamicLibrary) {
        if let Ok(fini) = resolve_fini(lib, &self.fini_symbol) {
            let _ = unsafe { fini(self.context()) };
        }
    }
}

impl Default for PluginLoader {
    fn default() -> Self {
        Self::new()
    }
}

/// Resolve the init entry point `name` in `lib`, or the loader error when the
/// library does not export it. `dlsym` returns only an address; transmuting a
/// pointer to a function pointer is the portable way to call it.
fn resolve_init(lib: &DynamicLibrary, name: &str) -> io::Result<PluginInitFn> {
    let addr = lib.symbol(name)?;
    Ok(unsafe { std::mem::transmute::<*mut c_void, PluginInitFn>(addr) })
}

/// Resolve the fini entry point `name` in `lib`, or the loader error when the
/// library does not export it.
fn resolve_fini(lib: &DynamicLibrary, name: &str) -> io::Result<PluginFiniFn> {
    let addr = lib.symbol(name)?;
    Ok(unsafe { std::mem::transmute::<*mut c_void, PluginFiniFn>(addr) })
}
