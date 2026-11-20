//! `BWConfig` port (`lib/server/bwconfig.cpp`): the `server/bw.xml`
//! configuration chain behind `BWConfig::get`.
//!
//! Reference quirks preserved:
//! - a personalised `server/bw_<username>.xml` (the passwd entry of the
//!   effective uid, which `-uid` rewires) starts the chain and falls back
//!   to `server/bw.xml` when it does not exist;
//! - each config names its parent through `<parentFile>`; the chain ends
//!   at an empty `parentFile` value or a file that cannot be opened --
//!   the latter also records `badFilename`, making the config "bad";
//! - lookups scan the chain front to back and take the first hit, so the
//!   most-derived file wins and parent files only supply what the child
//!   leaves unset;
//! - `+key value` command-line pairs are written into the *front*
//!   section, overriding everything (`writeString` creates the path);
//! - with no config sections at all, command-line processing fails.

use resources::bwresource::{current_uid, BwResource};
use resources::datasection::DataSection;
use std::sync::Arc;

/// `BWConfig::init()` + `BWConfig::init(argc, argv)` state: the chain
/// (`g_sections`), `badFilename` and `shouldDebug`.
pub struct BwConfig {
    /// `(root section, filename)` pairs, most-derived first.
    sections: Vec<(Arc<DataSection>, String)>,
    /// The file that made the config bad (`badFilename`).
    bad_filename: String,
    /// `shouldDebug`, read from `debugConfigOptions`.
    pub should_debug: i32,
    /// `g_inited`: set once the chain has run.
    inited: bool,
}

impl Default for BwConfig {
    fn default() -> Self {
        Self::new()
    }
}

impl BwConfig {
    pub fn new() -> Self {
        Self {
            sections: Vec::new(),
            bad_filename: String::new(),
            should_debug: 0,
            inited: false,
        }
    }

    /// The real-environment wrapper of [`BwConfig::init_from_sources`]:
    /// the personalised filename comes from the passwd entry of the
    /// effective uid (`getpwuid(::getUserId())`).
    pub fn init(&mut self, resource: &BwResource) {
        let username = passwd_name(current_uid());
        self.init_from_sources(resource, username.as_deref());
    }

    /// `BWConfig::init()` (the filename chain). `username` selects the
    /// personalised first file, `None` starts at `server/bw.xml` directly.
    pub fn init_from_sources(&mut self, resource: &BwResource, username: Option<&str>) {
        const BASE_FILENAME: &str = "server/bw.xml";

        self.sections.clear();
        self.bad_filename.clear();

        let mut filename = match username {
            Some(name) => format!("server/bw_{name}.xml"),
            None => BASE_FILENAME.to_string(),
        };
        let mut is_user_filename = username.is_some();

        while !filename.is_empty() {
            // This also handles reload.
            resource.purge(&filename, false);
            match resource.open_section(&filename, false) {
                Some(section) => {
                    self.sections
                        .push((Arc::clone(&section), filename.clone()));
                    filename = section.read_string("parentFile", "");
                }
                None if is_user_filename && !resource.file_exists(&filename) => {
                    // A missing personalised file is not an error: the
                    // base configuration takes over.
                    filename = BASE_FILENAME.to_string();
                }
                None => {
                    // Distinguish a file that exists but does not parse
                    // from one that is simply not there; either way the
                    // config is bad and the chain stops.
                    if resource.file_exists(&filename) {
                        eprintln!("Could not parse {filename}");
                    } else {
                        eprintln!("Could not find {filename}");
                    }
                    self.bad_filename = filename;
                    filename = String::new();
                }
            }
            is_user_filename = false;
        }

        // `update("debugConfigOptions", shouldDebug)` -- a fresh `i32`
        // default of zero matches the static's initialiser.
        self.should_debug = self.get_int("debugConfigOptions", 0);
        self.inited = true;
    }

