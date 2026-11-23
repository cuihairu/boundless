//! The process watcher: a path-addressable tree of typed debug values
//! (`lib/cstdmf/watcher.hpp` / `watcher.cpp`, BW-12-01-01).
//!
//! Every process registers scalar leaves and directory groupings under
//! slash-separated paths ("game/rounds", "flags/enabled"); debug tooling
//! later reads and writes them by path (protocol 1 strings here, the
//! protocol 2 stream form arrives with the remote-query item BW-12-01-04).
//!
//! # Port shape
//!
//! - [`Watcher`] is the reference's abstract `Watcher`
//!   (`watcher.hpp:713-956`): [`Watcher::get_as_string`] answers with the
//!   value string, description and mode; [`Watcher::set_from_string`] writes
//!   a parsed value back. The `base` pointer offset the reference threads
//!   through every call becomes ordinary closure/handle capture -- the
//!   observable string protocol is unchanged.
//! - [`DataWatcher`] is `DataWatcher<TYPE>` (`watcher.hpp:2563-2724`): one
//!   typed leaf, read via [`WatcherValue::to_cpp_string`], written via
//!   [`WatcherValue::from_cpp_string`], gated on its access mode. The
//!   reference's `MemberWatcher`/`FunctionWatcher` collapse into the same
//!   shape (they differ only in where the reference stores its value
//!   reference).
//! - [`DirectoryWatcher`] is `DirectoryWatcher` (`watcher.cpp:115-544`):
//!   children live in a vector kept sorted by label -- the reference builds
//!   with `#define SORTED_WATCHERS` (`watcher.cpp:96`), so enumeration and
//!   lookups see label order, not registration order. `addChild` splits the
//!   path on the first separator, rejects duplicates and empty segments, and
//!   creates intermediate directories on demand.
//! - [`partition_path`] is `Watcher::partitionPath` (`watcher.cpp:574-593`):
//!   split at the *last* separator, directory keeps its trailing slash.
//! - The process-wide root is `Watcher::rootWatcherInternal()`
//!   (`watcher.cpp:61-70`): a lazily created [`DirectoryWatcher`],
//!   [`fini`]ed on shutdown; [`has_root`] probes without creating.
//!
//! # Deltas
//!
//! - `NULL` and empty paths unify into `&str` (`""`): every reference
//!   code path treats them identically (`isEmptyPath` checks both).
//! - The protocol 2 stream API (`getAsStream`/`setFromStream`, the
//!   `__doc__` pseudo-path, `watcherValueToStream`) is the remote-query
//!   surface and lands with BW-12-01-04; the string protocol here is
//!   complete for the tree.
//! - `CallableWatcher` and its subtypes are BW-12-01-03; `SafeWatcher` /
//!   `FreezeWatcher` serve the fault-injection framework (BW-18).
//! - The reference logs `ERROR_MSG` on duplicate or empty `addChild`;
//!   without a log sink the port records the same conditions as `false`
//!   returns.

use std::cell::RefCell;
use std::rc::Rc;

/// `WATCHER_SEPARATOR` (`watcher.hpp:67`): the path component separator.
pub const SEPARATOR: char = '/';

/// `Watcher::Mode` (`watcher.hpp:720-726`): what a watcher exposes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// `WT_INVALID`: an error answer.
    Invalid,
    /// `WT_READ_ONLY`: the value cannot be changed.
    ReadOnly,
    /// `WT_READ_WRITE`: the value can be changed.
    ReadWrite,
    /// `WT_DIRECTORY`: the watcher has children.
    Directory,
    /// `WT_CALLABLE`: the watcher is a function.
    Callable,
}

/// `WatcherDataType` (`watcher.hpp:40-52`): the wire value types.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DataType {
    /// `WATCHER_TYPE_UNKNOWN`.
    Unknown,
    /// `WATCHER_TYPE_INT`.
    Int,
    /// `WATCHER_TYPE_UINT`.
    Uint,
    /// `WATCHER_TYPE_FLOAT`.
    Float,
    /// `WATCHER_TYPE_BOOL`.
    Bool,
    /// `WATCHER_TYPE_STRING`.
    String,
    /// `WATCHER_TYPE_TUPLE`.
    Tuple,
    /// `WATCHER_TYPE_TYPE`.
    Type,
}

/// The child-visitor callback of [`Watcher::visit_children`]: receives the
/// child's label and the child itself.
pub type VisitFn<'a> = &'a mut dyn FnMut(&str, &Rc<RefCell<dyn Watcher>>);

/// The string protocol of `Watcher` (`watcher.hpp:757-833`): every node
/// answers `get`/`set` for paths relative to itself, where the empty path
/// means "this node".
pub trait Watcher {
    /// `Watcher::getAsString` (`watcher.cpp:128-158`): write the value at
    /// `path` into `result`, the node's comment into `desc` and the value's
    /// mode into `mode`. `false` when the path does not resolve.
    fn get_as_string(
        &self,
        path: &str,
        result: &mut String,
        desc: &mut String,
        mode: &mut Mode,
    ) -> bool;

    /// `Watcher::setFromString` (`watcher.cpp:202-222`): parse `value` into
    /// the node at `path`. `false` leaves the old value in place.
    fn set_from_string(&mut self, path: &str, value: &str) -> bool;

    /// `DirectoryWatcher::visitChildren`'s traversal (`watcher.cpp:246-293`):
    /// call `visit` for each child at `path`, in label order. `false` when
    /// the path does not resolve to a directory.
    fn visit_children(&self, path: &str, visit: VisitFn<'_>) -> bool {
        let _ = (path, visit);
        false
    }

    /// `Watcher::addChild` (`watcher.hpp:961`): the base watcher is a leaf
    /// and takes no children; only [`DirectoryWatcher`] accepts them.
    fn add_child(&mut self, _path: &str, _watcher: Rc<RefCell<dyn Watcher>>) -> bool {
        false
    }

    /// `Watcher::removeChild` (`watcher.hpp:974`): a leaf has nothing to
    /// remove.
    fn remove_child(&mut self, _path: &str) -> bool {
        false
    }

    /// `Watcher::getChild` (`watcher.hpp:985`): a leaf has no children.
    fn get_child(&self, _path: &str) -> Option<Rc<RefCell<dyn Watcher>>> {
        None
    }

    /// The watcher's comment (`watcher.hpp:956`, `comment_`).
    fn comment(&self) -> &str;

    /// `Watcher::setComment` (`watcher.hpp`): retitle the node.
    fn set_comment(&mut self, comment: String);
}

/// One leaf value's string protocol: `watcherValueToString`
/// (`watcher.hpp:686-706`) out, `watcherStringToValue` (`watcher.hpp:78-155`)
/// in, plus the [`DataType`] the stream protocol will report.
pub trait WatcherValue: 'static {
    /// The `WatcherDataType` a stream form of this leaf carries.
    fn data_type() -> DataType;
    /// `watcherValueToString`: the default ostream rendering.
    fn to_cpp_string(&self) -> String;
    /// `watcherStringToValue`: parse like `istream >>`, `None` on failure
    /// (the caller keeps the old value, as the reference does).
    fn from_cpp_string(value: &str) -> Option<Self>
    where
        Self: Sized;
}

impl WatcherValue for i32 {
    fn data_type() -> DataType {
        DataType::Int
    }
    fn to_cpp_string(&self) -> String {
        self.to_string()
    }
    fn from_cpp_string(value: &str) -> Option<Self> {
        parse_istream_int(value).and_then(|v| i32::try_from(v).ok())
    }
}

impl WatcherValue for u32 {
    fn data_type() -> DataType {
        DataType::Uint
    }
    fn to_cpp_string(&self) -> String {
        self.to_string()
    }
    fn from_cpp_string(value: &str) -> Option<Self> {
        // `istream >> unsigned int` reads a signed run and wraps modulo 2^32
        // ("num_get" semantics): "-5" becomes 4294967291.
        parse_istream_int(value).map(|v| (v.rem_euclid(1 << 32)) as u32)
    }
}

