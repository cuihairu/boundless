//! The Watcher path tree and its value types (BW-12-01-01).
//!
//! Ported from `programming/bigworld/lib/cstdmf/watcher.{hpp,cpp}`: the
//! `DirectoryWatcher` tree (watcher.cpp:297-536), the `WatcherDataType`
//! enumeration (watcher.hpp:40-49), the `Watcher::Mode` access modes
//! (watcher.hpp:719-727), and the scalar value-to-string rendering
//! (`watcherValueToString`, watcher.hpp:680-710). The observable behaviour
//! is pinned to the original suite `lib/cstdmf/unit_test/test_watcher.cpp`.
//!
//! Path semantics follow the reference exactly:
//!
//! * a segment is either a directory or a leaf, never both; adding under a
//!   leaf is refused (the base `Watcher::addChild` returns false,
//!   watcher.hpp:900-902) and so is replacing an existing child
//!   ("tried to replace existing watcher", watcher.cpp:331-335);
//! * `addChild` on a multi-segment path creates the intermediate directory
//!   *before* recursing, so a refused tail can leave an empty directory
//!   behind (watcher.cpp:346-376) — that quirk is kept;
//! * lookup skips nothing: an empty first segment (leading or trailing
//!   slash) resolves to nothing (`getChild`, watcher.cpp:429-474);
//! * children are kept in sorted label order, as `SORTED_WATCHERS` builds
//!   do (watcher.cpp:323-329, asserted by test_watcher.cpp:222-238).

use std::collections::BTreeMap;

/// The path separator, `WATCHER_SEPARATOR` (watcher.hpp).
pub const WATCHER_SEPARATOR: char = '/';

/// What a directory watcher renders as (test_watcher.cpp:150-152).
pub const DIRECTORY_STRING: &str = "<DIR>";

/// The wire/data type of a watched value, `WatcherDataType`
/// (watcher.hpp:40-49). The discriminants are protocol values: STRING is 5.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum WatcherDataType {
    /// `WATCHER_TYPE_UNKNOWN`
    Unknown = 0,
    /// `WATCHER_TYPE_INT` — `int` and `int64` values alike
    /// (watcher.hpp:988-995, 1022-1029).
    Int = 1,
    /// `WATCHER_TYPE_UINT` — `unsigned int` values.
    Uint = 2,
    /// `WATCHER_TYPE_FLOAT` — `float` and `double` values
    /// (watcher.hpp:1001-1015).
    Float = 3,
    /// `WATCHER_TYPE_BOOL`
    Bool = 4,
    /// `WATCHER_TYPE_STRING` — also the generic fallback for any other C++
    /// type (watcher.hpp:962-968).
    String = 5,
    /// `WATCHER_TYPE_TUPLE`
    Tuple = 6,
    /// `WATCHER_TYPE_TYPE` — a `WatcherDataType` itself is a watchable value
    /// (watcher.hpp:970-978).
    Type = 7,
}

impl WatcherDataType {
    /// The type for a protocol discriminant byte, if it is one of the eight
    /// defined values.
    pub const fn from_discriminant(discriminant: u8) -> Option<Self> {
        Some(match discriminant {
            0 => Self::Unknown,
            1 => Self::Int,
            2 => Self::Uint,
            3 => Self::Float,
            4 => Self::Bool,
            5 => Self::String,
            6 => Self::Tuple,
            7 => Self::Type,
            _ => return None,
        })
    }

    /// The protocol name, `WATCHER_TYPE_<name>` without the prefix.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Int => "int",
            Self::Uint => "uint",
            Self::Float => "float",
            Self::Bool => "bool",
            Self::String => "string",
            Self::Tuple => "tuple",
            Self::Type => "type",
        }
    }
}

/// A watcher's access mode, `Watcher::Mode` (watcher.hpp:719-727).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Mode {
    /// `WT_INVALID` — indicates an error.
    Invalid,
    /// `WT_READ_ONLY` — the watched value cannot be changed.
    ReadOnly,
    /// `WT_READ_WRITE` — the watched value can be changed.
    ReadWrite,
    /// `WT_DIRECTORY` — the watcher has children.
    Directory,
    /// `WT_CALLABLE` — the watcher is a callable function.
    Callable,
}