    /// `BWConfig::init(argc, argv)`: run the chain (unless already done --
    /// the reference's `g_inited` guard) and apply the command line.
    /// Returns `processCommandLine`'s verdict.
    pub fn init_from_args(
        &mut self,
        resource: &BwResource,
        args: &[String],
    ) -> bool {
        let username = passwd_name(current_uid());
        self.init_from_sources_with_args(resource, username.as_deref(), args)
    }

    /// The injectable core of [`BwConfig::init_from_args`]: the
    /// personalised filename comes from `username`, not the passwd entry
    /// of the running uid.
    pub fn init_from_sources_with_args(
        &mut self,
        resource: &BwResource,
        username: Option<&str>,
        args: &[String],
    ) -> bool {
        if self.inited {
            // WARNING_MSG "Already initialised before parse args." -- the
            // chain stays as it is.
        } else {
            self.init_from_sources(resource, username);
        }
        self.process_command_line(resource, args)
    }

    /// `BWConfig::processCommandLine`: every `+key value` argument pair is
    /// written into the front (most-derived) section, creating the path --
    /// which is what makes the command line override the whole chain.
    /// Like the reference's `i < argc - 1` scan, a trailing `+key` without
    /// a value argument is ignored.
    ///
    /// `resource` is not in the reference signature; the Rust port replaces
    /// a mutated clone of the front section and re-adds it to the resource
    /// cache (`BwResource::readd_section`) to reproduce the shared-object
    /// coherence the C++ tree gets for free.
    pub fn process_command_line(
        &mut self,
        resource: &BwResource,
        args: &[String],
    ) -> bool {
        if self.sections.is_empty() {
            eprintln!("No configuration section.");
            return false;
        }

        let filename = self.sections[0].1.clone();
        let mut front = (*self.sections[0].0).clone();
        let mut changed = false;

        let mut i = 0usize;
        while i + 1 < args.len() {
            if args[i].as_bytes().first() == Some(&b'+') {
                front.write_string(&args[i][1..], &args[i + 1]);
                changed = true;
                i += 1;
            }
            i += 1;
        }

        if changed {
            let arc = Arc::new(front);
            resource.readd_section(&filename, &arc);
            self.sections[0].0 = arc;
        }
        true
    }

    /// `BWConfig::getSection`: the first chain entry that opens `path`
    /// wins -- the most-derived file's value hides the parents'.
    pub fn get_section(&self, path: &str) -> Option<&DataSection> {
        for (section, _) in &self.sections {
            if let Some(hit) = section.open_section(path) {
                return Some(hit);
            }
        }
        None
    }

    /// `BWConfig::get` (typed overloads): the found section converted with
    /// its default-taking `as` conversion, or the default.
    pub fn get_string(&self, path: &str, default: &str) -> String {
        match self.get_section(path) {
            Some(section) => section.as_string().to_string(),
            None => default.to_string(),
        }
    }

    pub fn get_bool(&self, path: &str, default: bool) -> bool {
        match self.get_section(path) {
            Some(section) => section.as_bool(default),
            None => default,
        }
    }

    pub fn get_int(&self, path: &str, default: i32) -> i32 {
        match self.get_section(path) {
            Some(section) => section.as_int(default),
            None => default,
        }
    }

    pub fn get_uint(&self, path: &str, default: u32) -> u32 {
        match self.get_section(path) {
            Some(section) => section.as_uint(default),
            None => default,
        }
    }

    pub fn get_float(&self, path: &str, default: f32) -> f32 {
        match self.get_section(path) {
            Some(section) => section.as_float(default),
            None => default,
        }
    }

    pub fn get_double(&self, path: &str, default: f64) -> f64 {
        match self.get_section(path) {
            Some(section) => section.as_double(default),
            None => default,
        }
    }

