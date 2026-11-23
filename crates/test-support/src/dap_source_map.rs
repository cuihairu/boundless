//! BW-27-02-04: mapping debug symbols to the local source files.
//!
//! A target built or run elsewhere — a container, a build server — reports
//! paths that are not the editor's. The source map rewrites a remote path to
//! the local file the editor has open. The deepest matching root wins, and a
//! root only matches at a path boundary, so a shorter root never swallows a
//! deeper one or a same-prefix sibling.

/// A map from remote roots to local roots.
#[derive(Default)]
pub struct SourceMap {
    /// Remote roots with their local counterparts, deepest (longest) first.
    roots: Vec<(String, String)>,
}

impl SourceMap {
    /// An empty map.
    pub fn new() -> Self {
        Self::default()
    }

    /// Map one remote root to its local root. The new root is matched before
    /// a shorter one.
    pub fn add_root(&mut self, remote: &str, local: &str) {
        self.roots.push((remote.to_string(), local.to_string()));
        self.roots
            .sort_by_key(|(remote, _)| std::cmp::Reverse(remote.len()));
    }

    /// The local file a remote path is. A path under no root maps to nothing.
    pub fn local(&self, remote: &str) -> Option<String> {
        for (remote_root, local_root) in &self.roots {
            if let Some(rest) = remote.strip_prefix(remote_root.as_str())
                && (rest.is_empty() || rest.starts_with('/'))
            {
                return Some(format!("{local_root}{rest}"));
            }
        }
        None
    }

    /// The local file a `path:line` symbol (or a bare path) is. The line
    /// suffix is dropped when it is numeric; anything else is part of the
    /// path.
    pub fn symbol_file(&self, symbol: &str) -> Option<String> {
        let path = match symbol.rsplit_once(':') {
            Some((path, line)) if line.bytes().all(|b| b.is_ascii_digit()) => path,
            _ => symbol,
        };
        self.local(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_symbol_maps_to_its_local_file() {
        let mut map = SourceMap::new();
        map.add_root("/build/app", "/home/cui/work/app");

        // A `path:line` symbol drops the line and lands on the local file.
        assert_eq!(
            map.symbol_file("/build/app/scripts/Avatar.py:120"),
            Some("/home/cui/work/app/scripts/Avatar.py".to_string())
        );
        assert_eq!(
            map.local("/build/app/scripts/Avatar.py"),
            Some("/home/cui/work/app/scripts/Avatar.py".to_string())
        );
    }

    #[test]
    fn the_deepest_root_wins() {
        let mut map = SourceMap::new();
        map.add_root("/build/app", "/local/app");
        map.add_root("/build/app/scripts", "/local/app_scripts");

        // The deeper root maps to its own local tree, not the outer one's.
        assert_eq!(
            map.local("/build/app/scripts/space.rs"),
            Some("/local/app_scripts/space.rs".to_string())
        );
        assert_eq!(
            map.local("/build/app/res/data.json"),
            Some("/local/app/res/data.json".to_string())
        );
    }

    #[test]
    fn a_root_only_matches_at_a_path_boundary() {
        let mut map = SourceMap::new();
        map.add_root("/build/app", "/local/app");

        // "/build/apple" shares a prefix but is a sibling, not under the root.
        assert_eq!(map.local("/build/apple"), None);
        assert_eq!(map.local("/build/app"), Some("/local/app".to_string()));
    }

    #[test]
    fn an_unmapped_path_maps_to_nothing() {
        let mut map = SourceMap::new();
        map.add_root("/build/app", "/local/app");
        assert_eq!(map.local("/elsewhere/x.py"), None);
        assert_eq!(map.symbol_file("/elsewhere/x.py:5"), None);
    }

    #[test]
    fn a_line_suffix_is_stripped_only_when_numeric() {
        let mut map = SourceMap::new();
        map.add_root("/build/app", "/local/app");

        // A numeric suffix is the line; a non-numeric one is part of the path.
        assert_eq!(
            map.symbol_file("/build/app/a.py:120"),
            Some("/local/app/a.py".into())
        );
        assert_eq!(
            map.symbol_file("/build/app/a.py"),
            Some("/local/app/a.py".into())
        );
        assert_eq!(
            map.symbol_file("/build/app/a.py:notaline"),
            Some("/local/app/a.py:notaline".into())
        );
    }
}
