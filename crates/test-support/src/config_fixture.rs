//! Configuration and database fixtures: start a test from a known state
//! (BW-18-01-03).
//!
//! The reference `BWConfig` chain resolves `server/bw_<uid>.xml` and walks
//! `<parentFile>` entries, and entity data lives under database directories
//! the role binaries expect to find. Tests need both, but they should not
//! have to hand-write XML or rebuild the resource-path plumbing each time:
//! a [`ConfigFixture`] writes the `server/` tree and loads it through the
//! real [`BwConfig`], and a [`DatabaseFixture`] lays out fresh `db/` and
//! `journal/` directories.

use std::path::PathBuf;

use resources::bwresource::BwResource;
use server_runtime::config::BwConfig;

use crate::workspace::TempWorkspace;

/// A `server/bw.xml` tree on disk, loaded through the real config chain.
pub struct ConfigFixture {
    workspace: TempWorkspace,
}

impl ConfigFixture {
    /// Create an empty configuration root.
    pub fn new() -> Self {
        Self::with_label("config")
    }

    /// Create a uniquely named configuration root, so a kept failure
    /// directory says which test owned it.
    pub fn with_label(label: &str) -> Self {
        Self {
            workspace: TempWorkspace::with_label(label),
        }
    }

    /// Absolute path of the resource root.
    pub fn root(&self) -> PathBuf {
        self.workspace.root().to_path_buf()
    }

    /// Write `text` at `relative`, creating parent directories.
    pub fn add(&self, relative: &str, text: &str) -> PathBuf {
        self.workspace.write_text(relative, text)
    }

    /// Load the chain the way a role binary does: the personalised file
    /// comes from the passwd entry of the uid and falls back to
    /// `server/bw.xml`.
    pub fn load(&self) -> BwConfig {
        let mut config = BwConfig::new();
        config.init(&self.resource());
        config
    }

    /// Load the chain with an explicit user name, or `None` to start at
    /// `server/bw.xml` directly.
    pub fn load_with(&self, username: Option<&str>) -> BwConfig {
        let mut config = BwConfig::new();
        config.init_from_sources(&self.resource(), username);
        config
    }

    /// Load `server/bw.xml` as the only section of a fresh chain.
    pub fn load_base(&self) -> BwConfig {
        self.load_with(None)
    }

    /// The `BwResource` over the fixture root, for `is_bad` checks and
    /// further resource lookups.
    pub fn resource(&self) -> BwResource {
        let root = self.workspace.root().to_string_lossy().into_owned();
        BwResource::init_paths(&[&root])
    }
}

impl Default for ConfigFixture {
    fn default() -> Self {
        Self::new()
    }
}

/// Fresh database directories: `db/` and `journal/` laid out under an
/// isolated root, so an entity store starts from a known empty state. The
/// real storage engine arrives with BW-09; until then this fixture fixes
/// the layout and paths the config points at.
pub struct DatabaseFixture {
    workspace: TempWorkspace,
}

impl DatabaseFixture {
    /// Create a fresh database root with `db/` and `journal/` in place.
    pub fn new() -> Self {
        Self::with_label("database")
    }

    /// Create a uniquely named database root.
    pub fn with_label(label: &str) -> Self {
        let workspace = TempWorkspace::with_label(label);
        std::fs::create_dir_all(workspace.root().join("db")).expect("db directory");
        std::fs::create_dir_all(workspace.root().join("journal")).expect("journal directory");
        Self { workspace }
    }

    /// Absolute path of the database root.
    pub fn root(&self) -> PathBuf {
        self.workspace.root().to_path_buf()
    }

    /// The entity database directory, empty when the fixture is new.
    pub fn database_dir(&self) -> PathBuf {
        self.workspace.root().join("db")
    }

    /// The journal directory, empty when the fixture is new.
    pub fn journal_dir(&self) -> PathBuf {
        self.workspace.root().join("journal")
    }

    /// Drop a database file at `relative` inside the root.
    pub fn add(&self, relative: &str, bytes: &[u8]) -> PathBuf {
        self.workspace.write_file(relative, bytes)
    }

    /// True when `relative` exists inside the root.
    pub fn exists(&self, relative: &str) -> bool {
        self.workspace.exists(relative)
    }

    /// Read a database file back.
    pub fn read(&self, relative: &str) -> Vec<u8> {
        self.workspace.read(relative)
    }