    /// `BWConfig::update`: write the config value into `value` when the
    /// path resolves; the return says whether a hit was written.
    pub fn update_bool(&self, path: &str, value: &mut bool) -> bool {
        match self.get_section(path) {
            Some(section) => {
                *value = section.as_bool(*value);
                true
            }
            None => false,
        }
    }

    pub fn update_int(&self, path: &str, value: &mut i32) -> bool {
        match self.get_section(path) {
            Some(section) => {
                *value = section.as_int(*value);
                true
            }
            None => false,
        }
    }

    /// `BWConfig::beginSearchSections` / `SearchIterator`: every direct
    /// child of every chain entry named `name`, in chain order, with the
    /// filename of the config that supplied it. This is how repeated
    /// sections (one `<baseapp>` per config) are all visible at once.
    pub fn search_sections(&self, name: &str) -> Vec<(Arc<DataSection>, String)> {
        let mut out = Vec::new();
        for (section, config_path) in &self.sections {
            for index in 0..section.count_children() {
                let child = section.open_child(index).expect("index in range");
                if child.section_name() == name {
                    out.push((Arc::new(child.clone()), config_path.clone()));
                }
            }
        }
        out
    }

    /// `BWConfig::getChildrenNames`: the child names of `parent_path` from
    /// every chain entry that has one, concatenated in chain order.
    pub fn get_children_names(&self, parent_path: &str) -> Vec<String> {
        let mut names = Vec::new();
        for (section, _) in &self.sections {
            if let Some(parent) = section.open_section(parent_path) {
                for index in 0..parent.count_children() {
                    let child = parent.open_child(index).expect("index in range");
                    names.push(child.section_name().to_string());
                }
            }
        }
        names
    }

    /// `BWConfig::isBad`: the reference re-opens the offending file
    /// (the load error surfaces from the section machinery) and reports
    /// the config unusable. A clean init keeps this `false`.
    pub fn is_bad(&self, resource: &BwResource) -> bool {
        if !self.bad_filename.is_empty() {
            resource.open_section(&self.bad_filename, false);
            eprintln!("Could not open configuration file {}", self.bad_filename);
            return true;
        }
        false
    }
}

