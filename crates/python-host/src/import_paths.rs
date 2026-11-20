//! `PyImportPaths` and the `sys.path` assembly `Script::init` does
//! (`lib/pyscript/py_import_paths.cpp`, `script.cpp`).
//!
//! The reference builds `sys.path` from the app's own `PyImportPaths` plus a
//! fixed set of resource-tree paths, expands every resource path across all
//! `BWResource` roots, deduplicates, and hands Python a list:
//!
//! ```cpp
//! PyImportPaths sysPaths( DELIM );
//! sysPaths.append( appPaths );
//! sysPaths.addResPath( commonPath );            // scripts/common
//! sysPaths.addResPath( commonPath + "/Lib" );   // scripts/common/Lib
//! sysPaths.addResPath( serverCommonPath );      // scripts/server_common/...
//! PySys_SetObject( "path", sysPaths.pathAsObject() );
//! ```
//!
//! That is the whole of "the original scripts import as they are": the
//! scripts the engine ships live under those resource paths, not in the
//! interpreter's standard-library directories.

use super::capi;
use super::pyref::PyRef;

/// `EntityDef::Constants::commonPath()`.
pub const COMMON_PATH: &str = "scripts/common";
/// `EntityDef::Constants::serverCommonPath()`.
pub const SERVER_COMMON_PATH: &str = "scripts/server_common";
/// `EntityDef::Constants::entitiesClientPath()`.
pub const ENTITIES_CLIENT_PATH: &str = "scripts/client";

/// `PyImportPaths`: the ordered, deduplicated import path list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportPaths {
    paths: Vec<String>,
    delimiter: char,
}

impl Default for ImportPaths {
    /// A fresh collection uses the platform delimiter, so a defaulted value
    /// never carries a NUL separator.
    fn default() -> Self {
        Self::new(Self::default_delimiter())
    }
}

impl ImportPaths {
    /// `PyImportPaths( delimiter )`.
    pub fn new(delimiter: char) -> Self {
        Self {
            paths: Vec::new(),
            delimiter,
        }
    }

    /// The platform delimiter `Script::init` builds its list with.
    ///
    /// The reference passes `DELIM` from CPython's `Include/osdefs.h`: `';'`
    /// on Windows and VxWorks, and nothing at all anywhere else, where it
    /// falls back to `PyImportPaths::DEFAULT_DELIMITER`, also `';'`. Python's
    /// own `os.pathsep` is `':'` on unix, which is what a unix `sys.path`
    /// string actually wants, so the port uses that instead of the reference's
    /// literal `';'` here -- recorded, not hidden.
    pub fn default_delimiter() -> char {
        if cfg!(windows) { ';' } else { ':' }
    }

    /// The delimiter `paths_as_string` joins with.
    pub fn delimiter(&self) -> char {
        self.delimiter
    }

    /// `setDelimiter`.
    pub fn set_delimiter(&mut self, delimiter: char) {
        self.delimiter = delimiter
    }

    /// `empty`.
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    /// `addNonResPath`: a path already present is not added again.
    pub fn add_non_res_path(&mut self, path: &str) {
        if !self.paths.iter().any(|known| known == path) {
            self.paths.push(path.to_owned());
        }
    }

    /// `addResPath`: a path inside the res tree, joined onto every root.
    ///
    /// The reference reads the roots out of `BWResource`; the port takes them
    /// as an argument so the assembly stays testable without a live
    /// `BWResource`.
    pub fn add_res_path(&mut self, roots: &[&str], path: &str) {
        for root in roots {
            self.add_non_res_path(&format!("{root}/{path}"));
        }
    }

    /// `append`: the other collection's paths come after this one's.
    pub fn append(&mut self, other: &ImportPaths) {
        for path in &other.paths {
            self.add_non_res_path(path);
        }
    }

    /// `pathsAsString`: the list joined by the delimiter.
    pub fn paths_as_string(&self) -> String {
        self.paths.join(self.delimiter.to_string().as_str())
    }

    /// `pathAsObject`: one unicode string per path, in order.
    ///
    /// The reference skips a path whose conversion fails and does not
    /// backfill the gap, so the list can come back shorter than the path
    /// list; the same rule is kept here.
    ///
    /// A path that cannot be encoded is not skipped: the reference's
    /// `bw_utf8tow` cannot fail, so the only reachable difference is a caller
    /// handing in a string with an embedded NUL, and a path list silently
    /// missing an entry is worse than reporting it. Such a string yields the
    /// empty handle, and the caller then reports the failed `sys.path`
    /// assignment.
    ///
    /// # Safety
    ///
    /// The interpreter must be up and the caller must own the GIL.
    pub unsafe fn path_as_object(&self) -> PyRef {
        let Some(encoded_paths) = self.encoded_paths() else {
            return PyRef::empty();
        };
        unsafe {
            let list = capi::PyList_New(encoded_paths.len() as capi::PySsize);
            if list.is_null() {
                return PyRef::empty();
            }
            for (index, encoded) in encoded_paths.iter().enumerate() {
                let item = capi::PyUnicode_FromString(encoded.as_ptr());
                if item.is_null() || capi::PyList_SetItem(list, index as capi::PySsize, item) != 0 {
                    // `PyList_SetItem` steals the item on success only.
                    capi::dec_ref(item);
                    capi::dec_ref(list);
                    return PyRef::empty();
                }
            }
            PyRef::from_owned(list).unwrap_or_default()
        }
    }

    /// Every path as a C string, or `None` when one carries an embedded NUL.
    fn encoded_paths(&self) -> Option<Vec<std::ffi::CString>> {
        self.paths
            .iter()
            .map(|path| std::ffi::CString::new(path.as_str()).ok())
            .collect()
    }
}