impl WatcherValue for i64 {
    fn data_type() -> DataType {
        DataType::Int
    }
    fn to_cpp_string(&self) -> String {
        self.to_string()
    }
    fn from_cpp_string(value: &str) -> Option<Self> {
        parse_istream_int(value)
    }
}

impl WatcherValue for u64 {
    fn data_type() -> DataType {
        DataType::Uint
    }
    fn to_cpp_string(&self) -> String {
        self.to_string()
    }
    fn from_cpp_string(value: &str) -> Option<Self> {
        parse_istream_int(value).map(|v| (v as i128).rem_euclid(1i128 << 64) as u64)
    }
}

impl WatcherValue for f32 {
    fn data_type() -> DataType {
        DataType::Float
    }
    fn to_cpp_string(&self) -> String {
        // `ostream << float` promotes to double and prints with the default
        // six significant digits.
        format_double_ostream(*self as f64)
    }
    fn from_cpp_string(value: &str) -> Option<Self> {
        parse_istream_double(value).map(|v| v as f32)
    }
}

impl WatcherValue for f64 {
    fn data_type() -> DataType {
        DataType::Float
    }
    fn to_cpp_string(&self) -> String {
        format_double_ostream(*self)
    }
    fn from_cpp_string(value: &str) -> Option<Self> {
        parse_istream_double(value)
    }
}

impl WatcherValue for bool {
    fn data_type() -> DataType {
        DataType::Bool
    }
    fn to_cpp_string(&self) -> String {
        // `watcherValueToString(bool)` (`watcher.hpp:701-705`).
        if *self {
            "true".to_string()
        } else {
            "false".to_string()
        }
    }
    fn from_cpp_string(value: &str) -> Option<Self> {
        // `watcherStringToValue(bool)` (`watcher.hpp:129-155`): "true" and
        // "false" case-insensitive, otherwise the numeric `istream` read
        // where any non-zero long is true.
        if value.eq_ignore_ascii_case("true") {
            Some(true)
        } else if value.eq_ignore_ascii_case("false") {
            Some(false)
        } else {
            parse_istream_int(value).map(|v| v != 0)
        }
    }
}

impl WatcherValue for String {
    fn data_type() -> DataType {
        DataType::String
    }
    fn to_cpp_string(&self) -> String {
        // `watcherValueToString(const BW::string &)` passes through.
        self.clone()
    }
    fn from_cpp_string(value: &str) -> Option<Self> {
        // `watcherStringToValue(BW::string &)` copies unconditionally.
        Some(value.to_string())
    }
}

/// `Watcher::partitionPath` (`watcher.cpp:574-593`): split at the *last*
/// separator. The directory half keeps its trailing slash; a path without a
/// separator is all name.
pub fn partition_path(path: &str) -> (String, String) {
    match path.rfind(SEPARATOR) {
        Some(pos) => (path[pos + 1..].to_string(), path[..pos + 1].to_string()),
        None => (path.to_string(), String::new()),
    }
}

/// `DirectoryWatcher::tail` (`watcher.cpp:522-541`): everything after the
/// first separator. The reference returns NULL without one; the empty string
/// is the port's identical "no tail" marker (`isEmptyPath`).
fn tail(path: &str) -> &str {
    match path.find(SEPARATOR) {
        Some(pos) => &path[pos + 1..],
        None => "",
    }
}

/// The first path segment, the port of the `compareLength` arithmetic in
/// `findChild`/`getChild`/`removeChild` (`watcher.cpp:477-520`).
fn head(path: &str) -> &str {
    match path.find(SEPARATOR) {
        Some(pos) => &path[..pos],
        None => path,
    }
}

/// `istream >>` for the integer family (`watcher.hpp:80-99`): skip leading
/// whitespace, an optional sign, then the longest digit run. Text after the
/// run is left unread (so "42abc" yields 42); no digits or an out-of-range
/// value fail, leaving the old value in place.
fn parse_istream_int(value: &str) -> Option<i64> {
    let s = value.trim_start();
    let (negative, digits) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    // Every accepted character is an ASCII digit, so the char count is the
    // byte length of the run.
    let run_len = digits.chars().take_while(|c| c.is_ascii_digit()).count();
    if run_len == 0 {
        return None;
    }
    let magnitude: i128 = digits[..run_len].parse().ok()?;
    let parsed: i128 = if negative { -magnitude } else { magnitude };
    i64::try_from(parsed).ok()
}

/// `istream >>` for the floating family: the longest prefix matching
/// `[+-]digits[.digits][eE[+-]digits]`, at least one mantissa digit required.
/// An infinite result for finite input fails the way the reference's
/// `failbit` does.
fn parse_istream_double(value: &str) -> Option<f64> {
    let s = value.trim_start();
    let bytes = s.as_bytes();
    let mut end = 0usize;
    if end < bytes.len() && (bytes[end] == b'+' || bytes[end] == b'-') {
        end += 1;
    }
    let mut mantissa_digits = 0usize;
    while end < bytes.len() && bytes[end].is_ascii_digit() {
        end += 1;
        mantissa_digits += 1;
    }
    if end < bytes.len() && bytes[end] == b'.' {
        end += 1;
        while end < bytes.len() && bytes[end].is_ascii_digit() {
            end += 1;
            mantissa_digits += 1;
        }
    }
    if mantissa_digits == 0 {
        return None;
    }
    if end < bytes.len() && (bytes[end] == b'e' || bytes[end] == b'E') {
        let mut exp_end = end + 1;
        if exp_end < bytes.len() && (bytes[exp_end] == b'+' || bytes[exp_end] == b'-') {
            exp_end += 1;
        }
        let mut exp_digits = 0usize;
        while exp_end < bytes.len() && bytes[exp_end].is_ascii_digit() {
            exp_end += 1;
            exp_digits += 1;
        }
        if exp_digits > 0 {
            end = exp_end;
        }
    }
    let parsed: f64 = s[..end].parse().ok()?;
    if parsed.is_infinite() {
        return None;
    }
    Some(parsed)
}

/// `ostream << double` with the default precision: six significant digits,
/// `%g`-style -- fixed notation inside the exponent window `[-4, 6)`,
/// scientific outside it, trailing zeros stripped, exponents signed and at
/// least two digits ("1e+06").
fn format_double_ostream(value: f64) -> String {
    if value.is_nan() {
        return if value.is_sign_negative() {
            "-nan".to_string()
        } else {
            "nan".to_string()
        };
    }
    if value.is_infinite() {
        return if value > 0.0 {
            "inf".to_string()
        } else {
            "-inf".to_string()
        };
    }
    if value == 0.0 {
        return if value.is_sign_negative() {
            "-0".to_string()
        } else {
            "0".to_string()
        };
    }
    const PRECISION: usize = 6;
    let precision = PRECISION as i32;
    let negative = value < 0.0;
    let magnitude = value.abs();
    // Round to six significant digits first; the rounded exponent decides
    // the notation exactly the way %g's does (9.999996e5 rounds into 1e6).
    let scientific = format!("{:.*e}", PRECISION - 1, magnitude);
    let (mantissa, exponent) = scientific.split_once('e').expect("the e notation");
    let exponent: i32 = exponent.parse().expect("a decimal exponent");
    let digits = mantissa.replace('.', "");
    let digits = digits.trim_end_matches('0');
    let digits = if digits.is_empty() { "0" } else { digits };
    let sign = if negative { "-" } else { "" };
    if (-4..precision).contains(&exponent) {
        // Fixed: place the stripped digits around the decimal point.
        if exponent >= 0 {
            let whole = digits.len() as i32 - 1;
            if whole <= exponent {
                let zeros = "0".repeat((exponent - whole) as usize);
                format!("{sign}{digits}{zeros}")
            } else {
                let split = (exponent + 1) as usize;
                format!("{sign}{}.{}", &digits[..split], &digits[split..])
            }
        } else {
            let zeros = "0".repeat((-exponent - 1) as usize);
            format!("{sign}0.{zeros}{digits}")
        }
    } else {
        let exp = if exponent < 0 {
            format!("{:03}", exponent)
        } else {
            format!("+{:02}", exponent)
        };
        let first = &digits[..1];
        let rest = &digits[1..];
        if rest.is_empty() {
            format!("{sign}{first}e{exp}")
        } else {
            format!("{sign}{first}.{rest}e{exp}")
        }
    }
}