/// A scalar watched value. Each variant carries the `WatcherDataType` the
/// reference's `watcherValueToStream` overloads report for the C++ type
/// (watcher.hpp:962-1030): `int`/`int64` are INT, `unsigned int` is UINT,
/// `float`/`double` are FLOAT, `bool` is BOOL, everything else streams as
/// STRING, and a `WatcherDataType` value itself is TYPE.
#[derive(Debug, Clone, PartialEq)]
pub enum WatcherValue {
    /// An `int` (or `int64`) value.
    Int(i64),
    /// An `unsigned int` value.
    Uint(u64),
    /// A `float` or `double` value.
    Float(f64),
    /// A `bool` value.
    Bool(bool),
    /// A string value.
    String(String),
    /// A `WatcherDataType` value.
    Type(WatcherDataType),
}

impl WatcherValue {
    /// The data type this value reports, the way the streaming overloads do.
    pub const fn data_type(&self) -> WatcherDataType {
        match self {
            Self::Int(_) => WatcherDataType::Int,
            Self::Uint(_) => WatcherDataType::Uint,
            Self::Float(_) => WatcherDataType::Float,
            Self::Bool(_) => WatcherDataType::Bool,
            Self::String(_) => WatcherDataType::String,
            Self::Type(_) => WatcherDataType::Type,
        }
    }

    /// The `watcherValueToString` rendering (watcher.hpp:680-710): generic
    /// values go through `ostream <<` (decimal integers, default-precision
    /// floats), booleans render as `true`/`false`, strings verbatim.
    pub fn as_string(&self) -> String {
        match self {
            Self::Int(v) => v.to_string(),
            Self::Uint(v) => v.to_string(),
            Self::Float(v) => cpp_float_string(*v),
            Self::Bool(v) => {
                if *v {
                    "true".to_string()
                } else {
                    "false".to_string()
                }
            }
            Self::String(v) => v.clone(),
            // The enum has no operator<<, so the generic template prints it
            // as its integer value.
            Self::Type(v) => (*v as u8).to_string(),
        }
    }
}

/// `ostream <<` default float rendering: precision 6 with `%g` semantics —
/// fixed notation while the exponent sits in -4..5, scientific with a
/// two-digit signed exponent outside it, trailing zeros trimmed.
fn cpp_float_string(value: f64) -> String {
    if value == 0.0 {
        return if value.is_sign_negative() {
            "-0".to_string()
        } else {
            "0".to_string()
        };
    }

    // Six significant digits in scientific form tell us the decimal
    // exponent without touching log10.
    let scientific = format!("{value:.5e}");
    let (mantissa, exponent_text) = scientific
        .split_once('e')
        .expect("LowerExp has an exponent");
    let exponent: i32 = exponent_text.parse().expect("decimal exponent");
    let digits: String = mantissa.chars().filter(|c| c.is_ascii_digit()).collect();
    let negative = mantissa.starts_with('-');

    if !(-4..6).contains(&exponent) {
        let mut out = String::new();
        if negative {
            out.push('-');
        }
        out.push(digits.as_bytes()[0] as char);
        let fraction = digits[1..].trim_end_matches('0');
        if !fraction.is_empty() {
            out.push('.');
            out.push_str(fraction);
        }
        out.push('e');
        out.push(if exponent < 0 { '-' } else { '+' });
        let magnitude = exponent.unsigned_abs();
        if magnitude < 10 {
            out.push('0');
        }
        out.push_str(&magnitude.to_string());
        out
    } else {
        let decimals = (5 - exponent).max(0) as usize;
        let mut fixed = format!("{value:.decimals$}");
        if decimals > 0 {
            while fixed.ends_with('0') {
                fixed.pop();
            }
            if fixed.ends_with('.') {
                fixed.pop();
            }
        }
        fixed
    }
}

/// One node of the watcher path tree: a directory of labelled children, or
/// a typed leaf value. A segment is one or the other, never both.
#[derive(Debug, Clone, PartialEq)]
pub enum WatcherNode {
    /// A `DirectoryWatcher` — mode `WT_DIRECTORY`, no value of its own.
    Directory {
        /// The labelled children, in sorted label order.
        children: BTreeMap<String, WatcherNode>,
    },
    /// A `DataWatcher` leaf holding a typed value.
    Value {
        /// The leaf's data type.
        data_type: WatcherDataType,
        /// The leaf's access mode.
        mode: Mode,
        /// The watched value.
        value: WatcherValue,
    },
}