/// The reference's `#if !BWCLIENT_AS_PYTHON_MODULE` block: the app's own
/// paths first, then the resource-tree paths the engine's scripts live in.
///
/// `platform` is `BW::PlatformInfo::buildStr()`; `config` is
/// `BW_COMPILE_TIME_CONFIG` and `None` stands for `BW_CONFIG_HYBRID`, the one
/// value the reference does not append to the path. The reference also warns
/// when `platform` differs from the compile-time platform; that is a logging
/// concern and lands with the script-logging item.
pub fn role_paths(
    app_paths: &ImportPaths,
    roots: &[&str],
    platform: &str,
    config: Option<&str>,
) -> ImportPaths {
    let mut sys_paths = ImportPaths::new(ImportPaths::default_delimiter());
    sys_paths.append(app_paths);
    sys_paths.add_res_path(roots, COMMON_PATH);
    sys_paths.add_res_path(roots, &format!("{COMMON_PATH}/Lib"));
    #[cfg(windows)]
    {
        sys_paths.add_res_path(roots, &format!("{ENTITIES_CLIENT_PATH}/DLLs/{platform}"));
    }
    #[cfg(not(windows))]
    {
        let mut server_common = format!("{SERVER_COMMON_PATH}/lib-dynload-{platform}");
        if let Some(config) = config {
            server_common.push('_');
            server_common.push_str(config);
        }
        sys_paths.add_res_path(roots, &server_common);
    }
    sys_paths
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An empty collection renders as the empty string and reports empty.
    #[test]
    fn the_empty_collection_renders_empty() {
        let paths = ImportPaths::new(ImportPaths::default_delimiter());
        assert!(paths.is_empty());
        assert_eq!(paths.paths_as_string(), String::new());
    }

    /// `addNonResPath` deduplicates in place and keeps insertion order.
    #[test]
    fn paths_are_deduplicated_in_order() {
        let mut paths = ImportPaths::new(';');
        paths.add_non_res_path("alpha");
        paths.add_non_res_path("beta");
        paths.add_non_res_path("alpha");
        assert_eq!(paths.paths_as_string(), "alpha;beta");
    }

    /// `setDelimiter` only changes how the list renders, not its content.
    #[test]
    fn the_delimiter_only_affects_the_string_form() {
        let mut paths = ImportPaths::new(';');
        paths.add_non_res_path("alpha");
        paths.add_non_res_path("beta");
        paths.set_delimiter(':');
        assert_eq!(paths.delimiter(), ':');
        assert_eq!(paths.paths_as_string(), "alpha:beta");
    }

    /// `append` puts the other collection's paths after this one's, still
    /// deduplicated.
    #[test]
    fn append_keeps_the_other_paths_last() {
        let mut paths = ImportPaths::new(';');
        paths.add_non_res_path("alpha");
        let mut other = ImportPaths::new(';');
        other.add_non_res_path("alpha");
        other.add_non_res_path("beta");
        other.add_non_res_path("gamma");
        paths.append(&other);
        assert_eq!(paths.paths_as_string(), "alpha;beta;gamma");
    }

    /// `addResPath` joins the path onto every root, in root order.
    #[test]
    fn a_resource_path_expands_over_every_root() {
        let mut paths = ImportPaths::new(':');
        paths.add_res_path(&["/res/a", "/res/b"], "scripts/common");
        assert_eq!(
            paths.paths_as_string(),
            "/res/a/scripts/common:/res/b/scripts/common"
        );
    }

    /// A path already present through another root is not repeated.
    #[test]
    fn expanded_roots_are_deduplicated_together() {
        let mut paths = ImportPaths::new(':');
        paths.add_res_path(&["/res", "/res"], COMMON_PATH);
        assert_eq!(paths.paths_as_string(), "/res/scripts/common");
    }

    /// The reference's `#else` branch: the server common path carries the
    /// platform and, for non-hybrid builds, the compile-time config.
    #[cfg(not(windows))]
    #[test]
    fn role_paths_carry_the_platform_and_config() {
        let mut app = ImportPaths::new(':');
        app.add_non_res_path("/app/scripts");
        let assembled = role_paths(&app, &["/res"], "linux-x86-64", Some("server"));
        assert_eq!(
            assembled.paths_as_string(),
            "/app/scripts:/res/scripts/common:/res/scripts/common/Lib:/res/scripts/server_common/lib-dynload-linux-x86-64_server",
        );
    }

    /// `BW_CONFIG_HYBRID` is the one config the reference does not append.
    #[cfg(not(windows))]
    #[test]
    fn hybrid_config_adds_no_suffix() {
        let app = ImportPaths::new(':');
        let assembled = role_paths(&app, &["/res"], "linux-x86-64", None);
        assert_eq!(
            assembled.paths_as_string(),
            "/res/scripts/common:/res/scripts/common/Lib:/res/scripts/server_common/lib-dynload-linux-x86-64",
        );
    }

    /// The default collection already carries the platform delimiter, so a
    /// defaulted value never joins on a NUL.
    #[test]
    fn the_default_collection_carries_the_platform_delimiter() {
        let mut paths = ImportPaths::default();
        paths.add_non_res_path("alpha");
        paths.add_non_res_path("beta");
        assert_eq!(paths.delimiter(), ImportPaths::default_delimiter());
        let separator = if cfg!(windows) { ';' } else { ':' };
        assert_eq!(paths.paths_as_string(), format!("alpha{separator}beta"));
    }
}