/// `DataWatcher<TYPE>` (`watcher.hpp:2563-2724`): one typed value. The
/// reference holds `TYPE &rValue_`; the port holds the shared handle, and
/// the constructor's mode normalisation (`!= READ_WRITE && != READ_ONLY`
/// degrades to `READ_ONLY`) is kept.
pub struct DataWatcher<T: WatcherValue> {
    value: Rc<RefCell<T>>,
    access: Mode,
    comment: String,
}

impl<T: WatcherValue> DataWatcher<T> {
    /// The reference constructor (`watcher.hpp:2566-2581`).
    pub fn new(value: Rc<RefCell<T>>, access: Mode) -> Self {
        let access = match access {
            Mode::ReadWrite | Mode::ReadOnly => access,
            _ => Mode::ReadOnly,
        };
        Self {
            value,
            access,
            comment: String::new(),
        }
    }
}

impl<T: WatcherValue> Watcher for DataWatcher<T> {
    fn get_as_string(
        &self,
        path: &str,
        result: &mut String,
        desc: &mut String,
        mode: &mut Mode,
    ) -> bool {
        if path.is_empty() {
            *result = self.value.borrow().to_cpp_string();
            *desc = self.comment.clone();
            *mode = self.access;
            true
        } else {
            false
        }
    }

    fn set_from_string(&mut self, path: &str, value: &str) -> bool {
        // Only an empty path on a read-write leaf accepts a write
        // (`watcher.hpp:2593-2605`); a parse failure keeps the old value.
        if path.is_empty()
            && self.access == Mode::ReadWrite
            && let Some(parsed) = T::from_cpp_string(value)
        {
            *self.value.borrow_mut() = parsed;
            return true;
        }
        false
    }

    fn comment(&self) -> &str {
        &self.comment
    }

    fn set_comment(&mut self, comment: String) {
        self.comment = comment;
    }
}

/// One child entry: `DirData` (`watcher.hpp:1120-1140`), minus the `base`
/// offset the port folds into the watcher itself.
struct DirData {
    label: String,
    watcher: Rc<RefCell<dyn Watcher>>,
}

/// `CallableWatcher::ExposeHint` (`watcher.hpp:2877-2884`): where a debug
/// tool may route the call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExposeHint {
    /// `WITH_ENTITY`.
    WithEntity,
    /// `ALL`.
    All,
    /// `WITH_SPACE`.
    WithSpace,
    /// `LEAST_LOADED`.
    LeastLoaded,
    /// `LOCAL_ONLY`, the constructor default.
    LocalOnly,
}

/// `CallableWatcher::ArgDescription` (`watcher.hpp:2862-2875`): one
/// declared parameter's type and description.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArgDescription {
    /// The `WatcherDataType` the parameter takes.
    pub data_type: DataType,
    /// The parameter's description text.
    pub description: String,
}

/// `CallableWatcher` (`watcher.cpp:611-706`): a watcher that names a
/// function instead of holding a value. The string protocol answers reads
/// with the marker "Callable function" and refuses writes -- the reference
/// only supports invoking callables over the protocol 2 stream request
/// machinery (`setFromStream`, BW-12-01-04), and its `setFromString` is a
/// hard rejection (`watcher.cpp:663-673`).
pub struct CallableWatcher {
    expose_hint: ExposeHint,
    arg_list: Vec<ArgDescription>,
    comment: String,
}

impl CallableWatcher {
    /// The reference constructor (`watcher.cpp:622-629`), `LOCAL_ONLY` by
    /// default.
    pub fn new(hint: ExposeHint) -> Self {
        Self {
            expose_hint: hint,
            arg_list: Vec::new(),
            comment: String::new(),
        }
    }

    /// `CallableWatcher::addArg` (`watcher.cpp:635-640`): declare one
    /// parameter, in call order.
    pub fn add_arg(&mut self, data_type: DataType, description: &str) {
        self.arg_list.push(ArgDescription {
            data_type,
            description: description.to_string(),
        });
    }

    /// `CallableWatcher::exposedHint()`.
    pub fn exposed_hint(&self) -> ExposeHint {
        self.expose_hint
    }

    /// `CallableWatcher::exposeHint( hint )`.
    pub fn set_expose_hint(&mut self, hint: ExposeHint) {
        self.expose_hint = hint;
    }

    /// The declared parameters, in call order.
    pub fn arg_list(&self) -> &[ArgDescription] {
        &self.arg_list
    }

    /// `CallableWatcher::isArgsPath` (`watcher.cpp:735-742`): the pseudo
    /// path carrying the parameter descriptions.
    pub fn is_args_path(path: &str) -> bool {
        path == "__args__"
    }

    /// `CallableWatcher::isExposePath` (`watcher.cpp:749-756`): the pseudo
    /// path carrying the expose hint.
    pub fn is_expose_path(path: &str) -> bool {
        path == "__expose__"
    }
}

impl Watcher for CallableWatcher {
    fn get_as_string(
        &self,
        path: &str,
        result: &mut String,
        desc: &mut String,
        mode: &mut Mode,
    ) -> bool {
        if path.is_empty() {
            // (`watcher.cpp:644-659`): a callable answers the marker string.
            *result = "Callable function".to_string();
            *mode = Mode::Callable;
            *desc = self.comment.clone();
            true
        } else {
            false
        }
    }

    fn set_from_string(&mut self, _path: &str, _value: &str) -> bool {
        // (`watcher.cpp:663-673`): protocol v1 callable invocation is not
        // supported -- the reference logs ERROR_MSG and refuses.
        false
    }

    fn comment(&self) -> &str {
        &self.comment
    }

    fn set_comment(&mut self, comment: String) {
        self.comment = comment;
    }
}

/// `NoArgFuncCallableWatcher::Function` (`watcher.hpp:2949-2950`): fills
/// the stdio `output` and the return `value`, `false` when the call fails.
pub type NoArgFunc = Box<dyn FnMut(&mut String, &mut String) -> bool>;

/// `NoArgFuncCallableWatcher` (`watcher.cpp:836-960`): a callable backed by
/// one function. The reference invokes it from `setFromStream` after the
/// protocol 2 argument tuple decodes; the port keeps the same `onCall`
/// contract behind a stored closure, and the wire decode lands with the
/// request machinery (BW-12-01-04).
pub struct NoArgFuncCallableWatcher {
    callable: CallableWatcher,
    func: NoArgFunc,
}

impl NoArgFuncCallableWatcher {
    /// The reference constructor (`watcher.cpp:944-951`).
    pub fn new(func: NoArgFunc, hint: ExposeHint) -> Self {
        Self {
            callable: CallableWatcher::new(hint),
            func,
        }
    }

    /// `NoArgCallableWatcher::onCall` (`watcher.cpp:925-932`): run the
    /// function, returning `(stdio output, return value)`; `None` when the
    /// function fails, the shape `setFromStream` treats as a failed call.
    pub fn on_call(&mut self) -> Option<(String, String)> {
        let mut output = String::new();
        let mut value = String::new();
        if (self.func)(&mut output, &mut value) {
            Some((output, value))
        } else {
            None
        }
    }
}

impl Watcher for NoArgFuncCallableWatcher {
    fn get_as_string(
        &self,
        path: &str,
        result: &mut String,
        desc: &mut String,
        mode: &mut Mode,
    ) -> bool {
        self.callable.get_as_string(path, result, desc, mode)
    }

    fn set_from_string(&mut self, path: &str, value: &str) -> bool {
        self.callable.set_from_string(path, value)
    }

    fn comment(&self) -> &str {
        &self.callable.comment
    }