impl WatcherNode {
    /// A new empty directory watcher.
    pub fn directory() -> Self {
        Self::Directory {
            children: BTreeMap::new(),
        }
    }

    /// A new value watcher. As the `DataWatcher` constructor does
    /// (watcher.hpp:2574-2577), any mode other than read-only or
    /// read-write is coerced to read-only.
    pub fn value(value: WatcherValue, mode: Mode) -> Self {
        let mode = if matches!(mode, Mode::ReadOnly | Mode::ReadWrite) {
            mode
        } else {
            Mode::ReadOnly
        };
        Self::Value {
            data_type: value.data_type(),
            mode,
            value,
        }
    }

    /// This node's mode: `WT_DIRECTORY` for directories, the leaf's access
    /// mode for values.
    pub const fn mode(&self) -> Mode {
        match self {
            Self::Directory { .. } => Mode::Directory,
            Self::Value { mode, .. } => *mode,
        }
    }

    /// This node's data type: directories have none (`WATCHER_TYPE_UNKNOWN`),
    /// leaves have their value's type.
    pub const fn data_type(&self) -> WatcherDataType {
        match self {
            Self::Directory { .. } => WatcherDataType::Unknown,
            Self::Value { data_type, .. } => *data_type,
        }
    }

    /// The watched value, if this node is a leaf.
    pub const fn watched_value(&self) -> Option<&WatcherValue> {
        match self {
            Self::Directory { .. } => None,
            Self::Value { value, .. } => Some(value),
        }
    }

    /// The `getAsString` rendering: directories print `<DIR>`
    /// (test_watcher.cpp:150-152), leaves print their value.
    pub fn as_string(&self) -> String {
        match self {
            Self::Directory { .. } => DIRECTORY_STRING.to_string(),
            Self::Value { value, .. } => value.as_string(),
        }
    }

    /// The immediate child with this exact label. Leaves have no children
    /// (base `Watcher::getChild` returns NULL, watcher.hpp:909-911).
    pub fn child(&self, label: &str) -> Option<&Self> {
        match self {
            Self::Directory { children } => children.get(label),
            Self::Value { .. } => None,
        }
    }

    /// The immediate children, in sorted label order.
    pub fn children(&self) -> Vec<(&str, &Self)> {
        match self {
            Self::Directory { children } => children
                .iter()
                .map(|(label, node)| (label.as_str(), node))
                .collect(),
            Self::Value { .. } => Vec::new(),
        }
    }

    /// `DirectoryWatcher::addChild` (watcher.cpp:297-385): attach `node` at
    /// `path` relative to this directory. Refused paths return false: an
    /// empty path, a duplicate leaf, and any attempt to hang children under
    /// a leaf.
    pub fn add(&mut self, path: &str, node: Self) -> bool {
        let children = match self {
            Self::Directory { children } => children,
            // The base Watcher::addChild cannot take children.
            Self::Value { .. } => return false,
        };

        if path.is_empty() {
            // "tried to add unnamed child (no trailing slashes please)"
            return false;
        }

        match path.find(WATCHER_SEPARATOR) {
            None => {
                if children.contains_key(path) {
                    // "tried to replace existing watcher"
                    return false;
                }
                children.insert(path.to_string(), node);
                true
            }
            Some(separator) => {
                let label = &path[..separator];
                let tail = tail(path).expect("a separator is present");
                if let Some(child) = children.get_mut(label) {
                    match child {
                        Self::Directory { .. } => child.add(tail, node),
                        // A leaf cannot take children.
                        Self::Value { .. } => false,
                    }
                } else {
                    // The intermediate directory is created and attached
                    // before recursing, exactly as the reference does — a
                    // refused tail leaves the empty directory behind.
                    let mut directory = Self::directory();
                    let added = directory.add(tail, node);
                    children.insert(label.to_string(), directory);
                    added
                }
            }
        }
    }