    /// Keep the root on drop so a failure can be inspected.
    pub fn keep(&mut self) {
        self.workspace.keep();
    }
}

impl Default for DatabaseFixture {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `server/bw.xml` (parent) + `server/bw_user.xml` (child naming the
    /// parent): the chain under test for every resolution rule.
    fn parent_child(label: &str) -> ConfigFixture {
        let fixture = ConfigFixture::with_label(label);
        fixture.add(
            "server/bw.xml",
            "<root>\n\
             \t<app>\n\
             \t\t<port>5000</port>\n\
             \t\t<name>parent</name>\n\
             \t</app>\n\
             \t<shared><limit>10</limit></shared>\n\
             </root>\n",
        );
        fixture.add(
            "server/bw_user.xml",
            "<root>\n\
             \t<parentFile>server/bw.xml</parentFile>\n\
             \t<app>\n\
             \t\t<name>child</name>\n\
             \t</app>\n\
             </root>\n",
        );
        fixture
    }

    #[test]
    fn a_base_config_starts_from_a_known_state() {
        let fixture = parent_child("base");
        let config = fixture.load_base();

        assert_eq!(config.section_names(), ["server/bw.xml"]);
        assert_eq!(config.get_int("app/port", 0), 5000);
        assert_eq!(config.get_string("app/name", ""), "parent");
        assert_eq!(config.get_int("shared/limit", 0), 10);
        assert!(!config.is_bad(&fixture.resource()));
    }

    #[test]
    fn the_chain_resolves_child_first() {
        let fixture = parent_child("chain");
        let config = fixture.load_with(Some("user"));

        assert_eq!(
            config.section_names(),
            ["server/bw_user.xml", "server/bw.xml"]
        );
        // Overridden in the child, filled in from the parent.
        assert_eq!(config.get_string("app/name", ""), "child");
        assert_eq!(config.get_int("app/port", 0), 5000);
        assert!(!config.is_bad(&fixture.resource()));
    }

    #[test]
    fn a_missing_personalised_file_falls_back_to_base() {
        let fixture = ConfigFixture::with_label("fallback");
        fixture.add("server/bw.xml", "<root><app><port>5000</port></app></root>");

        let config = fixture.load_with(Some("absent-user"));
        assert_eq!(config.section_names(), ["server/bw.xml"]);
        assert_eq!(config.get_int("app/port", 0), 5000);
    }

    #[test]
    fn a_broken_chain_reports_bad() {
        let fixture = ConfigFixture::with_label("broken");
        let config = fixture.load_with(None);

        assert!(config.section_names().is_empty());
        assert!(config.is_bad(&fixture.resource()));
    }

    #[test]
    fn two_fixtures_load_independently() {
        let first = parent_child("first");
        let second = parent_child("second");
        assert_ne!(first.root(), second.root());

        second.add("server/bw.xml", "<root><app><port>6000</port></app></root>");
        assert_eq!(first.load_base().get_int("app/port", 0), 5000);
        assert_eq!(second.load_base().get_int("app/port", 0), 6000);
    }

    #[test]
    fn the_database_directories_exist_and_start_empty() {
        let fixture = DatabaseFixture::with_label("dirs");

        assert!(fixture.database_dir().is_dir());
        assert!(fixture.journal_dir().is_dir());
        assert_eq!(
            std::fs::read_dir(fixture.database_dir()).unwrap().count(),
            0
        );
    }

    #[test]
    fn a_database_backed_config_starts_from_a_known_state() {
        let config_fixture = ConfigFixture::with_label("joined");
        let database = DatabaseFixture::with_label("joined");
        database.add("db/entities.xml", b"<root/>");

        config_fixture.add(
            "server/bw.xml",
            &format!(
                "<root>\n\
                 \t<database>\n\
                 \t\t<file>{}</file>\n\
                 \t\t<journal>{}</journal>\n\
                 \t</database>\n\
                 </root>\n",
                database.database_dir().join("entities.xml").display(),
                database.journal_dir().display(),
            ),
        );

        let config = config_fixture.load_base();
        let file = config.get_string("database/file", "");
        assert!(file.ends_with("db/entities.xml"), "file: {file}");
        assert!(database.exists("db/entities.xml"));
        assert_eq!(database.read("db/entities.xml"), b"<root/>");
    }
}