    fn set_comment(&mut self, comment: String) {
        self.callable.set_comment(comment);
    }
}

/// `DirectoryWatcher` (`watcher.cpp:115-544`): the tree node. Children are
/// kept sorted by label -- the reference builds with `SORTED_WATCHERS`
/// (`watcher.cpp:96`).
pub struct DirectoryWatcher {
    children: Vec<DirData>,
    comment: String,
}

impl DirectoryWatcher {
    /// `DirectoryWatcher::DirectoryWatcher()` (`watcher.cpp:115-118`).
    pub fn new() -> Self {
        Self {
            children: Vec::new(),
            comment: String::new(),
        }
    }

    /// `findChild` (`watcher.cpp:477-520`): the child matching the first
    /// path segment. An empty segment fails the way the reference's
    /// `ERROR_MSG` arm does.
    fn find_child(&self, path: &str) -> Option<&DirData> {
        let segment = head(path);
        if segment.is_empty() {
            return None;
        }
        self.children.iter().find(|d| d.label == segment)
    }
}

impl Default for DirectoryWatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl Watcher for DirectoryWatcher {
    fn get_as_string(
        &self,
        path: &str,
        result: &mut String,
        desc: &mut String,
        mode: &mut Mode,
    ) -> bool {
        if path.is_empty() {
            // (`watcher.cpp:134-141`): a directory answers "<DIR>".
            *result = "<DIR>".to_string();
            *desc = self.comment.clone();
            *mode = Mode::Directory;
            return true;
        }
        match self.find_child(path) {
            Some(found) => found
                .watcher
                .borrow()
                .get_as_string(tail(path), result, desc, mode),
            None => false,
        }
    }

    fn set_from_string(&mut self, path: &str, value: &str) -> bool {
        match self.find_child(path) {
            Some(found) => found
                .watcher
                .borrow_mut()
                .set_from_string(tail(path), value),
            None => false,
        }
    }

    fn visit_children(&self, path: &str, visit: VisitFn<'_>) -> bool {
        if path.is_empty() {
            for child in &self.children {
                visit(child.label.as_str(), &child.watcher);
            }
            return true;
        }
        match self.find_child(path) {
            Some(found) => found.watcher.borrow().visit_children(tail(path), visit),
            None => false,
        }
    }

    fn comment(&self) -> &str {
        &self.comment
    }

    fn set_comment(&mut self, comment: String) {
        self.comment = comment;
    }

    fn add_child(&mut self, path: &str, watcher: Rc<RefCell<dyn Watcher>>) -> bool {
        if path.is_empty() {
            return false;
        }
        if !path.contains(SEPARATOR) {
            if self.find_child(path).is_some() {
                return false;
            }
            let position = self.children.partition_point(|d| d.label.as_str() < path);
            self.children.insert(
                position,
                DirData {
                    label: path.to_string(),
                    watcher,
                },
            );
            return true;
        }
        // The path descends: find or create the first-segment directory,
        // then recurse with the tail (`watcher.cpp:343-375`).
        let segment = head(path).to_string();
        let existing = self.find_child(&segment);
        let child = match existing {
            Some(found) => Rc::clone(&found.watcher),
            None => {
                let position = self
                    .children
                    .partition_point(|d| d.label.as_str() < segment.as_str());
                let created: Rc<RefCell<dyn Watcher>> =
                    Rc::new(RefCell::new(DirectoryWatcher::new()));
                self.children.insert(
                    position,
                    DirData {
                        label: segment,
                        watcher: Rc::clone(&created),
                    },
                );
                created
            }
        };
        child.borrow_mut().add_child(tail(path), watcher)
    }

    fn remove_child(&mut self, path: &str) -> bool {
        if path.is_empty() {
            return false;
        }
        let segment = head(path);
        let Some(position) = self.children.iter().position(|d| d.label == segment) else {
            return false;
        };
        if path.contains(SEPARATOR) {
            let child = Rc::clone(&self.children[position].watcher);
            child.borrow_mut().remove_child(tail(path))
        } else {
            self.children.remove(position);
            true
        }
    }

    fn get_child(&self, path: &str) -> Option<Rc<RefCell<dyn Watcher>>> {
        if path.is_empty() {
            return None;
        }
        let segment = head(path);
        let found = self.children.iter().find(|d| d.label == segment)?;
        if path.contains(SEPARATOR) {
            found.watcher.borrow().get_child(tail(path))
        } else {
            Some(Rc::clone(&found.watcher))
        }
    }
}

// The process root, `g_pRootWatcher` (`watcher.cpp:61-70`): a lazily
// created [`DirectoryWatcher`], dropped by [`fini`]. The reference's root
// is a process global that only the main loop touches; the port makes it
// thread-local -- the same shape for every real caller, and each test
// thread owns an isolated tree.
thread_local! {
    static ROOT: RefCell<Option<Rc<RefCell<DirectoryWatcher>>>> = const { RefCell::new(None) };
}

/// `Watcher::hasRootWatcherInternal()` (`watcher.cpp:50-54`): probe without
/// creating.
pub fn has_root() -> bool {
    ROOT.with(|root| root.borrow().is_some())
}

/// `Watcher::rootWatcherInternal()` (`watcher.cpp:61-70`).
pub fn root() -> Rc<RefCell<DirectoryWatcher>> {
    ROOT.with(|root| {
        root.borrow_mut()
            .get_or_insert_with(|| Rc::new(RefCell::new(DirectoryWatcher::new())))
            .clone()
    })
}

/// `Watcher::finiInternal()` (`watcher.cpp:72-80`).
pub fn fini() {
    ROOT.with(|root| *root.borrow_mut() = None);
}

/// The free `addWatcher(path, watcher)` registration
/// (`watcher.hpp:3101-3113` shape): adds into [`root`], returning the
/// watcher on success and `None` when the tree rejected the path.
pub fn add_watcher(
    path: &str,
    watcher: Rc<RefCell<dyn Watcher>>,
) -> Option<Rc<RefCell<dyn Watcher>>> {
    let root = root();
    if root.borrow_mut().add_child(path, Rc::clone(&watcher)) {
        Some(watcher)
    } else {
        None
    }
}

/// The `addWatcher(path, rValue, access, comment)` overload
/// (`watcher.hpp:3197-3219`): wrap a shared value in a [`DataWatcher`] and
/// register it.
pub fn add_watcher_value<T: WatcherValue>(
    path: &str,
    value: &Rc<RefCell<T>>,
    access: Mode,
    comment: &str,
) -> Option<Rc<RefCell<dyn Watcher>>> {
    let mut leaf = DataWatcher::new(Rc::clone(value), access);
    leaf.set_comment(comment.to_string());
    let registered: Rc<RefCell<dyn Watcher>> = Rc::new(RefCell::new(leaf));
    add_watcher(path, Rc::clone(&registered))
}

/// `getWatcher(path)` (`watcher.hpp:3233-3238`).
pub fn get_watcher(path: &str) -> Option<Rc<RefCell<dyn Watcher>>> {
    root().borrow().get_child(path)
}

/// The top-level string read, the entry the query machinery takes against
/// the root (`rootWatcher()->getAsString( path, ... )`): the value string
/// for `path`, with the node's comment written into `desc` and its mode
/// into `mode`. `None` when the path does not resolve; the empty path is
/// the root itself and answers "<DIR>".
pub fn get_watcher_string(path: &str, desc: &mut String, mode: &mut Mode) -> Option<String> {
    let mut result = String::new();
    if root().borrow().get_as_string(path, &mut result, desc, mode) {
        Some(result)
    } else {
        None
    }
}