    /// `DirectoryWatcher::getChild` (watcher.cpp:429-474): resolve `path`
    /// relative to this directory. An empty first segment resolves to
    /// nothing, and so does any path that runs through a leaf.
    pub fn get(&self, path: &str) -> Option<&Self> {
        let children = match self {
            Self::Directory { children } => children,
            // The base Watcher::getChild finds nothing.
            Self::Value { .. } => return None,
        };

        // The reference only looks a child up when the first segment is
        // non-empty: `compareLength != 0` (watcher.cpp:443). That is what
        // makes a leading separator unresolvable, even though `addChild`
        // happily creates the empty-labelled directory.
        let separator = path.find(WATCHER_SEPARATOR);
        let compare_length = separator.unwrap_or(path.len());
        if compare_length == 0 {
            return None;
        }

        match separator {
            None => children.get(path),
            Some(separator) => {
                let label = &path[..separator];
                let tail = tail(path).expect("a separator is present");
                match children.get(label) {
                    Some(Self::Directory { .. }) => children.get(label).expect("found").get(tail),
                    _ => None,
                }
            }
        }
    }

    /// `DirectoryWatcher::removeChild` (watcher.cpp:388-425): remove the
    /// node at `path`. Removing through a leaf, or a path that is not
    /// present, returns false. Intermediate directories are left behind,
    /// as in the reference.
    pub fn remove(&mut self, path: &str) -> bool {
        let children = match self {
            Self::Directory { children } => children,
            // The base Watcher::removeChild cannot remove.
            Self::Value { .. } => return false,
        };

        // Like getChild, an empty first segment removes nothing
        // (watcher.cpp:393-403).
        let separator = path.find(WATCHER_SEPARATOR);
        let compare_length = separator.unwrap_or(path.len());
        if compare_length == 0 {
            return false;
        }

        match separator {
            None => children.remove(path).is_some(),
            Some(separator) => {
                let label = &path[..separator];
                let tail = tail(path).expect("a separator is present");
                match children.get_mut(label) {
                    Some(Self::Directory { .. }) => {
                        children.get_mut(label).expect("just found").remove(tail)
                    }
                    _ => false,
                }
            }
        }
    }
}

/// `Watcher::partitionPath` (watcher.cpp:574-596): split a full path at its
/// last separator into `(name, directory)`, where `directory` keeps the
/// trailing separator. A path without a separator is all name.
pub fn partition_path(path: &str) -> (String, String) {
    match path.rfind(WATCHER_SEPARATOR) {
        Some(position) => (
            path[position + 1..].to_string(),
            path[..=position].to_string(),
        ),
        None => (path.to_string(), String::new()),
    }
}

/// `DirectoryWatcher::tail` (watcher.cpp:522-536): the path after the first
/// separator, or `None` when the path has none.
pub fn tail(path: &str) -> Option<&str> {
    path.find(WATCHER_SEPARATOR)
        .map(|separator| &path[separator + 1..])
}

/// The watcher path tree rooted at a `DirectoryWatcher` — the port of
/// `Watcher::rootWatcher()`'s tree (watcher.hpp:3013).
#[derive(Debug, Clone, PartialEq)]
pub struct WatcherTree {
    root: WatcherNode,
}

impl Default for WatcherTree {
    fn default() -> Self {
        Self::new()
    }
}

impl WatcherTree {
    /// A new tree with an empty root directory.
    pub fn new() -> Self {
        Self {
            root: WatcherNode::directory(),
        }
    }

    /// Attach a value leaf at `path`, coercing the mode the way the
    /// `DataWatcher` constructor does. Returns false when the path is
    /// refused.
    pub fn add(&mut self, path: &str, value: WatcherValue, mode: Mode) -> bool {
        self.root.add(path, WatcherNode::value(value, mode))
    }

    /// Resolve `path` to its node; `None` when there is no node there.
    pub fn get(&self, path: &str) -> Option<&WatcherNode> {
        self.root.get(path)
    }

    /// Remove the node at `path`. Returns false when there is nothing to
    /// remove there.
    pub fn remove(&mut self, path: &str) -> bool {
        self.root.remove(path)
    }