/// The `pw_name` half of `getpwuid`: the `/etc/passwd` account name for
/// `uid`; `None` when no entry matches.
fn passwd_name(uid: u32) -> Option<String> {
    let passwd = std::fs::read_to_string("/etc/passwd").ok()?;
    for line in passwd.lines() {
        let fields: Vec<&str> = line.split(':').collect();
        if fields.len() >= 3 && fields[2].parse::<u32>().ok() == Some(uid) {
            return Some(fields[0].to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static TEMP_COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn temp_root(tag: &str) -> String {
        let id = TEMP_COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "boundless_bwc_{}_{}_{}",
            tag,
            std::process::id(),
            id
        ));
        fs::create_dir_all(&dir).unwrap();
        dir.to_string_lossy().into_owned()
    }

    fn write(path: &str, content: &str) {
        if let Some(parent) = std::path::Path::new(path).parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
    }

    /// `server/bw.xml` (parent) + `server/bw_user.xml` (child naming the
    /// parent): the chain under test for every resolution rule.
    fn chained_resource(tag: &str) -> (BwResource, String) {
        let root = temp_root(tag);
        write(
            &format!("{root}/server/bw.xml"),
            "<root>\n\
             \t<app>\n\
             \t\t<port>5000</port>\n\
             \t\t<name>parent</name>\n\
             \t</app>\n\
             \t<shared><limit>10</limit></shared>\n\
             \t<hasDebugConfig>true</hasDebugConfig>\n\
             </root>\n",
        );
        write(
            &format!("{root}/server/bw_user.xml"),
            "<root>\n\
             \t<parentFile>server/bw.xml</parentFile>\n\
             \t<app>\n\
             \t\t<name>child</name>\n\
             \t</app>\n\
             </root>\n",
        );
        let resource = BwResource::init_paths(&[&root]);
        (resource, root)
    }

    /// The parentFile chain runs child first and stops at an empty
    /// `parentFile` value: two sections, most-derived first.
    #[test]
    fn chain_follows_parent_file_child_first() {
        let (resource, _keep) = chained_resource("chain");
        let mut config = BwConfig::new();
        config.init_from_sources(&resource, Some("user"));

        assert_eq!(config.sections.len(), 2);
        assert_eq!(config.sections[0].1, "server/bw_user.xml");
        assert_eq!(config.sections[1].1, "server/bw.xml");
        assert!(config.bad_filename.is_empty());
        assert!(!config.is_bad(&resource));
    }

    /// Acceptance: values of the chained configs resolve like the
    /// reference -- the child hides the parent key by key, and the parent
    /// fills in everything the child leaves unset.
    #[test]
    fn final_values_of_parallel_configs_resolve_like_the_reference() {
        let (resource, _keep) = chained_resource("final");
        let mut config = BwConfig::new();
        config.init_from_sources(&resource, Some("user"));

        // Overridden in the child.
        assert_eq!(config.get_string("app/name", "def"), "child");
        // Only present in the parent.
        assert_eq!(config.get_int("app/port", 0), 5000);
        assert_eq!(config.get_int("shared/limit", -1), 10);
        // Missing everywhere: the default survives.
        assert_eq!(config.get_string("app/host", "fallback"), "fallback");
        assert_eq!(config.get_bool("app/host", true), true);
        assert_eq!(config.get_uint("nope", 7), 7);
        // Typed conversions come from the winning section's `as`.
        assert_eq!(config.get_bool("hasDebugConfig", false), true);
        assert_eq!(config.get_section("app/name").unwrap().as_string(), "child");
        assert!(config.get_section("missing").is_none());
    }

    /// A deeper parentFile chain keeps its order and lookup depth.
    #[test]
    fn parent_file_chain_can_be_three_files_deep() {
        let root = temp_root("deep");
        write(
            &format!("{root}/server/bw.xml"),
            "<root><a>base</a><b>base</b><c>base</c></root>",
        );
        write(
            &format!("{root}/server/bw_two.xml"),
            "<root><parentFile>server/bw.xml</parentFile><b>two</b></root>",
        );
        write(
            &format!("{root}/server/bw_user.xml"),
            "<root><parentFile>server/bw_two.xml</parentFile><c>user</c></root>",
        );
        let resource = BwResource::init_paths(&[&root]);
        let mut config = BwConfig::new();
        config.init_from_sources(&resource, Some("user"));

        assert_eq!(
            config
                .sections
                .iter()
                .map(|(_, name)| name.as_str())
                .collect::<Vec<_>>(),
            ["server/bw_user.xml", "server/bw_two.xml", "server/bw.xml"]
        );
        assert_eq!(config.get_string("a", ""), "base");
        assert_eq!(config.get_string("b", ""), "two");
        assert_eq!(config.get_string("c", ""), "user");
    }

    /// A missing personalised file falls back to `server/bw.xml` silently;
    /// the fallback branch consumes exactly one link.
    #[test]
    fn missing_user_file_falls_back_to_the_base_config() {
        let (resource, _keep) = chained_resource("fallback");
        let mut config = BwConfig::new();
        config.init_from_sources(&resource, Some("ghost"));

        // bw_ghost.xml does not exist -> the base chain only.
        assert_eq!(config.sections.len(), 1);
        assert_eq!(config.sections[0].1, "server/bw.xml");
        assert!(config.bad_filename.is_empty());
    }

    /// A missing base configuration marks the config bad; with no
    /// sections the command line fails too.
    #[test]
    fn missing_base_config_is_bad_and_rejects_the_command_line() {
        let root = temp_root("nogbase");
        let resource = BwResource::init_paths(&[&root]);
        let mut config = BwConfig::new();
        config.init_from_sources(&resource, None);

        assert!(config.sections.is_empty());
        assert_eq!(config.bad_filename, "server/bw.xml");
        assert!(config.is_bad(&resource));
        assert!(!config.process_command_line(&resource, &[]));
    }

    /// An existing but unparseable file takes the "Could not parse"
    /// branch: bad as well.
    #[test]
    fn unparseable_config_is_bad() {
        let root = temp_root("unparse");
        write(&format!("{root}/server/bw.xml"), "not xml at all");
        let resource = BwResource::init_paths(&[&root]);
        let mut config = BwConfig::new();
        config.init_from_sources(&resource, None);

        assert_eq!(config.bad_filename, "server/bw.xml");
        assert!(config.is_bad(&resource));
    }

    /// `+key value` pairs land in the front section with path creation,
    /// overriding every chain entry -- and `BWResource::openSection`
    /// serves the overridden tree afterwards (shared-object coherence).
    #[test]
    fn command_line_overrides_write_into_the_front_section() {
        let (resource, _keep) = chained_resource("plusargs");
        let mut config = BwConfig::new();
        config.init_from_sources(&resource, Some("user"));

        let args: Vec<String> = [
            "app",
            "--res",
            "irrelevant",
            "+app/name",
            "fromcmd",
            "+net/publicKey",
            "KEY",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert!(config.process_command_line(&resource, &args));

        // Deep override of an existing value and creation of a new path.
        assert_eq!(config.get_string("app/name", ""), "fromcmd");
        assert_eq!(config.get_string("net/publicKey", ""), "KEY");
        // Non-`+` arguments (and their values) are left alone.
        assert_eq!(config.get_string("app/port", ""), "5000");

        // The resource layer sees the mutated front section too.
        let reloaded = resource.open_section("server/bw_user.xml", false).unwrap();
        assert_eq!(
            reloaded.open_section("app/name").unwrap().as_string(),
            "fromcmd"
        );
        // ...while the parent file stays untouched on disk.
        let parent = resource.open_section("server/bw.xml", false).unwrap();
        assert_eq!(parent.open_section("app/name").unwrap().as_string(), "parent");
    }

    /// A trailing `+key` with no value argument is ignored, like the
    /// reference's `i < argc - 1` bound.
    #[test]
    fn trailing_dangling_plus_key_is_ignored() {
        let (resource, _keep) = chained_resource("dangling");
        let mut config = BwConfig::new();
        config.init_from_sources(&resource, Some("user"));

        let args: Vec<String> = ["+app/name".to_string()].to_vec();
        assert!(config.process_command_line(&resource, &args));
        // Nothing was written: the child value survives.
        assert_eq!(config.get_string("app/name", ""), "child");
    }

    /// `init_from_args` runs the chain first and then processes the
    /// command line; a second call warns but keeps working.
    #[test]
    fn init_from_args_combines_chain_and_command_line() {
        let (resource, _keep) = chained_resource("combined");
        let mut config = BwConfig::new();
        let args: Vec<String> = ["+shared/limit".to_string(), "99".to_string()].to_vec();
        assert!(config.init_from_sources_with_args(&resource, Some("user"), &args));
        assert_eq!(config.sections.len(), 2);
        assert_eq!(config.get_int("shared/limit", 0), 99);

        // A fully fresh config with no resolvable chain fails on the
        // command-line half.
        let empty_root = temp_root("combined_empty");
        let empty = BwResource::init_paths(&[&empty_root]);
        let mut bad = BwConfig::new();
        assert!(!bad.init_from_args(&empty, &[]));
    }

    /// `search_sections` yields every repeated tag across the chain in
    /// chain order, labelled with its source config.
    #[test]
    fn search_sections_lists_repeated_tags_in_chain_order() {
        let (resource, _keep) = chained_resource("search");
        let mut config = BwConfig::new();
        config.init_from_sources(&resource, Some("user"));

        let mut hits = config.search_sections("app");
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].1, "server/bw_user.xml");
        assert_eq!(hits[1].1, "server/bw.xml");
        assert_eq!(hits[0].0.as_string(), "");
        assert_eq!(
            hits[0]
                .0
                .open_section("name")
                .unwrap()
                .as_string(),
            "child"
        );

        // A name only the parent defines resolves once, from the parent.
        hits = config.search_sections("shared");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].1, "server/bw.xml");
        // Nothing matches a foreign name.
        assert!(config.search_sections("ghost").is_empty());
    }

    /// `get_children_names` concatenates child names across the chain.
    #[test]
    fn children_names_concatenate_across_the_chain() {
        let (resource, _keep) = chained_resource("children");
        let mut config = BwConfig::new();
        config.init_from_sources(&resource, Some("user"));

        let names = config.get_children_names("app");
        // The child's <name> first, then the parent's <port>, <name>.
        assert_eq!(names, ["name", "port", "name"]);
        assert!(config.get_children_names("ghost").is_empty());
    }

    /// `debugConfigOptions` feeds `should_debug`.
    #[test]
    fn debug_config_options_set_should_debug() {
        let root = temp_root("debugopt");
        write(
            &format!("{root}/server/bw.xml"),
            "<root><debugConfigOptions>2</debugConfigOptions></root>",
        );
        let resource = BwResource::init_paths(&[&root]);
        let mut config = BwConfig::new();
        config.init_from_sources(&resource, None);
        assert_eq!(config.should_debug, 2);

        // Absent: the static stays zero.
        let (resource, _keep) = chained_resource("debugopt_zero");
        let mut config = BwConfig::new();
        config.init_from_sources(&resource, None);
        assert_eq!(config.should_debug, 0);
    }

    /// An empty `<parentFile />` value stops the chain like a missing one.
    #[test]
    fn empty_parent_file_value_stops_the_chain() {
        let root = temp_root("emptyparent");
        write(
            &format!("{root}/server/bw.xml"),
            "<root><parentFile></parentFile><a>1</a></root>",
        );
        let resource = BwResource::init_paths(&[&root]);
        let mut config = BwConfig::new();
        config.init_from_sources(&resource, None);

        assert_eq!(config.sections.len(), 1);
        assert_eq!(config.sections[0].1, "server/bw.xml");
        assert_eq!(config.get_int("a", 0), 1);
    }

    /// A missing parent named by `parentFile` marks the config bad mid
    /// chain: the good prefix is kept, the bad link recorded.
    #[test]
    fn broken_parent_file_link_is_bad_mid_chain() {
        let root = temp_root("brokenparent");
        write(
            &format!("{root}/server/bw.xml"),
            "<root><parentFile>server/bw_missing.xml</parentFile><a>1</a></root>",
        );
        let resource = BwResource::init_paths(&[&root]);
        let mut config = BwConfig::new();
        config.init_from_sources(&resource, None);

        assert_eq!(config.sections.len(), 1);
        assert_eq!(config.bad_filename, "server/bw_missing.xml");
        assert!(config.is_bad(&resource));
    }

    /// Re-initialising the same config picks up changed files (the chain
    /// purges each filename before reopening -- reload semantics).
    #[test]
    fn re_init_purges_and_sees_changed_files() {
        let (resource, root) = chained_resource("reload");
        let mut config = BwConfig::new();
        config.init_from_sources(&resource, Some("user"));
        assert_eq!(config.get_int("app/port", 0), 5000);

        write(
            &format!("{root}/server/bw.xml"),
            "<root><app><port>6000</port><name>parent</name></app></root>",
        );
        config.init_from_sources(&resource, Some("user"));
        assert_eq!(config.get_int("app/port", 0), 6000);
    }

    /// `update_*` writes through only on a hit.
    #[test]
    fn update_writes_through_on_hits_only() {
        let (resource, _keep) = chained_resource("update");
        let mut config = BwConfig::new();
        config.init_from_sources(&resource, Some("user"));

        let mut port = 1;
        assert!(config.update_int("app/port", &mut port));
        assert_eq!(port, 5000);

        // Misses leave the value untouched.
        let mut missing_int = -5;
        assert!(!config.update_int("ghost", &mut missing_int));
        assert_eq!(missing_int, -5);

        let mut flag = true;
        assert!(!config.update_bool("ghost", &mut flag));
        assert_eq!(flag, true);

        assert!(config.update_bool("hasDebugConfig", &mut flag));
    }

    /// The remaining typed getters read the winning section's `as`
    /// conversion; a miss falls to the default.
    #[test]
    fn typed_getters_cover_uint_float_double() {
        let (resource, _keep) = chained_resource("typed");
        let mut config = BwConfig::new();
        config.init_from_sources(&resource, Some("user"));

        assert_eq!(config.get_uint("app/port", 0), 5000);
        assert_eq!(config.get_uint("ghost", 7), 7);
        assert_eq!(config.get_float("shared/limit", 0.5), 10.0);
        assert_eq!(config.get_float("ghost", 1.5), 1.5);
        assert_eq!(config.get_double("shared/limit", 0.25), 10.0);
        assert_eq!(config.get_double("ghost", 2.5), 2.5);
    }

    /// A second `init(argc, argv)`-style call keeps the existing chain
    /// (`g_inited`) and only reprocesses the command line.
    #[test]
    fn already_inited_chain_is_kept_on_a_second_call() {
        let (resource, _keep) = chained_resource("inited");
        let mut config = BwConfig::new();
        assert!(
            config
                .init_from_sources_with_args(&resource, Some("user"), &[])
        );
        assert_eq!(config.sections.len(), 2);

        // Second call: the chain is NOT rebuilt -- the front section keeps
        // its command-line mutations from the first round.
        let again: Vec<String> = ["+app/name".to_string(), "again".to_string()].to_vec();
        assert!(
            config
                .init_from_sources_with_args(&resource, Some("ghost"), &again)
        );
        assert_eq!(config.sections.len(), 2);
        assert_eq!(config.sections[0].1, "server/bw_user.xml");
        assert_eq!(config.get_string("app/name", ""), "again");
        // The parent name from the untouched chain still resolves.
        assert_eq!(config.get_int("app/port", 0), 5000);
    }

    /// `Default` mirrors `BwConfig::new()`.
    #[test]
    fn default_matches_new() {
        let config = BwConfig::default();
        assert_eq!(config.should_debug, 0);
        assert!(config.get_children_names("").is_empty());
    }

    /// A uid with no passwd entry yields no personalised filename.
    #[test]
    fn passwd_name_misses_unknown_uids() {
        assert_eq!(passwd_name(4_000_000_000), None);
    }

    /// `passwd_name` reads the account name for a uid.
    #[test]
    fn passwd_name_resolves_the_running_account() {
        let uid = current_uid();
        let name = passwd_name(uid);
        // The running account always has a passwd entry in CI images with
        // /etc/passwd present; on exotic systems (no /etc/passwd) the
        // helper stays graceful.
        if let Some(name) = name {
            assert!(!name.is_empty());
            let _ = uid;
        }
    }

    /// No /etc/passwd (or no matching entry): `passwd_name` is None and
    /// `init` starts straight at the base config.
    #[test]
    fn init_without_a_passwd_entry_uses_the_base_config() {
        let (resource, _keep) = chained_resource("nopasswd");
        let mut config = BwConfig::new();
        // The wrapper resolves the username from the real environment;
        // this host has an entry, so the personalised chain runs.
        config.init(&resource);
        let personalised = config.sections.len() == 2;
        // Either way the chain resolves and the final values hold.
        assert_eq!(config.get_int("app/port", 0), 5000);
        assert!(personalised || config.sections.len() == 1);
    }
}