/// The top-level string write (`rootWatcher()->setFromString( path, ... )`):
/// parse `value` into the leaf at `path`. `false` on a missing path, a
/// non-writable node or a parse failure -- every rejected shape leaves the
/// old value in place.
pub fn set_watcher_string(path: &str, value: &str) -> bool {
    root().borrow_mut().set_from_string(path, value)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A read-write i32 leaf at "game/rounds" answers its value and mode.
    #[test]
    fn an_int_leaf_answers_its_value_and_mode() {
        fini();
        let rounds = Rc::new(RefCell::new(3i32));
        add_watcher_value("game/rounds", &rounds, Mode::ReadWrite, "round count");
        let mut result = String::new();
        let mut desc = String::new();
        let mut mode = Mode::Invalid;
        assert!(
            get_watcher("game/rounds")
                .expect("registered")
                .borrow()
                .get_as_string("", &mut result, &mut desc, &mut mode)
        );
        assert_eq!(result, "3");
        assert_eq!(desc, "round count");
        assert_eq!(mode, Mode::ReadWrite);
    }

    /// Every scalar type keeps its reference data type classification and
    /// its string rendering (`watcherValueToString` semantics).
    #[test]
    fn each_leaf_type_answers_its_data_type_and_string() {
        assert_eq!(i32::data_type(), DataType::Int);
        assert_eq!(i64::data_type(), DataType::Int);
        assert_eq!(u32::data_type(), DataType::Uint);
        assert_eq!(u64::data_type(), DataType::Uint);
        assert_eq!(f32::data_type(), DataType::Float);
        assert_eq!(f64::data_type(), DataType::Float);
        assert_eq!(bool::data_type(), DataType::Bool);
        assert_eq!(String::data_type(), DataType::String);
        assert_eq!(7i64.to_cpp_string(), "7");
        assert_eq!(7u32.to_cpp_string(), "7");
        assert_eq!(7u64.to_cpp_string(), "7");
        assert_eq!(f32::from_cpp_string("1.5"), Some(1.5));
        assert_eq!(f32::from_cpp_string("abc"), None);
        assert_eq!(true.to_cpp_string(), "true");
        assert_eq!(false.to_cpp_string(), "false");
        assert_eq!("abc".to_string().to_cpp_string(), "abc");
        assert_eq!((-2i32).to_cpp_string(), "-2");
    }

    /// Sets parse like `istream >>`: trailing text is left unread, no digits
    /// fails and keeps the old value.
    #[test]
    fn an_int_set_parses_like_the_istream_extraction() {
        fini();
        let rounds = Rc::new(RefCell::new(1i32));
        add_watcher_value("game/rounds", &rounds, Mode::ReadWrite, "");
        let watcher = get_watcher("game/rounds").expect("registered");
        assert!(watcher.borrow_mut().set_from_string("", "42abc"));
        assert_eq!(*rounds.borrow(), 42);
        // No digits: the reference leaves the old value and returns false.
        assert!(!watcher.borrow_mut().set_from_string("", "abc"));
        assert_eq!(*rounds.borrow(), 42);
        assert!(!watcher.borrow_mut().set_from_string("", "99999999999"));
        assert_eq!(*rounds.borrow(), 42);
        assert!(watcher.borrow_mut().set_from_string("", " -7"));
        assert_eq!(*rounds.borrow(), -7);
    }

    /// Unsigned leaves wrap a negative run modulo 2^N the way the reference
    /// "num_get" does; out-of-range magnitudes fail.
    #[test]
    fn an_uint_set_wraps_negatives_and_rejects_overflow() {
        assert_eq!(u32::from_cpp_string("-5"), Some(4294967291));
        assert_eq!(u32::from_cpp_string("12"), Some(12));
        assert_eq!(u64::from_cpp_string("-1"), Some(u64::MAX));
        assert_eq!(u64::from_cpp_string("18446744073709551616"), None);
        assert_eq!(i64::from_cpp_string("9223372036854775808"), None);
    }

    /// Float leaves parse leading runs and render six significant digits.
    #[test]
    fn a_float_leaf_matches_the_ostream_defaults() {
        fini();
        let rate = Rc::new(RefCell::new(0f64));
        add_watcher_value("game/rate", &rate, Mode::ReadWrite, "");
        let watcher = get_watcher("game/rate").expect("registered");
        assert!(watcher.borrow_mut().set_from_string("", "2.5rest"));
        assert_eq!(*rate.borrow(), 2.5);
        // The canonical six-digit rendering example, not a PI computation.
        #[allow(clippy::approx_constant)]
        let value = 3.14;
        assert_eq!(format_double_ostream(value), "3.14");
        assert_eq!(format_double_ostream(100000.0), "100000");
        assert_eq!(format_double_ostream(1000000.0), "1e+06");
        assert_eq!(format_double_ostream(0.0001), "0.0001");
        assert_eq!(format_double_ostream(0.00001), "1e-05");
        assert_eq!(format_double_ostream(123456789.0), "1.23457e+08");
        assert_eq!(format_double_ostream(-0.5), "-0.5");
        assert_eq!(format_double_ostream(0.0), "0");
        assert_eq!(format_double_ostream(f64::INFINITY), "inf");
        assert_eq!(format_double_ostream(f64::NEG_INFINITY), "-inf");
        assert_eq!(format_double_ostream(f64::NAN), "nan");
        assert_eq!(format_double_ostream(-f64::NAN), "-nan");
        assert_eq!(format_double_ostream(-0.0), "-0");
        assert_eq!(0f32.to_cpp_string(), "0");
        // `istream >>` for the floating family: signs, exponents, and the
        // failure shapes (no digits; infinite for finite input).
        assert_eq!(f64::from_cpp_string("-2.5"), Some(-2.5));
        assert_eq!(f64::from_cpp_string("+1e2"), Some(100.0));
        assert_eq!(f64::from_cpp_string("2.5E-3"), Some(0.0025));
        assert_eq!(f64::from_cpp_string(".5"), Some(0.5));
        assert_eq!(f64::from_cpp_string("1e"), Some(1.0));
        assert_eq!(f64::from_cpp_string("abc"), None);
        assert_eq!(f64::from_cpp_string("-"), None);
        assert_eq!(f64::from_cpp_string("1e999"), None);
        // The leaf answers gets through the same string protocol.
        let mut result = String::new();
        let mut desc = String::new();
        let mut mode = Mode::Invalid;
        assert!(
            watcher
                .borrow()
                .get_as_string("", &mut result, &mut desc, &mut mode)
        );
        assert_eq!(result, "2.5");
        assert_eq!(mode, Mode::ReadWrite);
    }

    /// Bool leaves accept case-insensitive words and the numeric read where
    /// any non-zero long is true.
    #[test]
    fn a_bool_leaf_reads_words_then_numbers() {
        assert_eq!(bool::from_cpp_string("TRUE"), Some(true));
        assert_eq!(bool::from_cpp_string("False"), Some(false));
        assert_eq!(bool::from_cpp_string("0"), Some(false));
        assert_eq!(bool::from_cpp_string("2"), Some(true));
        assert_eq!(bool::from_cpp_string("yes"), None);
        let enabled = Rc::new(RefCell::new(false));
        fini();
        add_watcher_value("flags/enabled", &enabled, Mode::ReadWrite, "");
        assert!(
            get_watcher("flags/enabled")
                .expect("registered")
                .borrow_mut()
                .set_from_string("", "true")
        );
        assert!(*enabled.borrow());
    }

    /// A read-only leaf refuses sets but answers gets.
    #[test]
    fn a_read_only_leaf_refuses_sets() {
        fini();
        let locked = Rc::new(RefCell::new(9i32));
        add_watcher_value("game/locked", &locked, Mode::ReadOnly, "");
        let watcher = get_watcher("game/locked").expect("registered");
        assert!(!watcher.borrow_mut().set_from_string("", "10"));
        assert_eq!(*locked.borrow(), 9);
        let mut result = String::new();
        let mut desc = String::new();
        let mut mode = Mode::Invalid;
        assert!(
            watcher
                .borrow()
                .get_as_string("", &mut result, &mut desc, &mut mode)
        );
        assert_eq!(result, "9");
        assert_eq!(mode, Mode::ReadOnly);
    }

    /// A directory answers "<DIR>" with the directory mode and its comment.
    #[test]
    fn a_directory_answers_dir_with_its_comment() {
        fini();
        let rounds = Rc::new(RefCell::new(0i32));
        add_watcher_value("game/rounds", &rounds, Mode::ReadWrite, "");
        let mut result = String::new();
        let mut desc = String::new();
        let mut mode = Mode::Invalid;
        assert!(
            get_watcher("game")
                .expect("intermediate created")
                .borrow()
                .get_as_string("", &mut result, &mut desc, &mut mode)
        );
        assert_eq!(result, "<DIR>");
        assert_eq!(mode, Mode::Directory);
    }

    /// `visit_children` walks label-sorted children: the reference build
    /// defines `SORTED_WATCHERS` (`watcher.cpp:96`).
    #[test]
    fn children_enumerate_in_label_order() {
        fini();
        let zero = Rc::new(RefCell::new(0i32));
        // Register out of order; the tree sorts by label.
        add_watcher_value("game/zebra", &zero, Mode::ReadOnly, "");
        add_watcher_value("game/apple", &zero, Mode::ReadOnly, "");
        add_watcher_value("game/mango", &zero, Mode::ReadWrite, "");
        let mut labels = Vec::new();
        get_watcher("game")
            .expect("directory")
            .borrow()
            .visit_children("", &mut |label, _| labels.push(label.to_string()));
        assert_eq!(labels, vec!["apple", "mango", "zebra"]);
        // A deeper path visits through the tree.
        let mut seen = false;
        assert!(root().borrow().visit_children("game", &mut |label, _| {
            if label == "apple" {
                seen = true;
            }
        }));
        assert!(seen);
        assert!(
            !get_watcher("game")
                .expect("directory")
                .borrow()
                .visit_children("nope", &mut |_, _| ())
        );
    }

    /// A duplicate leaf label is rejected and the original stays.
    #[test]
    fn a_duplicate_leaf_label_is_rejected() {
        fini();
        let first = Rc::new(RefCell::new(1i32));
        let second = Rc::new(RefCell::new(2i32));
        assert!(add_watcher_value("game/rounds", &first, Mode::ReadWrite, "").is_some());
        assert!(add_watcher_value("game/rounds", &second, Mode::ReadWrite, "").is_none());
        let mut result = String::new();
        let mut desc = String::new();
        let mut mode = Mode::Invalid;
        assert!(
            get_watcher("game/rounds")
                .expect("original kept")
                .borrow()
                .get_as_string("", &mut result, &mut desc, &mut mode)
        );
        assert_eq!(result, "1");
    }

    /// Deep paths auto-create intermediate directories; `getWatcher` and
    /// `DirectoryWatcher::getChild` agree on them.
    #[test]
    fn deep_paths_create_intermediate_directories() {
        fini();
        let value = Rc::new(RefCell::new(5i32));
        assert!(add_watcher_value("a/b/c/deep", &value, Mode::ReadWrite, "").is_some());
        let via_root = get_watcher("a/b/c/deep").expect("root lookup");
        let mut result = String::new();
        let mut desc = String::new();
        let mut mode = Mode::Invalid;
        assert!(
            via_root
                .borrow()
                .get_as_string("", &mut result, &mut desc, &mut mode)
        );
        assert_eq!(result, "5");
        // `getChild` walks the same tree from the root directory itself.
        let via_child = root()
            .borrow()
            .get_child("a/b/c/deep")
            .expect("child lookup");
        assert_eq!(
            Rc::as_ptr(&via_root) as *const (),
            Rc::as_ptr(&via_child) as *const ()
        );
        assert!(root().borrow().get_child("a/missing").is_none());
        // A second deep registration whose intermediate is created into a
        // non-empty directory exercises the sorted insert position
        // (`watcher.cpp:343-375`).
        assert!(add_watcher_value("a/m/deep2", &value, Mode::ReadWrite, "").is_some());
        assert!(get_watcher("a/m/deep2").is_some());
    }

    /// `removeChild` takes a leaf out and prunes deeper paths.
    #[test]
    fn remove_child_takes_leaves_and_deeper_paths_out() {
        fini();
        let value = Rc::new(RefCell::new(1i32));
        add_watcher_value("a/b/leaf", &value, Mode::ReadWrite, "");
        add_watcher_value("a/other", &value, Mode::ReadWrite, "");
        assert!(root().borrow_mut().remove_child("a/b/leaf"));
        assert!(get_watcher("a/b/leaf").is_none());
        assert!(get_watcher("a/other").is_some());
        assert!(!root().borrow_mut().remove_child("a/absent"));
    }

    /// `partition_path` splits at the last separator, directory keeping its
    /// trailing slash (`watcher.cpp:574-593`).
    #[test]
    fn partition_path_splits_at_the_last_separator() {
        let (name, directory) = partition_path("section1/section2/hello");
        assert_eq!(name, "hello");
        assert_eq!(directory, "section1/section2/");
        let (name, directory) = partition_path("plain");
        assert_eq!(name, "plain");
        assert_eq!(directory, "");
    }

    /// A leaf rejects non-empty get/set paths; a directory rejects sets at
    /// its own node.
    #[test]
    fn a_leaf_rejects_deeper_paths_and_a_directory_rejects_sets() {
        fini();
        let value = Rc::new(RefCell::new(1i32));
        add_watcher_value("leaf", &value, Mode::ReadWrite, "");
        let watcher = get_watcher("leaf").expect("registered");
        assert!(!watcher.borrow_mut().set_from_string("deeper", "2"));
        let mut result = String::new();
        let mut desc = String::new();
        let mut mode = Mode::Invalid;
        assert!(
            !watcher
                .borrow()
                .get_as_string("deeper", &mut result, &mut desc, &mut mode)
        );
        assert!(!root().borrow_mut().set_from_string("", "2"));
    }

    /// A leaf answers the reference base-class defaults for the tree
    /// methods (`watcher.hpp:961-990`): it takes no children and visits
    /// none.
    #[test]
    fn a_leaf_rejects_the_tree_methods() {
        fini();
        let value = Rc::new(RefCell::new(1i32));
        let registered = add_watcher_value("leafonly", &value, Mode::ReadWrite, "").expect("ok");
        let child: Rc<RefCell<dyn Watcher>> = Rc::new(RefCell::new(DataWatcher::new(
            Rc::new(RefCell::new(2i32)),
            Mode::ReadWrite,
        )));
        assert!(!registered.borrow_mut().add_child("child", child));
        assert!(!registered.borrow_mut().remove_child("child"));
        assert!(registered.borrow().get_child("child").is_none());
        assert!(!registered.borrow().visit_children("", &mut |_, _| ()));
        assert!(root().borrow().get_child("leafonly/deeper").is_none());
    }

    /// The root registry is lazily created, probeable and finitable
    /// (`hasRootWatcherInternal`/`rootWatcherInternal`/`finiInternal`).
    #[test]
    fn the_root_registry_lives_lazily_and_finites() {
        fini();
        assert!(!has_root());
        let _ = root();
        assert!(has_root());
        fini();
        assert!(!has_root());
        // A failed add (duplicate label after two registrations) still
        // returns None from the free function.
        let value = Rc::new(RefCell::new(1i32));
        assert!(add_watcher_value("dup", &value, Mode::ReadWrite, "").is_some());
        assert!(add_watcher_value("dup", &value, Mode::ReadWrite, "").is_none());
        assert!(add_watcher_value("", &value, Mode::ReadWrite, "").is_none());
        fini();
    }

    /// `tail`/`head` mirror the reference's separator arithmetic, including
    /// paths that start with a separator (empty segment fails).
    #[test]
    fn path_arithmetic_matches_the_reference_helpers() {
        assert_eq!(tail("a/b/c"), "b/c");
        assert_eq!(tail("abc"), "");
        assert_eq!(head("a/b/c"), "a");
        assert_eq!(head("abc"), "abc");
        fini();
        let value = Rc::new(RefCell::new(1i32));
        add_watcher_value("x/y", &value, Mode::ReadWrite, "");
        // "/x/y" starts with an empty segment; neither get nor set resolves.
        assert!(!root().borrow_mut().set_from_string("/x/y", "2"));
        let mut result = String::new();
        let mut desc = String::new();
        let mut mode = Mode::Invalid;
        assert!(
            !root()
                .borrow()
                .get_as_string("/x/y", &mut result, &mut desc, &mut mode)
        );
    }

    /// Strings pass through both directions unchanged
    /// (`watcher.hpp:121-127`, `watcher.hpp:695-699`).
    #[test]
    fn a_string_leaf_passes_through_both_ways() {
        fini();
        let name = Rc::new(RefCell::new("hello".to_string()));
        add_watcher_value("game/name", &name, Mode::ReadWrite, "");
        let watcher = get_watcher("game/name").expect("registered");
        assert!(watcher.borrow_mut().set_from_string("", "any text at all"));
        assert_eq!(*name.borrow(), "any text at all");
    }

    /// Comment retitling works on both node kinds (`setComment`).
    #[test]
    fn comments_can_be_retitled() {
        fini();
        let value = Rc::new(RefCell::new(1i32));
        let registered = add_watcher_value("c/leaf", &value, Mode::ReadWrite, "first").expect("ok");
        registered.borrow_mut().set_comment("second".to_string());
        assert_eq!(registered.borrow().comment(), "second");
        let directory = get_watcher("c").expect("directory");
        assert_eq!(directory.borrow().comment(), "");
        directory
            .borrow_mut()
            .set_comment("dir comment".to_string());
        assert_eq!(directory.borrow().comment(), "dir comment");
    }

    /// `DataWatcher::new` normalises non read modes to read-only
    /// (`watcher.hpp:2570-2573`).
    #[test]
    fn the_data_watcher_normalises_its_access_mode() {
        let value = Rc::new(RefCell::new(1i32));
        let leaf = DataWatcher::new(value, Mode::Callable);
        let mut result = String::new();
        let mut desc = String::new();
        let mut mode = Mode::Invalid;
        assert!(leaf.get_as_string("", &mut result, &mut desc, &mut mode));
        assert_eq!(mode, Mode::ReadOnly);
    }

    /// A bare `DirectoryWatcher` registered by hand serves the same string
    /// protocol as one the tree created.
    #[test]
    fn a_hand_registered_directory_serves_the_protocol() {
        fini();
        let leaf: Rc<RefCell<dyn Watcher>> = Rc::new(RefCell::new(DataWatcher::new(
            Rc::new(RefCell::new(7i32)),
            Mode::ReadWrite,
        )));
        let mut directory = DirectoryWatcher::default();
        assert!(directory.add_child("inner", Rc::clone(&leaf)));
        assert!(!directory.add_child("", Rc::clone(&leaf)));
        let mut result = String::new();
        let mut desc = String::new();
        let mut mode = Mode::Invalid;
        assert!(directory.get_as_string("inner", &mut result, &mut desc, &mut mode));
        assert_eq!(result, "7");
        assert!(directory.set_from_string("inner", "8"));
        assert!(directory.get_child("inner").is_some());
        assert!(directory.get_child("").is_none());
        assert!(!directory.remove_child(""));
        assert!(directory.remove_child("inner"));
    }

    /// The root get/set entries answer registered leaves and reject every
    /// non-resolving shape (`rootWatcher()->getAsString`/`setFromString`).
    #[test]
    fn the_root_get_set_entries_answer_and_reject() {
        fini();
        let rounds = Rc::new(RefCell::new(3i32));
        add_watcher_value("game/rounds", &rounds, Mode::ReadWrite, "round count");
        // Read: value, comment and mode come back together.
        let mut desc = String::new();
        let mut mode = Mode::Invalid;
        assert_eq!(
            get_watcher_string("game/rounds", &mut desc, &mut mode),
            Some("3".to_string())
        );
        assert_eq!(desc, "round count");
        assert_eq!(mode, Mode::ReadWrite);
        // Write through the entry, then read the new value back.
        assert!(set_watcher_string("game/rounds", "7"));
        assert_eq!(*rounds.borrow(), 7);
        assert_eq!(
            get_watcher_string("game/rounds", &mut desc, &mut mode),
            Some("7".to_string())
        );
        // The root itself answers "<DIR>" at the empty path.
        assert_eq!(
            get_watcher_string("", &mut desc, &mut mode),
            Some("<DIR>".to_string())
        );
        assert_eq!(mode, Mode::Directory);
        // The intermediate directory answers the same shape.
        assert_eq!(
            get_watcher_string("game", &mut desc, &mut mode),
            Some("<DIR>".to_string())
        );
        // Rejections: missing paths, directory writes, empty segments.
        assert_eq!(
            get_watcher_string("game/absent", &mut desc, &mut mode),
            None
        );
        assert!(!set_watcher_string("game/absent", "1"));
        assert!(!set_watcher_string("", "1"));
        assert!(!set_watcher_string("game", "1"));
        assert!(!set_watcher_string("/game/rounds", "1"));
        assert_eq!(*rounds.borrow(), 7);
    }

    /// Every rejected write leaves the old value in place; a read-only leaf
    /// refuses through the entry but keeps answering reads.
    #[test]
    fn rejected_writes_leave_old_values_in_place() {
        fini();
        let locked = Rc::new(RefCell::new(9i32));
        add_watcher_value("game/locked", &locked, Mode::ReadOnly, "locked leaf");
        let mut desc = String::new();
        let mut mode = Mode::Invalid;
        assert_eq!(
            get_watcher_string("game/locked", &mut desc, &mut mode),
            Some("9".to_string())
        );
        assert_eq!(mode, Mode::ReadOnly);
        assert_eq!(desc, "locked leaf");
        assert!(!set_watcher_string("game/locked", "10"));
        assert_eq!(*locked.borrow(), 9);
        // A parse failure keeps the old value; a trailing-garbage run parses.
        let rounds = Rc::new(RefCell::new(1i32));
        add_watcher_value("game/rounds", &rounds, Mode::ReadWrite, "");
        assert!(!set_watcher_string("game/rounds", "abc"));
        assert_eq!(*rounds.borrow(), 1);
        assert!(!set_watcher_string("game/rounds", "99999999999"));
        assert_eq!(*rounds.borrow(), 1);
        assert!(!set_watcher_string("game/rounds", "true"));
        assert_eq!(*rounds.borrow(), 1);
        assert!(set_watcher_string("game/rounds", "42abc"));
        assert_eq!(*rounds.borrow(), 42);
        assert_eq!(
            get_watcher_string("game/rounds", &mut desc, &mut mode),
            Some("42".to_string())
        );
    }

    /// Every scalar type round-trips through the root get/set entries with
    /// the reference renderings; per-leaf access modes report correctly.
    #[test]
    fn every_scalar_type_round_trips_through_the_root_entry() {
        fini();
        let int_leaf = Rc::new(RefCell::new(0i32));
        let uint_leaf = Rc::new(RefCell::new(0u32));
        let int64_leaf = Rc::new(RefCell::new(0i64));
        let uint64_leaf = Rc::new(RefCell::new(0u64));
        let float32_leaf = Rc::new(RefCell::new(0f32));
        let float64_leaf = Rc::new(RefCell::new(0f64));
        let bool_leaf = Rc::new(RefCell::new(false));
        let string_leaf = Rc::new(RefCell::new(String::new()));
        add_watcher_value("types/i", &int_leaf, Mode::ReadWrite, "");
        add_watcher_value("types/u", &uint_leaf, Mode::ReadWrite, "");
        add_watcher_value("types/i64", &int64_leaf, Mode::ReadWrite, "");
        add_watcher_value("types/u64", &uint64_leaf, Mode::ReadOnly, "");
        add_watcher_value("types/f32", &float32_leaf, Mode::ReadWrite, "");
        add_watcher_value("types/f64", &float64_leaf, Mode::ReadWrite, "");
        add_watcher_value("types/b", &bool_leaf, Mode::ReadWrite, "");
        add_watcher_value("types/s", &string_leaf, Mode::ReadWrite, "");
        let mut desc = String::new();
        let mut mode = Mode::Invalid;
        // i32: signed write and read.
        assert!(set_watcher_string("types/i", "-12"));
        assert_eq!(*int_leaf.borrow(), -12);
        assert_eq!(
            get_watcher_string("types/i", &mut desc, &mut mode),
            Some("-12".to_string())
        );
        // u32: negative runs wrap modulo 2^32 ("num_get" semantics).
        assert!(set_watcher_string("types/u", "-5"));
        assert_eq!(*uint_leaf.borrow(), 4294967291);
        assert_eq!(
            get_watcher_string("types/u", &mut desc, &mut mode),
            Some("4294967291".to_string())
        );
        // i64: full range.
        assert!(set_watcher_string("types/i64", "9223372036854775807"));
        assert_eq!(*int64_leaf.borrow(), i64::MAX);
        assert_eq!(
            get_watcher_string("types/i64", &mut desc, &mut mode),
            Some("9223372036854775807".to_string())
        );
        // u64: read-only mode reports through the entry and refuses writes.
        assert_eq!(mode, Mode::ReadWrite);
        assert_eq!(
            get_watcher_string("types/u64", &mut desc, &mut mode),
            Some("0".to_string())
        );
        assert_eq!(mode, Mode::ReadOnly);
        assert!(!set_watcher_string("types/u64", "1"));
        assert_eq!(*uint64_leaf.borrow(), 0);
        // f32: plain rendering.
        assert!(set_watcher_string("types/f32", "2.5"));
        assert_eq!(*float32_leaf.borrow(), 2.5);
        assert_eq!(
            get_watcher_string("types/f32", &mut desc, &mut mode),
            Some("2.5".to_string())
        );
        // f64: six-significant-digit rendering on read.
        assert!(set_watcher_string("types/f64", "0.00001"));
        assert_eq!(*float64_leaf.borrow(), 0.00001);
        assert_eq!(
            get_watcher_string("types/f64", &mut desc, &mut mode),
            Some("1e-05".to_string())
        );
        // bool: case-insensitive words in, lowercase "true"/"false" out.
        assert!(set_watcher_string("types/b", "TRUE"));
        assert!(*bool_leaf.borrow());
        assert_eq!(
            get_watcher_string("types/b", &mut desc, &mut mode),
            Some("true".to_string())
        );
        assert!(set_watcher_string("types/b", "0"));
        assert!(!*bool_leaf.borrow());
        assert_eq!(
            get_watcher_string("types/b", &mut desc, &mut mode),
            Some("false".to_string())
        );
        // String: text passes through both directions, spaces included.
        assert!(set_watcher_string("types/s", "any text at all"));
        assert_eq!(*string_leaf.borrow(), "any text at all");
        assert_eq!(
            get_watcher_string("types/s", &mut desc, &mut mode),
            Some("any text at all".to_string())
        );
    }

    /// A callable watcher answers the marker string with the callable mode
    /// and refuses every string-protocol write (`watcher.cpp:644-673`).
    #[test]
    fn a_callable_answers_the_marker_and_refuses_writes() {
        let mut callable = CallableWatcher::new(ExposeHint::LocalOnly);
        callable.set_comment("do a thing".to_string());
        let mut result = String::new();
        let mut desc = String::new();
        let mut mode = Mode::Invalid;
        assert!(callable.get_as_string("", &mut result, &mut desc, &mut mode));
        assert_eq!(result, "Callable function");
        assert_eq!(mode, Mode::Callable);
        assert_eq!(desc, "do a thing");
        // Any non-empty path does not resolve.
        assert!(!callable.get_as_string("sub", &mut result, &mut desc, &mut mode));
        // Protocol v1 invocation is not supported: every set refuses.
        assert!(!callable.set_from_string("", ""));
        assert!(!callable.set_from_string("sub", "1"));
        assert_eq!(callable.comment(), "do a thing");
    }

    /// Parameters declare in call order with their types and descriptions
    /// (`CallableWatcher::addArg`, `watcher.cpp:635-640`).
    #[test]
    fn a_callable_declares_parameters_in_order_with_types() {
        let mut callable = CallableWatcher::new(ExposeHint::All);
        callable.add_arg(DataType::Int, "count");
        callable.add_arg(DataType::Bool, "enable");
        assert_eq!(
            callable.arg_list(),
            &[
                ArgDescription {
                    data_type: DataType::Int,
                    description: "count".to_string()
                },
                ArgDescription {
                    data_type: DataType::Bool,
                    description: "enable".to_string()
                },
            ]
        );
    }

    /// The expose hint defaults to `LOCAL_ONLY` and resets through its
    /// accessor (`watcher.hpp:2886-2892`).
    #[test]
    fn an_expose_hint_defaults_local_and_resets() {
        let mut callable = CallableWatcher::new(ExposeHint::LocalOnly);
        assert_eq!(callable.exposed_hint(), ExposeHint::LocalOnly);
        callable.set_expose_hint(ExposeHint::WithEntity);
        assert_eq!(callable.exposed_hint(), ExposeHint::WithEntity);
        callable.set_expose_hint(ExposeHint::All);
        assert_eq!(callable.exposed_hint(), ExposeHint::All);
        callable.set_expose_hint(ExposeHint::WithSpace);
        assert_eq!(callable.exposed_hint(), ExposeHint::WithSpace);
        callable.set_expose_hint(ExposeHint::LeastLoaded);
        assert_eq!(callable.exposed_hint(), ExposeHint::LeastLoaded);
        assert_eq!(
            CallableWatcher::new(ExposeHint::All).exposed_hint(),
            ExposeHint::All
        );
    }

    /// The pseudo paths match exactly (`isArgsPath`/`isExposePath`,
    /// `watcher.cpp:735-756`).
    #[test]
    fn the_callable_pseudo_paths_match_exactly() {
        assert!(CallableWatcher::is_args_path("__args__"));
        assert!(!CallableWatcher::is_args_path(""));
        assert!(!CallableWatcher::is_args_path("args"));
        assert!(!CallableWatcher::is_args_path("__expose__"));
        assert!(CallableWatcher::is_expose_path("__expose__"));
        assert!(!CallableWatcher::is_expose_path(""));
        assert!(!CallableWatcher::is_expose_path("__args__"));
    }

    /// A function-backed callable returns its output and value through the
    /// `onCall` contract and serves the same string protocol in the tree.
    #[test]
    fn a_no_arg_func_callable_returns_its_output_and_value() {
        let mut callable = NoArgFuncCallableWatcher::new(
            Box::new(|output: &mut String, value: &mut String| {
                output.push_str("printed\n");
                value.push_str("42");
                true
            }),
            ExposeHint::LocalOnly,
        );
        assert_eq!(
            callable.on_call(),
            Some(("printed\n".to_string(), "42".to_string()))
        );
        // A failing call is the shape `setFromStream` reports as failure.
        let mut failing = NoArgFuncCallableWatcher::new(
            Box::new(|_: &mut String, _: &mut String| false),
            ExposeHint::LocalOnly,
        );
        assert_eq!(failing.on_call(), None);
        // Registered into the tree it answers the marker through the root
        // entry and refuses the protocol v1 write.
        fini();
        let registered: Rc<RefCell<dyn Watcher>> = Rc::new(RefCell::new(callable));
        registered
            .borrow_mut()
            .set_comment("run the thing".to_string());
        assert_eq!(registered.borrow().comment(), "run the thing");
        assert!(add_watcher("tools/do_thing", registered).is_some());
        let mut desc = String::new();
        let mut mode = Mode::Invalid;
        assert_eq!(
            get_watcher_string("tools/do_thing", &mut desc, &mut mode),
            Some("Callable function".to_string())
        );
        assert_eq!(desc, "run the thing");
        assert_eq!(mode, Mode::Callable);
        assert!(!set_watcher_string("tools/do_thing", "1"));
    }
}