    /// The root directory's children, in sorted label order.
    pub fn children(&self) -> Vec<(&str, &WatcherNode)> {
        self.root.children()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// test_watcher.cpp:18-35 — Watcher_partitionPath.
    #[test]
    fn partition_path_matches_the_reference_suite() {
        let (name, directory) = partition_path("root/child/grandchild");
        assert_eq!(name, "grandchild");
        assert_eq!(directory, "root/child/");

        let (name, directory) = partition_path("child");
        assert_eq!(name, "child");
        assert_eq!(directory, "");

        let (name, directory) = partition_path("a/b");
        assert_eq!(name, "b");
        assert_eq!(directory, "a/");
    }

    /// test_watcher.cpp:37-43 — Watcher_tail.
    #[test]
    fn tail_matches_the_reference_suite() {
        assert_eq!(tail("a/b/c"), Some("b/c"));
        assert_eq!(tail("ab/c"), Some("c"));
        assert_eq!(tail("no_separator"), None);
        assert_eq!(tail("a/"), Some(""));
    }

    /// test_watcher.cpp:78-96 — DirectoryWatcher_addAndGetChild.
    #[test]
    fn add_and_get_child_matches_the_reference_suite() {
        let mut tree = WatcherTree::new();
        assert!(tree.add("value", WatcherValue::Int(7), Mode::ReadOnly));

        let node = tree.get("value").expect("the added value");
        assert_eq!(node.watched_value(), Some(&WatcherValue::Int(7)));
        assert_eq!(tree.get("nonExistent"), None);

        // Adding the same label twice is refused.
        assert!(!tree.add("value", WatcherValue::Int(8), Mode::ReadOnly));
        assert_eq!(
            tree.get("value").unwrap().watched_value(),
            Some(&WatcherValue::Int(7))
        );

        // The unnamed child is refused.
        assert!(!tree.add("", WatcherValue::Int(9), Mode::ReadOnly));
    }

    /// test_watcher.cpp:98-113 — DirectoryWatcher_addNestedPath.
    #[test]
    fn add_nested_path_matches_the_reference_suite() {
        let mut tree = WatcherTree::new();
        assert!(tree.add("sub/value", WatcherValue::Int(1), Mode::ReadOnly));

        let sub = tree.get("sub").expect("the intermediate directory");
        assert_eq!(sub.mode(), Mode::Directory);
        let leaf = tree.get("sub/value").expect("the nested value");
        assert_eq!(leaf.watched_value(), Some(&WatcherValue::Int(1)));

        // A leaf cannot take children.
        assert!(!tree.add("sub/value/deeper", WatcherValue::Int(2), Mode::ReadOnly));
        assert_eq!(
            tree.get("sub/value").unwrap().watched_value(),
            Some(&WatcherValue::Int(1))
        );
    }

    /// test_watcher.cpp:116-140 — DirectoryWatcher_removeChild.
    #[test]
    fn remove_child_matches_the_reference_suite() {
        let mut tree = WatcherTree::new();
        assert!(tree.add("value", WatcherValue::Int(1), Mode::ReadOnly));

        assert!(tree.remove("value"));
        assert_eq!(tree.get("value"), None);

        // Removing again finds nothing.
        assert!(!tree.remove("value"));

        assert!(tree.add("sub/value", WatcherValue::Int(2), Mode::ReadOnly));
        // Removing the leaf leaves the intermediate directory behind.
        assert!(tree.remove("sub/value"));
        assert!(tree.get("sub").is_some());
        assert_eq!(tree.get("sub/value"), None);

        // Removing through a leaf is refused.
        assert!(!tree.remove("sub/value/deeper"));

        // The empty path (the reference's removeChild(NULL)) removes nothing.
        assert!(!tree.remove(""));
        assert!(!tree.remove("/"));
    }

    /// test_watcher.cpp:142-166 — DirectoryWatcher_getAsString: a directory
    /// prints `<DIR>` with WT_DIRECTORY, and a leaf prints its value with
    /// its own mode.
    #[test]
    fn get_as_string_matches_the_reference_suite() {
        let mut tree = WatcherTree::new();

        let root = tree.get("");
        // The empty path resolves to nothing at the root directory; the
        // directory itself is what the tree holds.
        assert_eq!(root, None);

        assert!(tree.add("value", WatcherValue::Int(7), Mode::ReadOnly));
        let leaf = tree.get("value").expect("the value");
        assert_eq!(leaf.as_string(), "7");
        assert_eq!(leaf.mode(), Mode::ReadOnly);

        // Unknown children fail, and so does a path through a leaf.
        assert_eq!(tree.get("nonExistent"), None);
        assert_eq!(tree.get("value/sub"), None);

        // A directory node renders as <DIR>.
        tree.add("sub/leaf", WatcherValue::Int(3), Mode::ReadOnly);
        let sub = tree.get("sub").expect("the directory");
        assert_eq!(sub.as_string(), DIRECTORY_STRING);
        assert_eq!(sub.mode(), Mode::Directory);
    }

    /// test_watcher.cpp:222-238 — DirectoryWatcher_visitChildren keeps the
    /// children in sorted label order.
    #[test]
    fn children_are_kept_in_sorted_label_order() {
        let mut tree = WatcherTree::new();
        assert!(tree.add("second", WatcherValue::Int(2), Mode::ReadOnly));
        assert!(tree.add("third", WatcherValue::Int(3), Mode::ReadOnly));
        assert!(tree.add("first", WatcherValue::Int(1), Mode::ReadOnly));

        let labels: Vec<&str> = tree.children().iter().map(|(label, _)| *label).collect();
        assert_eq!(labels, ["first", "second", "third"]);
        let values: Vec<&WatcherValue> = tree
            .children()
            .iter()
            .filter_map(|(_, node)| node.watched_value())
            .collect();
        assert_eq!(
            values,
            [
                &WatcherValue::Int(1),
                &WatcherValue::Int(2),
                &WatcherValue::Int(3)
            ]
        );
    }

    /// The acceptance for BW-12-01-01: for the same path, the value and the
    /// type match. Each scalar's data type follows the
    /// `watcherValueToStream` overloads (watcher.hpp:962-1030). The 3.14
    /// datum is the decimal the reference renders, not an approximation of pi.
    #[test]
    #[allow(clippy::approx_constant)]
    fn the_same_path_reports_the_same_value_and_type() {
        let mut tree = WatcherTree::new();
        assert!(tree.add("load", WatcherValue::Int(42), Mode::ReadOnly));
        assert!(tree.add("tick", WatcherValue::Uint(7), Mode::ReadOnly));
        assert!(tree.add("fps", WatcherValue::Float(3.14), Mode::ReadOnly));
        assert!(tree.add("online", WatcherValue::Bool(true), Mode::ReadOnly));
        assert!(tree.add(
            "name",
            WatcherValue::String("cellapp1".into()),
            Mode::ReadOnly
        ));
        assert!(tree.add(
            "queryType",
            WatcherValue::Type(WatcherDataType::Bool),
            Mode::ReadOnly
        ));

        let load = tree.get("load").expect("load");
        assert_eq!(load.data_type(), WatcherDataType::Int);
        assert_eq!(load.watched_value(), Some(&WatcherValue::Int(42)));

        let tick = tree.get("tick").expect("tick");
        assert_eq!(tick.data_type(), WatcherDataType::Uint);
        assert_eq!(tick.watched_value(), Some(&WatcherValue::Uint(7)));

        let fps = tree.get("fps").expect("fps");
        assert_eq!(fps.data_type(), WatcherDataType::Float);
        assert_eq!(fps.watched_value(), Some(&WatcherValue::Float(3.14)));

        let online = tree.get("online").expect("online");
        assert_eq!(online.data_type(), WatcherDataType::Bool);
        assert_eq!(online.watched_value(), Some(&WatcherValue::Bool(true)));

        let name = tree.get("name").expect("name");
        assert_eq!(name.data_type(), WatcherDataType::String);
        assert_eq!(
            name.watched_value(),
            Some(&WatcherValue::String("cellapp1".into()))
        );

        let query_type = tree.get("queryType").expect("queryType");
        assert_eq!(query_type.data_type(), WatcherDataType::Type);
        assert_eq!(
            query_type.watched_value(),
            Some(&WatcherValue::Type(WatcherDataType::Bool))
        );

        // A deep path resolves with its value and type intact.
        assert!(tree.add("cells/count", WatcherValue::Int(12), Mode::ReadOnly));
        let count = tree.get("cells/count").expect("cells/count");
        assert_eq!(count.data_type(), WatcherDataType::Int);
        assert_eq!(count.watched_value(), Some(&WatcherValue::Int(12)));
        assert_eq!(
            tree.get("cells").unwrap().data_type(),
            WatcherDataType::Unknown
        );
    }

    /// The eight protocol values round-trip through their discriminants, and
    /// STRING keeps its wire value 5.
    #[test]
    fn data_types_round_trip_through_their_discriminants() {
        let all = [
            (WatcherDataType::Unknown, 0u8),
            (WatcherDataType::Int, 1),
            (WatcherDataType::Uint, 2),
            (WatcherDataType::Float, 3),
            (WatcherDataType::Bool, 4),
            (WatcherDataType::String, 5),
            (WatcherDataType::Tuple, 6),
            (WatcherDataType::Type, 7),
        ];
        for (data_type, discriminant) in all {
            assert_eq!(data_type as u8, discriminant);
            assert_eq!(
                WatcherDataType::from_discriminant(discriminant),
                Some(data_type)
            );
        }
        assert_eq!(WatcherDataType::from_discriminant(8), None);
    }

    /// The `DataWatcher` constructor coerces any mode other than
    /// read-only/read-write to read-only (watcher.hpp:2574-2577).
    #[test]
    fn value_modes_coerce_like_the_data_watcher_constructor() {
        assert_eq!(
            WatcherNode::value(WatcherValue::Int(1), Mode::ReadWrite).mode(),
            Mode::ReadWrite
        );
        assert_eq!(
            WatcherNode::value(WatcherValue::Int(1), Mode::ReadOnly).mode(),
            Mode::ReadOnly
        );
        assert_eq!(
            WatcherNode::value(WatcherValue::Int(1), Mode::Invalid).mode(),
            Mode::ReadOnly
        );
        assert_eq!(
            WatcherNode::value(WatcherValue::Int(1), Mode::Directory).mode(),
            Mode::ReadOnly
        );
        assert_eq!(
            WatcherNode::value(WatcherValue::Int(1), Mode::Callable).mode(),
            Mode::ReadOnly
        );
    }

    /// `watcherValueToString` (watcher.hpp:680-710): generic values stream
    /// with ostream defaults, booleans render as true/false, strings
    /// verbatim.
    #[test]
    fn value_strings_match_the_reference_rendering() {
        assert_eq!(WatcherValue::Int(5).as_string(), "5");
        assert_eq!(WatcherValue::Int(-12).as_string(), "-12");
        assert_eq!(WatcherValue::Uint(4294967295).as_string(), "4294967295");
        assert_eq!(WatcherValue::Bool(true).as_string(), "true");
        assert_eq!(WatcherValue::Bool(false).as_string(), "false");
        assert_eq!(
            WatcherValue::String("cellapp".into()).as_string(),
            "cellapp"
        );
        assert_eq!(WatcherValue::Type(WatcherDataType::String).as_string(), "5");
        assert_eq!(WatcherNode::directory().as_string(), "<DIR>");
    }

    /// Default `ostream <<` float rendering: precision 6, %g notation rules.
    /// The decimal datums are what the reference prints, not pi.
    #[test]
    #[allow(clippy::approx_constant)]
    fn float_strings_match_ostream_defaults() {
        assert_eq!(cpp_float_string(0.0), "0");
        assert_eq!(cpp_float_string(-0.0), "-0");
        assert_eq!(cpp_float_string(1.0), "1");
        assert_eq!(cpp_float_string(2.0), "2");
        assert_eq!(cpp_float_string(3.14), "3.14");
        // 0.1 + 0.2 prints with six significant digits, like the reference.
        assert_eq!(cpp_float_string(0.1 + 0.2), "0.3");
        assert_eq!(cpp_float_string(-3.14), "-3.14");
        assert_eq!(cpp_float_string(0.0001), "0.0001");
        // Below 1e-4 the notation flips to scientific.
        assert_eq!(cpp_float_string(0.00001), "1e-05");
        assert_eq!(cpp_float_string(100000.0), "100000");
        // At 1e6 and above the notation flips to scientific.
        assert_eq!(cpp_float_string(1000000.0), "1e+06");
        assert_eq!(cpp_float_string(123456.789), "123457");
        assert_eq!(cpp_float_string(1234567.8), "1.23457e+06");
        assert_eq!(cpp_float_string(2.5e-5), "2.5e-05");
    }

    /// The addChild quirk (watcher.cpp:346-376): a multi-segment path
    /// creates and keeps its intermediate directory even when the tail is
    /// refused, and a leading separator makes an empty-labelled directory
    /// that lookup cannot reach.
    #[test]
    fn refused_tails_leave_the_intermediate_directory_behind() {
        let mut tree = WatcherTree::new();
        // The empty tail is refused, but the directory "sub" is kept.
        assert!(!tree.add("sub/", WatcherValue::Int(1), Mode::ReadOnly));
        assert_eq!(
            tree.get("sub").map(|node| node.mode()),
            Some(Mode::Directory)
        );

        // A leading separator creates an empty-labelled directory; the
        // empty first segment resolves to nothing.
        assert!(tree.add("/leading/leaf", WatcherValue::Int(2), Mode::ReadOnly));
        assert_eq!(tree.get("/leading/leaf"), None);
        assert_eq!(tree.get("leading/leaf"), None);
        let (_, empty) = tree
            .children()
            .into_iter()
            .find(|(label, _)| label.is_empty())
            .expect("the empty-labelled directory");
        assert_eq!(empty.mode(), Mode::Directory);

        // A trailing separator resolves to nothing as well.
        tree.add("plain", WatcherValue::Int(3), Mode::ReadOnly);
        assert_eq!(tree.get("plain/"), None);
    }

    /// Every protocol value carries its `WATCHER_TYPE_<name>` label.
    #[test]
    fn data_type_labels_match_the_protocol_names() {
        assert_eq!(WatcherDataType::Unknown.as_str(), "unknown");
        assert_eq!(WatcherDataType::Int.as_str(), "int");
        assert_eq!(WatcherDataType::Uint.as_str(), "uint");
        assert_eq!(WatcherDataType::Float.as_str(), "float");
        assert_eq!(WatcherDataType::Bool.as_str(), "bool");
        assert_eq!(WatcherDataType::String.as_str(), "string");
        assert_eq!(WatcherDataType::Tuple.as_str(), "tuple");
        assert_eq!(WatcherDataType::Type.as_str(), "type");
    }

    /// The `WatcherValue::as_string` renders a float the way the reference
    /// does.
    #[test]
    fn value_strings_render_floats() {
        assert_eq!(WatcherValue::Float(2.5).as_string(), "2.5");
        assert_eq!(WatcherValue::Float(-0.25).as_string(), "-0.25");
    }

    /// A negative value in scientific notation keeps its sign and a
    /// two-digit exponent, and an exponent of 10 or more is not zero-padded.
    #[test]
    fn float_strings_keep_signs_and_pad_exponents_to_two_digits() {
        assert_eq!(cpp_float_string(-2.5e-5), "-2.5e-05");
        assert_eq!(cpp_float_string(1e10), "1e+10");
        assert_eq!(cpp_float_string(-1e-10), "-1e-10");
    }

    /// The base `Watcher` methods (watcher.hpp:900-911) do nothing on a
    /// value node: no children, no additions, nothing to remove.
    #[test]
    fn value_nodes_behave_like_the_base_watcher() {
        let mut leaf = WatcherNode::value(WatcherValue::Int(1), Mode::ReadOnly);

        assert_eq!(leaf.watched_value(), Some(&WatcherValue::Int(1)));
        assert_eq!(leaf.child("sub"), None);
        assert!(leaf.children().is_empty());
        assert!(!leaf.add("sub", WatcherNode::directory()));
        assert_eq!(leaf.get("sub"), None);
        assert!(!leaf.remove("sub"));

        // A directory has no value of its own.
        let dir = WatcherNode::directory();
        assert_eq!(dir.watched_value(), None);
        assert_eq!(dir.child("sub"), None);
        assert!(dir.children().is_empty());
    }

    /// `WatcherTree::default()` is an empty tree, like `new()`.
    #[test]
    fn default_tree_is_empty() {
        assert_eq!(WatcherTree::default(), WatcherTree::new());
        assert_eq!(WatcherTree::default().children().len(), 0);
    }
}
