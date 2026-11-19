//! BW-01-03 DataSection: the XML section tree, a port of
//! `resmgr/xml_section.cpp` observable semantics.
//!
//! Model notes:
//! - Attributes are children too: a node's children are `[attributes in
//!   document order] + [element children in document order]`, so
//!   `find_child` can hit an attribute node and `count_children` counts
//!   attributes.
//! - Text values are trimmed at parse time (leading whitespace is skipped,
//!   trailing whitespace is cut); CDATA content is taken raw.
//! - Every reference parse error (mismatched tags, double text values,
//!   unclosed comments, …) fails the whole document — the caller then
//!   falls back to a binary section. Attribute syntax errors only stop
//!   attribute collection for that tag.
//! - The read-side `as_*` conversions port libc prefix semantics
//!   (`atoi`/`strtoul`/`strtod`) and iostream extraction semantics
//!   (`as_int64`, `as_vector2/4` with the strict eof check), because the
//!   reference mixes both freely.

use foundation::math::{Vector2, Vector3, Vector4};

use crate::special_chars;

/// Why an XML document failed to parse. The reference prints an
/// `ERROR_MSG` and returns NULL; the caller falls back to a binary
/// section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XmlParseError {
    pub message: String,
}

impl XmlParseError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for XmlParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "xml parse error: {}", self.message)
    }
}

impl std::error::Error for XmlParseError {}

/// One section of the tree: a named node with a string value, an
/// attribute marker and ordered children.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DataSection {
    name: String,
    value: String,
    /// Mirrors `cval_ != NULL`: only parse-time text/CDATA assignments set
    /// it, and a second assignment is a parse error.
    value_set: bool,
    is_attribute: bool,
    children: Vec<DataSection>,
}

/// Reference `XMLSection::isValidXMLTag`: rejects empty names, a leading
/// digit or byte >= 0x80, and any space, backslash or slash.
pub fn is_valid_xml_tag(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.is_empty() {
        return false;
    }
    // `str[0] < 0` is the signed-char view: bytes >= 0x80.
    if bytes[0] >= 0x80 || bytes[0].is_ascii_digit() {
        return false;
    }
    bytes
        .iter()
        .all(|&b| b != b' ' && b != b'\\' && b != b'/')
}

fn is_ws(b: u8) -> bool {
    b == b' ' || b == b'\t' || b == b'\r' || b == b'\n'
}

/// Parse a whole XML document into a tree rooted under `root_tag`
/// (`XMLSection::createFromBinary` + `createFromFile`'s "root" rename).
pub fn parse(root_tag: &str, raw: &[u8]) -> Result<DataSection, XmlParseError> {
    let mut p = Parser {
        data: raw,
        pos: 0,
        open: Vec::new(),
        root: None,
    };
    p.run()?;
    let mut root = p.root.expect("successful parse leaves a root node");
    if !root_tag.is_empty() {
        root.name = root_tag.to_string();
    }
    Ok(root)
}

struct Parser<'a> {
    data: &'a [u8],
    pos: usize,
    /// Stack of currently open (non-self-closed) elements; the top is the
    /// reference's `pCurrNode`.
    open: Vec<DataSection>,
    root: Option<DataSection>,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> u8 {
        self.data.get(self.pos).copied().unwrap_or(0)
    }

    fn alive(&self) -> bool {
        self.pos < self.data.len()
    }

    fn run(&mut self) -> Result<(), XmlParseError> {
        // C loop condition: continue while a node is open or no root yet.
        while !self.open.is_empty() || self.root.is_none() {
            if is_ws(self.peek()) {
                while is_ws(self.peek()) {
                    self.pos += 1;
                }
            }

            if self.peek() == b'<' {
                self.pos += 1;
                match self.peek() {
                    // `<!`: comment, CDATA or a consumed 2-char stub.
                    b'!' => {
                        self.pos += 1;
                        self.process_bang()?;
                    }
                    // `<?…?>`: skipped verbatim.
                    b'?' => {
                        self.pos += 1;
                        self.process_question_mark()?;
                    }
                    _ => self.process_tag()?,
                }
            } else if !self.alive() {
                let name = self.open.last().map_or("NULL", |n| n.name.as_str());
                return Err(XmlParseError::new(format!(
                    "unterminated tag {name}"
                )));
            } else if self.open.is_empty() {
                return Err(XmlParseError::new("file must start with a tag"));
            } else {
                self.read_value()?;
            }
        }
        Ok(())
    }

    /// `<!` bodies: `<!-- … -->` (with nesting count), `<![CDATA[…]]>`
    /// (raw value on the open node) or any other `<!XY…` stub, of which
    /// exactly the type character is consumed.
    fn process_bang(&mut self) -> Result<(), XmlParseError> {
        let type_char = self.next_or_err()?;
        match type_char {
            b'-' => {
                if self.next_or_err()? != b'-' {
                    return Err(XmlParseError::new(
                        "bad comment: expecting \"--\" following \"<!\"",
                    ));
                }
                self.skip_comment()
            }
            b'[' => {
                for expected in b"CDATA[" {
                    if self.next_or_err()? != *expected {
                        return Err(XmlParseError::new(
                            "bad CDATA: expecting \"CDATA[\" following \"<[\"",
                        ));
                    }
                }
                self.read_cdata()
            }
            // e.g. DOCTYPE: the reference consumes "!"+1 char and lets the
            // rest flow through the main loop.
            _ => Ok(()),
        }
    }

    fn next_or_err(&mut self) -> Result<u8, XmlParseError> {
        if !self.alive() {
            return Err(XmlParseError::new("unexpected end of document"));
        }
        let b = self.data[self.pos];
        self.pos += 1;
        Ok(b)
    }

    /// Sliding-window comment skip with the reference's nesting count: a
    /// `-->` decrements, a `<!--` increments.
    fn skip_comment(&mut self) -> Result<(), XmlParseError> {
        let mut count = 1;
        let mut prev3;
        let mut prev2 = 0u8;
        let mut prev1 = self.next_or_err()?;
        let mut curr = self.next_or_err()?;

        while self.alive() {
            prev3 = prev2;
            prev2 = prev1;
            prev1 = curr;
            curr = self.next_or_err()?;

            if prev2 == b'-' && prev1 == b'-' && curr == b'>' {
                count -= 1;
                if count == 0 {
                    return Ok(());
                }
            } else if prev3 == b'<' && prev2 == b'!' && prev1 == b'-' && curr == b'-'
            {
                count += 1;
            }
        }
        Err(XmlParseError::new("comment not closed"))
    }

    /// CDATA content is raw: no whitespace trim, no escape reduction,
    /// truncated at the first NUL (the reference observes it via strlen).
    fn read_cdata(&mut self) -> Result<(), XmlParseError> {
        let start = self.pos;
        while self.alive() {
            if self.data[self.pos] == b']' {
                let content_end = self.pos;
                let rest = &self.data[self.pos..];
                if rest.len() >= 3 && rest[1] == b']' && rest[2] == b'>' {
                    self.pos += 3;
                    let node = self
                        .open
                        .last_mut()
                        .ok_or_else(|| XmlParseError::new("CDATA outside section"))?;
                    if node.value_set {
                        return Err(XmlParseError::new(
                            "trying to set the value twice (CDATA)",
                        ));
                    }
                    let mut content = &self.data[start..content_end];
                    if let Some(nul) = content.iter().position(|&b| b == 0) {
                        content = &content[..nul];
                    }
                    node.value = String::from_utf8_lossy(content).into_owned();
                    node.value_set = true;
                    return Ok(());
                }
            }
            self.pos += 1;
        }
        Err(XmlParseError::new("CDATA section not closed"))
    }

    /// `<?…?>` skip; EOF before `?>` is an error.
    fn process_question_mark(&mut self) -> Result<(), XmlParseError> {
        let mut prev1 = self.next_or_err()?;
        let mut curr = self.next_or_err()?;
        while self.alive() && !(prev1 == b'?' && curr == b'>') {
            prev1 = curr;
            curr = self.next_or_err()?;
        }
        if !self.alive() && !(prev1 == b'?' && curr == b'>') {
            return Err(XmlParseError::new("\"<?\" section not closed"));
        }
        Ok(())
    }

    fn process_tag(&mut self) -> Result<(), XmlParseError> {
        let is_end_tag = self.peek() == b'/';
        if is_end_tag {
            self.pos += 1;
        }

        // Tag text runs to the next '>'.
        let Some(gt) = self.data[self.pos..].iter().position(|&b| b == b'>') else {
            return Err(XmlParseError::new("unclosed tag"));
        };
        let mut tag_text = &self.data[self.pos..self.pos + gt];
        self.pos += gt + 1;
        // Trailing whitespace of the tag text is trimmed.
        while let Some(&last) = tag_text.last() {
            if is_ws(last) {
                tag_text = &tag_text[..tag_text.len() - 1];
            } else {
                break;
            }
        }

        if self.open.is_empty() && is_end_tag {
            return Err(XmlParseError::new(
                "cannot have an end tag at start of file",
            ));
        }

        if is_end_tag {
            let end_name = String::from_utf8_lossy(tag_text).into_owned();
            // The reference has a whitespace-prefix recovery branch, but
            // node names can never contain whitespace (name extraction
            // stops at whitespace), so any difference here is fatal there
            // too.
            let node = self.open.pop().expect("checked above");
            if node.name != end_name {
                return Err(XmlParseError::new(format!(
                    "tags do not match: start {:?} end {:?}",
                    node.name, end_name
                )));
            }
            match self.open.last_mut() {
                Some(parent) => parent.children.push(node),
                None => self.root = Some(node),
            }
            return Ok(());
        }

        // Start tag: extract the name, detect the `<tag/>` shorthand.
        let is_empty_tag = tag_text.last() == Some(&b'/');
        let mut name_end = 0usize;
        while name_end < tag_text.len() {
            let b = tag_text[name_end];
            if b == 0 || b == b'/' || is_ws(b) {
                break;
            }
            name_end += 1;
        }
        let name = String::from_utf8_lossy(&tag_text[..name_end]).into_owned();
        let mut node = DataSection {
            name,
            ..DataSection::default()
        };

        // Attributes become the node's first children, in document order.
        self.parse_attributes(&mut node, &tag_text[name_end..], is_empty_tag)?;

        if is_empty_tag {
            // Empty tags never descend, so they attach immediately.
            match self.open.last_mut() {
                Some(parent) => parent.children.push(node),
                // A self-closed root completes the document at once; the
                // main loop then exits and ignores any trailing content,
                // exactly like the reference.
                None => self.root = Some(node),
            }
        } else {
            // Non-empty tags descend; children attach when the end tag
            // closes (first tag becomes the pending root).
            self.open.push(node);
        }
        Ok(())
    }

    /// Attribute collection. Syntax problems print an error in the
    /// reference and *break out of the attribute loop* — the tag itself
    /// still parses.
    fn parse_attributes(
        &mut self,
        node: &mut DataSection,
        rest: &[u8],
        _is_empty_tag: bool,
    ) -> Result<(), XmlParseError> {
        let mut p = 0usize;
        let end = rest.len();
        loop {
            // Skip whitespace between attributes.
            while p < end && is_ws(rest[p]) {
                p += 1;
            }

            // Attribute name: up to whitespace, '=' or NUL.
            let name_start = p;
            while p < end && rest[p] != 0 && rest[p] != b'=' && !is_ws(rest[p]) {
                p += 1;
            }
            // End of tag text (or a NUL): stop collecting attributes.
            if p >= end || rest[p] == 0 {
                break;
            }

            let next_char = rest[p];
            let name = String::from_utf8_lossy(&rest[name_start..p]).into_owned();
            p += 1;

            // The '=' may follow whitespace.
            if next_char != b'=' {
                while p < end && is_ws(rest[p]) {
                    p += 1;
                }
                if p >= end || rest[p] != b'=' {
                    // Reference: error message, then break out of the loop.
                    break;
                }
                p += 1;
            }

            while p < end && is_ws(rest[p]) {
                p += 1;
            }

            let Some(&quote) = rest.get(p) else {
                break;
            };
            if quote != b'"' && quote != b'\'' {
                // Reference: "missing quotes", break.
                break;
            }
            p += 1;

            let value_start = p;
            while p < end && rest[p] != 0 && rest[p] != quote {
                p += 1;
            }
            if p >= end || rest[p] != quote {
                // Reference: "unable to find end of attribute", break.
                break;
            }
            let value = special_chars::reduce(&String::from_utf8_lossy(
                &rest[value_start..p],
            ));
            p += 1;

            node.children.push(DataSection {
                name,
                value,
                value_set: true,
                is_attribute: true,
                children: Vec::new(),
            });
        }
        Ok(())
    }

    /// Text content between tags: leading whitespace was already skipped
    /// at loop top, trailing whitespace is trimmed, NUL truncates (strlen
    /// view), escapes are reduced. A second value assignment is fatal.
    fn read_value(&mut self) -> Result<(), XmlParseError> {
        let start = self.pos;
        let mut value_end = start;
        while self.alive() && self.peek() != b'<' {
            let b = self.data[self.pos];
            self.pos += 1;
            if !is_ws(b) {
                value_end = self.pos;
            }
        }
        if !self.alive() {
            let name = self.open.last().map_or("NULL", |n| n.name.as_str());
            return Err(XmlParseError::new(format!("unterminated tag {name}")));
        }

        let node = self.open.last_mut().expect("value read needs an open node");
        if node.value_set {
            return Err(XmlParseError::new(format!(
                "trying to set the value twice on {:?}",
                node.name
            )));
        }
        let mut span = &self.data[start..value_end];
        if let Some(nul) = span.iter().position(|&b| b == 0) {
            span = &span[..nul];
        }
        node.value = special_chars::reduce(&String::from_utf8_lossy(span));
        node.value_set = true;
        Ok(())
    }
}

impl DataSection {
    /// A standalone section with the given tag (`new XMLSection(tag)`).
    pub fn new(tag: &str) -> Self {
        Self {
            name: tag.to_string(),
            ..Self::default()
        }
    }

    pub fn section_name(&self) -> &str {
        &self.name
    }

    /// Children include attribute nodes, in document order.
    pub fn count_children(&self) -> usize {
        self.children.len()
    }

    pub fn open_child(&self, index: usize) -> Option<&DataSection> {
        self.children.get(index)
    }

    /// First child (attribute or element) with this name
    /// (`XMLSection::findChild`).
    pub fn find_child(&self, tag: &str) -> Option<&DataSection> {
        self.children.iter().find(|c| c.name == tag)
    }

    /// Append a new empty child section (`XMLSection::newSection`).
    pub fn add_child(&mut self, tag: &str) -> &mut DataSection {
        self.children.push(Self::new(tag));
        self.children.last_mut().expect("just pushed")
    }

    pub fn child_mut(&mut self, index: usize) -> Option<&mut DataSection> {
        self.children.get_mut(index)
    }

    pub fn is_attribute(&self) -> bool {
        self.is_attribute
    }

    /// Raw string value. The reference's `defaultVal` and flags parameters
    /// are ignored by `XMLSection::asString` — this always returns the
    /// value, empty or not.
    pub fn as_string(&self) -> &str {
        &self.value
    }

    /// Case-insensitive `true`/`false`; everything else (including `1`)
    /// yields the default.
    pub fn as_bool(&self, default: bool) -> bool {
        let v = self.value.as_bytes();
        if v.eq_ignore_ascii_case(b"true") {
            return true;
        }
        if v.eq_ignore_ascii_case(b"false") {
            return false;
        }
        default
    }

    /// `atoi` + `checkConversion`: prefix parse through i64 (ERANGE →
    /// default), i32 truncation, then a zero-result must come from the
    /// exact string "0".
    pub fn as_int(&self, default: i32) -> i32 {
        let bytes = self.value.as_bytes();
        if bytes.is_empty() {
            return default;
        }
        let (raw, erange) = strtol(bytes);
        if erange {
            return default;
        }
        let result = raw as i32;
        if result == 0 && self.value != "0" {
            return default;
        }
        result
    }

    /// `strtoul` + `checkConversion`: a negative sign wraps modulo 2^64
    /// before the u32 truncation; u64 overflow is ERANGE → default; a
    /// truncated result of 0 (e.g. "4294967296") is not "0" → default.
    pub fn as_uint(&self, default: u32) -> u32 {
        let bytes = self.value.as_bytes();
        if bytes.is_empty() {
            return default;
        }
        let (raw, erange) = strtoul(bytes);
        if erange {
            return default;
        }
        let result = raw as u32;
        if result == 0 && self.value != "0" {
            return default;
        }
        result
    }

    /// Exact-case `True`/`False` map to 1/0 before the `atol` parse.
    pub fn as_long(&self, default: i64) -> i64 {
        if self.value == "True" {
            return 1;
        }
        if self.value == "False" {
            return 0;
        }
        let bytes = self.value.as_bytes();
        if bytes.is_empty() {
            return default;
        }
        let (raw, erange) = strtol(bytes);
        if erange {
            return default;
        }
        if raw == 0 && self.value != "0" {
            return default;
        }
        raw
    }

    /// iostream extraction: leading whitespace skipped, optional sign,
    /// digits required (else fail → default), trailing junk ignored, no
    /// eof check; overflow fails.
    pub fn as_int64(&self, default: i64) -> i64 {
        let mut p = 0usize;
        let bytes = self.value.as_bytes();
        while p < bytes.len() && matches!(bytes[p], b' ' | b'\t' | b'\n' | b'\r' | 0x0B | 0x0C)
        {
            p += 1;
        }
        let mut negative = false;
        if p < bytes.len() && (bytes[p] == b'+' || bytes[p] == b'-') {
            negative = bytes[p] == b'-';
            p += 1;
        }
        let mut any = false;
        let mut acc: i64 = 0;
        let mut overflow = false;
        while p < bytes.len() && bytes[p].is_ascii_digit() {
            any = true;
            let digit = (bytes[p] - b'0') as i64;
            acc = match acc.checked_mul(10).and_then(|v| v.checked_add(digit)) {
                Some(v) => v,
                None => {
                    overflow = true;
                    break;
                }
            };
            p += 1;
        }
        if !any || overflow {
            return default;
        }
        if negative {
            -acc
        } else {
            acc
        }
    }

    /// `sscanf("%llu")`: whitespace and sign allowed, digits required
    /// (else default), overflow saturates at u64::MAX with ERANGE (the
    /// match still counts in glibc), trailing junk ignored. A minus sign
    /// wraps modulo 2^64 (`"-5"` → `u64::MAX - 4`, probed against glibc).
    pub fn as_uint64(&self, default: u64) -> u64 {
        let bytes = self.value.as_bytes();
        if bytes.is_empty() {
            return default;
        }
        let mut p = 0usize;
        while p < bytes.len() && matches!(bytes[p], b' ' | b'\t' | b'\n' | b'\r' | 0x0B | 0x0C)
        {
            p += 1;
        }
        let mut negative = false;
        if p < bytes.len() && (bytes[p] == b'+' || bytes[p] == b'-') {
            negative = bytes[p] == b'-';
            p += 1;
        }
        let mut any = false;
        let mut acc: u64 = 0;
        let mut overflow = false;
        while p < bytes.len() && bytes[p].is_ascii_digit() {
            any = true;
            acc = match acc.checked_mul(10).and_then(|v| v.checked_add((bytes[p] - b'0') as u64))
            {
                Some(v) => v,
                None => {
                    overflow = true;
                    break;
                }
            };
            p += 1;
        }
        if !any {
            return default;
        }
        if overflow {
            return u64::MAX;
        }
        if negative {
            acc.wrapping_neg()
        } else {
            acc
        }
    }

    /// `strtod` prefix semantics: any successfully parsed prefix counts
    /// (`"12abc"` → 12), only a full failure yields the default. Hex
    /// float literals are not supported (not produced by reference
    /// configs).
    pub fn as_double(&self, default: f64) -> f64 {
        let bytes = self.value.as_bytes();
        match strtod_from(bytes, 0).map(|(value, _)| value) {
            Some(v) => v,
            None => default,
        }
    }

    pub fn as_float(&self, default: f32) -> f32 {
        self.as_double(default as f64) as f32
    }

    /// `XMLSection::asBlob`: the value is assumed to be base64 and decoded
    /// with the lenient buffer variant. The reference's `defaultVal`
    /// parameter is ignored.
    pub fn as_blob(&self) -> Vec<u8> {
        crate::base64::decode_lenient(&self.value)
    }

    /// iostream extraction of exactly two floats plus the eof check:
    /// `"1 2 "` (trailing space) and `"1 2 3"` both fail to the default.
    pub fn as_vector2(&self, default: Vector2) -> Vector2 {
        let bytes = self.value.as_bytes();
        let mut p = 0usize;
        let Some((x, np)) = stream_float(bytes, p) else {
            return default;
        };
        p = np;
        let Some((y, np)) = stream_float(bytes, p) else {
            return default;
        };
        p = np;
        if p != bytes.len() {
            return default;
        }
        Vector2::new(x as f32, y as f32)
    }

    /// Three `strtod` prefix parses: trailing junk is fine, but all three
    /// numbers must be present.
    pub fn as_vector3(&self, default: Vector3) -> Vector3 {
        let bytes = self.value.as_bytes();
        let mut p = 0usize;
        let mut comps = [0.0f64; 3];
        for comp in &mut comps {
            let Some((v, np)) = strtod_from(bytes, p) else {
                return default;
            };
            *comp = v;
            p = np;
        }
        Vector3::new(comps[0] as f32, comps[1] as f32, comps[2] as f32)
    }

    /// Like `as_vector2`, but for four floats.
    pub fn as_vector4(&self, default: Vector4) -> Vector4 {
        let bytes = self.value.as_bytes();
        let mut p = 0usize;
        let mut comps = [0.0f64; 4];
        for comp in &mut comps {
            let Some((v, np)) = stream_float(bytes, p) else {
                return default;
            };
            *comp = v;
            p = np;
        }
        if p != bytes.len() {
            return default;
        }
        Vector4::new(
            comps[0] as f32,
            comps[1] as f32,
            comps[2] as f32,
            comps[3] as f32,
        )
    }

    /// `DataSection::readVector3`: find the child, convert or default.
    pub fn read_vector3(&self, tag: &str, default: Vector3) -> Vector3 {
        match self.find_child(tag) {
            Some(child) => child.as_vector3(default),
            None => default,
        }
    }

    // ------------------------------------------------------------------
    // Writers (setX / writeToStream formats, pinned from xml_section.cpp)
    // ------------------------------------------------------------------

    pub fn set_string(&mut self, value: &str) {
        self.value = value.to_string();
        self.value_set = true;
    }

    pub fn set_bool(&mut self, value: bool) {
        self.set_string(if value { "true" } else { "false" });
    }

    pub fn set_int(&mut self, value: i32) {
        self.set_string(&value.to_string());
    }

    pub fn set_uint(&mut self, value: u32) {
        self.set_string(&value.to_string());
    }

    pub fn set_long(&mut self, value: i64) {
        self.set_string(&value.to_string());
    }

    pub fn set_int64(&mut self, value: i64) {
        self.set_string(&value.to_string());
    }

    pub fn set_uint64(&mut self, value: u64) {
        self.set_string(&value.to_string());
    }

    /// `setprecision(6) fixed`.
    pub fn set_float(&mut self, value: f32) {
        self.set_string(&format!("{:.6}", value));
    }

    /// `setprecision(17) fixed`.
    pub fn set_double(&mut self, value: f64) {
        self.set_string(&format!("{:.17}", value));
    }

    pub fn set_vector2(&mut self, v: Vector2) {
        self.set_string(&format!("{:.6} {:.6}", v.x, v.y));
    }

    pub fn set_vector3(&mut self, v: Vector3) {
        self.set_string(&format!("{:.6} {:.6} {:.6}", v.x, v.y, v.z));
    }

    pub fn set_vector4(&mut self, v: Vector4) {
        self.set_string(&format!(
            "{:.6} {:.6} {:.6} {:.6}",
            v.x, v.y, v.z, v.w
        ));
    }

    /// Serialise as the reference `writeToStream` does from level 0:
    /// attribute children on their own indented lines (duplicates demoted
    /// to normal children), `" />"` for empty sections, the value after a
    /// tab on the opening line, one tab before the close tag when there
    /// are no children. An invalid root name becomes "root".
    pub fn write_to_string(&mut self) -> String {
        let mut out = String::new();
        self.write_stream(&mut out, 0);
        out
    }

    fn write_stream(&mut self, out: &mut String, level: usize) {
        for _ in 0..level {
            out.push('\t');
        }
        let tag = if level == 0 && !is_valid_xml_tag(&self.name) {
            "root".to_string()
        } else {
            self.name.clone()
        };
        out.push('<');
        out.push_str(&tag);

        // Attributes first, one per line; a duplicate name is demoted to
        // a normal child (the reference prints an error and does the same).
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut has_normal_children = false;
        for child in &mut self.children {
            if child.is_attribute {
                if !seen.insert(child.name.clone()) {
                    child.is_attribute = false;
                    has_normal_children = true;
                } else {
                    out.push('\n');
                    for _ in 0..level + 2 {
                        out.push('\t');
                    }
                    out.push_str(&child.name);
                    out.push_str("=\"");
                    out.push_str(&special_chars::expand(&child.value));
                    out.push('"');
                }
            } else {
                has_normal_children = true;
            }
        }

        if !has_normal_children && self.value.is_empty() {
            out.push_str(" />\n");
            return;
        }

        out.push('>');
        if !self.value.is_empty() {
            out.push('\t');
            out.push_str(&special_chars::expand(&self.value));
        }

        if !self.children.is_empty() {
            out.push('\n');
            for child in self.children.iter_mut() {
                if !child.is_attribute {
                    child.write_stream(out, level + 1);
                }
            }
            for _ in 0..level {
                out.push('\t');
            }
        } else {
            out.push('\t');
        }
        out.push_str("</");
        out.push_str(&tag);
        out.push_str(">\n");
    }
}

/// `strtol` (base 10) → (value, erange). Saturates at i64 bounds on
/// overflow, like glibc.
fn strtol(bytes: &[u8]) -> (i64, bool) {
    let mut p = skip_c_whitespace(bytes);
    let mut negative = false;
    if p < bytes.len() && (bytes[p] == b'+' || bytes[p] == b'-') {
        negative = bytes[p] == b'-';
        p += 1;
    }
    let mut any = false;
    let mut acc: u64 = 0;
    let mut overflow = false;
    while p < bytes.len() && bytes[p].is_ascii_digit() {
        any = true;
        acc = match acc
            .checked_mul(10)
            .and_then(|v| v.checked_add((bytes[p] - b'0') as u64))
        {
            Some(v) => v,
            None => {
                overflow = true;
                u64::MAX
            }
        };
        p += 1;
    }
    if !any {
        return (0, false);
    }
    if overflow {
        let saturated = if negative { i64::MIN } else { i64::MAX };
        return (saturated, true);
    }
    // -i64::MAX edge: u64 accumulation cannot represent i64::MIN magnitude
    // exactly, but only overflows there; glibc wraps, values that large
    // are out of scope for config parsing.
    let value = if negative {
        (acc as i64).wrapping_neg()
    } else {
        acc as i64
    };
    (value, false)
}

/// `strtoul` (base 10) → (value, erange). A minus sign negates modulo
/// 2^64 exactly like glibc.
fn strtoul(bytes: &[u8]) -> (u64, bool) {
    let mut p = skip_c_whitespace(bytes);
    let mut negative = false;
    if p < bytes.len() && (bytes[p] == b'+' || bytes[p] == b'-') {
        negative = bytes[p] == b'-';
        p += 1;
    }
    let mut any = false;
    let mut acc: u64 = 0;
    let mut overflow = false;
    while p < bytes.len() && bytes[p].is_ascii_digit() {
        any = true;
        acc = match acc
            .checked_mul(10)
            .and_then(|v| v.checked_add((bytes[p] - b'0') as u64))
        {
            Some(v) => v,
            None => {
                overflow = true;
                u64::MAX
            }
        };
        p += 1;
    }
    if !any {
        return (0, false);
    }
    if overflow {
        return (u64::MAX, true);
    }
    let value = if negative {
        acc.wrapping_neg()
    } else {
        acc
    };
    (value, false)
}

fn skip_c_whitespace(bytes: &[u8]) -> usize {
    let mut p = 0usize;
    while p < bytes.len() && matches!(bytes[p], b' ' | b'\t' | b'\n' | b'\r' | 0x0B | 0x0C) {
        p += 1;
    }
    p
}

/// `strtod` prefix parse from `p`, returning (value, new position).
/// Handles decimal notation with optional exponent plus
/// inf/infinity/nan. Returns None when no prefix parses.
fn strtod_from(bytes: &[u8], from: usize) -> Option<(f64, usize)> {
    let mut p = from;
    while p < bytes.len() && matches!(bytes[p], b' ' | b'\t' | b'\n' | b'\r' | 0x0B | 0x0C) {
        p += 1;
    }
    let mut q = p;
    let mut negative = false;
    if q < bytes.len() && (bytes[q] == b'+' || bytes[q] == b'-') {
        negative = bytes[q] == b'-';
        q += 1;
    }

    // inf / infinity / nan (case-insensitive).
    let rest = &bytes[q..];
    let lower: Vec<u8> = rest.iter().map(|b| b.to_ascii_lowercase()).collect();
    if lower.starts_with(b"infinity") {
        let v = if negative { f64::NEG_INFINITY } else { f64::INFINITY };
        return Some((v, q + 8));
    }
    if lower.starts_with(b"inf") {
        let v = if negative { f64::NEG_INFINITY } else { f64::INFINITY };
        return Some((v, q + 3));
    }
    if lower.starts_with(b"nan") {
        return Some((f64::NAN, q + 3));
    }

    let int_digits_start = q;
    while q < bytes.len() && bytes[q].is_ascii_digit() {
        q += 1;
    }
    let int_digits = q - int_digits_start;
    let mut frac_digits = 0usize;
    if q < bytes.len() && bytes[q] == b'.' {
        let frac_start = q + 1;
        let mut r = frac_start;
        while r < bytes.len() && bytes[r].is_ascii_digit() {
            r += 1;
        }
        frac_digits = r - frac_start;
        if int_digits > 0 || frac_digits > 0 {
            q = r;
        }
    }
    if int_digits == 0 && frac_digits == 0 {
        return None;
    }

    let mantissa_end = q;
    // Optional exponent; only consumed when digits follow.
    let mut end = mantissa_end;
    if q < bytes.len() && (bytes[q] == b'e' || bytes[q] == b'E') {
        let mut r = q + 1;
        if r < bytes.len() && (bytes[r] == b'+' || bytes[r] == b'-') {
            r += 1;
        }
        let exp_start = r;
        while r < bytes.len() && bytes[r].is_ascii_digit() {
            r += 1;
        }
        if r > exp_start {
            end = r;
        }
    }

    let text = std::str::from_utf8(&bytes[p..end]).expect("ASCII digits only");
    let value: f64 = text.parse().expect("valid decimal literal");
    Some((value, end))
}

/// iostream float extraction: like `strtod_from` but the whole remainder
/// after the number must be exactly the end (the caller checks), and the
/// value must fit in f32 (out-of-range sets failbit).
pub(crate) fn stream_float(bytes: &[u8], from: usize) -> Option<(f64, usize)> {
    let (v, np) = strtod_from(bytes, from)?;
    // Literal infinities pass; numeric overflow to infinity fails
    // extraction in C++.
    if v.is_infinite() {
        let p = skip_c_whitespace(bytes);
        let mut q = p;
        if q < bytes.len() && (bytes[q] == b'+' || bytes[q] == b'-') {
            q += 1;
        }
        let lower: Vec<u8> = bytes[q..].iter().map(|b| b.to_ascii_lowercase()).collect();
        let is_literal = lower.starts_with(b"inf") || lower.starts_with(b"nan");
        if !is_literal {
            return None;
        }
    }
    // Numeric overflow sets failbit; a literal inf is a valid value.
    if v.is_finite() && v.abs() > f32::MAX as f64 {
        return None;
    }
    Some((v, np))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_doc(xml: &str) -> DataSection {
        parse("root", xml.as_bytes()).expect("test document parses")
    }

    fn parse_err(xml: &str) -> XmlParseError {
        parse("root", xml.as_bytes()).expect_err("test document must fail")
    }

    // ------------------------------------------------------------------
    // Structure: duplicate children and empty-value queries (the BW-01-03-01
    // acceptance points).
    // ------------------------------------------------------------------

    #[test]
    fn duplicate_children_keep_order_and_find_child_takes_the_first() {
        let doc = parse_doc("<a><b>1</b><b>2</b><c>3</c></a>");
        assert_eq!(doc.section_name(), "root");
        assert_eq!(doc.count_children(), 3);
        let first = doc.find_child("b").expect("b exists");
        assert_eq!(first.as_string(), "1", "findChild returns the FIRST match");
        assert_eq!(doc.open_child(0).unwrap().as_string(), "1");
        assert_eq!(doc.open_child(1).unwrap().as_string(), "2");
        assert_eq!(doc.open_child(2).unwrap().section_name(), "c");
        assert!(doc.open_child(3).is_none());
    }

    #[test]
    fn empty_values_read_as_the_empty_string() {
        // `<a/>`, `<a></a>` and whitespace-only all yield "" and no
        // children — asString's default argument is ignored by the
        // reference, so an empty value is observable as "". The single
        // top-level tag IS the (renamed) root section.
        for xml in ["<a/>", "<a></a>", "<a>   </a>", "<a>\t\n</a>"] {
            let doc = parse_doc(xml);
            assert_eq!(doc.as_string(), "", "input: {xml}");
            assert_eq!(doc.count_children(), 0, "input: {xml}");
            assert_eq!(doc.section_name(), "root");
        }
    }

    #[test]
    fn attributes_are_children_before_elements() {
        let doc = parse_doc("<holder x=\"1\" y=\"2\"><x>3</x></holder>");
        let holder = &doc;
        assert_eq!(holder.count_children(), 3, "2 attributes + 1 element");
        // Document order: attributes first, then elements.
        assert_eq!(holder.open_child(0).unwrap().section_name(), "x");
        assert!(holder.open_child(0).unwrap().is_attribute());
        assert_eq!(holder.open_child(0).unwrap().as_string(), "1");
        assert_eq!(holder.open_child(1).unwrap().section_name(), "y");
        assert_eq!(holder.open_child(2).unwrap().as_string(), "3");
        assert!(!holder.open_child(2).unwrap().is_attribute());
        // findChild("x") hits the ATTRIBUTE (document order), not the
        // element — pinned reference behaviour.
        let found = holder.find_child("x").unwrap();
        assert!(found.is_attribute());
        assert_eq!(found.as_string(), "1");
    }

    #[test]
    fn self_closed_tag_with_attribute() {
        let doc = parse_doc("<root><a x=\"1\"/></root>");
        let a = doc.find_child("a").unwrap();
        assert_eq!(a.as_string(), "");
        assert_eq!(a.count_children(), 1);
        assert_eq!(a.find_child("x").unwrap().as_string(), "1");
    }

    #[test]
    fn text_is_trimmed_at_parse_time() {
        let doc = parse_doc("<a>  hello world  </a>");
        assert_eq!(doc.as_string(), "hello world", "trimmed value");

        // Leading whitespace of a value is skipped, trailing is trimmed
        // even when an empty child follows.
        let doc = parse_doc("<a>  x  <b/></a>");
        assert_eq!(doc.as_string(), "x");
        assert_eq!(doc.count_children(), 1);
    }

    #[test]
    fn root_tag_is_renamed() {
        let doc = parse_doc("<config><v>1</v></config>");
        assert_eq!(doc.section_name(), "root", "createFromBinary renames");
        assert_eq!(doc.count_children(), 1);
    }

    #[test]
    fn empty_root_tag_argument_keeps_original_name() {
        let doc = parse("", b"<config/>".as_slice()).unwrap();
        assert_eq!(doc.section_name(), "config");
    }

    // ------------------------------------------------------------------
    // Escapes and CDATA
    // ------------------------------------------------------------------

    #[test]
    fn escapes_reduce_in_values_and_attributes() {
        let doc = parse_doc("<a>&lt;&amp;&#65;&#x42;</a>");
        assert_eq!(doc.as_string(), "<&AB");

        let doc = parse_doc("<root><a q=\"&quot;q&quot;\">v</a></root>");
        let a = doc.find_child("a").unwrap();
        assert_eq!(a.find_child("q").unwrap().as_string(), "\"q\"");
    }

    #[test]
    fn unknown_escape_stays_literal() {
        // "&foo;" is an unknown name → literal. "&amp" without ';' still
        // reduces: the reference only checks the name and length, so an
        // end-of-string body matches.
        let doc = parse_doc("<a>&foo;&amp</a>");
        assert_eq!(doc.as_string(), "&foo;&");
    }

    #[test]
    fn cdata_is_raw_no_trim_no_reduce() {
        let doc = parse_doc("<a><![CDATA[  x < y & z >  ]]></a>");
        assert_eq!(doc.as_string(), "  x < y & z >  ");
    }

    #[test]
    fn comments_are_skipped_with_nesting() {
        let doc = parse_doc("<!-- outer <!-- inner --> still --><a>1</a>");
        assert_eq!(doc.as_string(), "1");
    }

    #[test]
    fn xml_declaration_is_skipped() {
        let doc = parse_doc("<?xml version=\"1.0\" encoding=\"utf-8\"?><a>1</a>");
        assert_eq!(doc.as_string(), "1");
    }

    // ------------------------------------------------------------------
    // Parse errors: the whole document fails
    // ------------------------------------------------------------------

    #[test]
    fn mixed_text_after_child_is_a_parse_error() {
        // `<a>1<b/>2</a>`: the second text hits the already-set value.
        let err = parse_err("<a>1<b/>2</a>");
        assert!(err.message.contains("value twice"), "{}", err.message);
    }

    #[test]
    fn text_then_cdata_is_a_parse_error() {
        let err = parse_err("<a>1<![CDATA[2]]></a>");
        assert!(err.message.contains("value twice"));
    }

    #[test]
    fn mismatched_end_tag_is_a_parse_error() {
        let err = parse_err("<a><b>x</c></a>");
        assert!(err.message.contains("do not match"));
    }

    #[test]
    fn end_tag_at_start_of_file_is_a_parse_error() {
        let err = parse_err("</a>");
        assert!(err.message.contains("end tag at start"));
    }

    #[test]
    fn unclosed_tag_is_a_parse_error() {
        assert!(parse_err("<a>x").message.contains("unterminated"));
        assert!(parse_err("<a").message.contains("unclosed"));
    }

    #[test]
    fn unterminated_comment_and_cdata_are_parse_errors() {
        assert!(parse_err("<!-- x").message.contains("comment"));
        assert!(parse_err("<a><![CDATA[x]]></a").message.contains("unclosed"));
    }

    #[test]
    fn empty_or_elementless_documents_fail() {
        assert!(!parse_err("").message.is_empty());
        assert!(!parse_err("<?xml?><!---->").message.is_empty());
    }

    #[test]
    fn content_before_first_tag_fails() {
        assert!(parse_err("hello<a/>").message.contains("start with a tag"));
    }

    #[test]
    fn trailing_content_after_root_is_ignored() {
        // The reference loop exits once the root closed; `<b/>` is never
        // processed.
        let doc = parse_doc("<a/><b/>");
        assert_eq!(doc.section_name(), "root");
        assert_eq!(doc.count_children(), 0);
    }

    #[test]
    fn attribute_syntax_errors_do_not_kill_the_tag() {
        // Missing quotes and a missing '=' break attribute collection but
        // the element still parses.
        let doc = parse_doc("<root><a x=1 y>hi</a></root>");
        let a = doc.find_child("a").unwrap();
        assert_eq!(a.as_string(), "hi");
        assert_eq!(a.count_children(), 0);
    }

    #[test]
    fn cdata_outside_a_section_fails() {
        let err = parse_err("<![CDATA[x]]><a/>");
        assert!(err.message.contains("outside"));
    }

    // ------------------------------------------------------------------
    // as_* conversions
    // ------------------------------------------------------------------

    #[test]
    fn as_bool_is_case_insensitive_true_false_only() {
        let doc = parse_doc("<r><a>true</a><b>FALSE</b><c>1</c><d>yes</d><e></e></r>");
        let r = &doc;
        assert!(r.find_child("a").unwrap().as_bool(false));
        assert!(!r.find_child("b").unwrap().as_bool(true));
        // "1" is not true/false → default.
        assert!(r.find_child("c").unwrap().as_bool(true));
        assert!(!r.find_child("c").unwrap().as_bool(false));
        assert!(r.find_child("d").unwrap().as_bool(true), "junk → default");
        assert!(r.find_child("e").unwrap().as_bool(true), "empty → default");
    }

    #[test]
    fn as_int_prefix_semantics() {
        let doc = parse_doc("<r><a>42</a><b>-7</b><c>42abc</c><d>abc</d><e></e><f>0</f><g>99999999999</g><h>99999999999999999999999</h></r>");
        let r = &doc;
        assert_eq!(r.find_child("a").unwrap().as_int(0), 42);
        assert_eq!(r.find_child("b").unwrap().as_int(0), -7);
        assert_eq!(r.find_child("c").unwrap().as_int(-1), 42, "prefix parse");
        assert_eq!(r.find_child("d").unwrap().as_int(-1), -1, "junk → default");
        assert_eq!(r.find_child("e").unwrap().as_int(-1), -1, "empty → default");
        assert_eq!(r.find_child("f").unwrap().as_int(-1), 0, "exact \"0\" is 0");
        // Overflows i64? No: 99999999999 fits i64 → (int) truncates.
        assert_eq!(
            r.find_child("g").unwrap().as_int(0),
            99999999999i64 as i32,
            "glibc (int)strtol truncates"
        );
        // Overflows i64 → ERANGE → default.
        assert_eq!(r.find_child("h").unwrap().as_int(-5), -5);
    }

    #[test]
    fn as_uint_wraps_and_range_checks() {
        let doc = parse_doc("<r><a>4294967295</a><b>4294967296</b><c>-1</c><d>abc</d><e></e><f>0</f></r>");
        let r = &doc;
        assert_eq!(r.find_child("a").unwrap().as_uint(0), u32::MAX);
        // Fits u64, truncates to 0, and 0 from a non-"0" string → default.
        assert_eq!(r.find_child("b").unwrap().as_uint(7), 7);
        // Negative wraps modulo 2^64, then truncates to u32::MAX.
        assert_eq!(r.find_child("c").unwrap().as_uint(0), u32::MAX);
        assert_eq!(r.find_child("d").unwrap().as_uint(9), 9);
        assert_eq!(r.find_child("e").unwrap().as_uint(9), 9);
        assert_eq!(r.find_child("f").unwrap().as_uint(9), 0);
    }

    #[test]
    fn as_long_maps_exact_case_true_false_first() {
        let doc = parse_doc("<r><a>True</a><b>False</b><c>true</c><d>-42</d></r>");
        let r = &doc;
        assert_eq!(r.find_child("a").unwrap().as_long(-1), 1);
        assert_eq!(r.find_child("b").unwrap().as_long(-1), 0);
        // Lower-case "true" is NOT mapped; atol("true") = 0 from junk →
        // default.
        assert_eq!(r.find_child("c").unwrap().as_long(-1), -1);
        assert_eq!(r.find_child("d").unwrap().as_long(-1), -42);
    }

    #[test]
    fn as_int64_stream_semantics() {
        let doc = parse_doc("<r><a>12abc</a><b>abc</b><c></c><d>  5 </d><e>99999999999999999999</e></r>");
        let r = &doc;
        assert_eq!(r.find_child("a").unwrap().as_int64(-1), 12, "junk tail ok");
        assert_eq!(r.find_child("b").unwrap().as_int64(-1), -1);
        assert_eq!(r.find_child("c").unwrap().as_int64(-1), -1);
        assert_eq!(r.find_child("d").unwrap().as_int64(-1), 5, "ws skipped");
        assert_eq!(
            r.find_child("e").unwrap().as_int64(-1),
            -1,
            "overflow fails extraction"
        );
    }

    #[test]
    fn as_uint64_sscanf_semantics() {
        let doc = parse_doc("<r><a>007</a><b>+7</b><c>abc</c><d></d></r>");
        let r = &doc;
        assert_eq!(r.find_child("a").unwrap().as_uint64(1), 7);
        assert_eq!(r.find_child("b").unwrap().as_uint64(1), 7);
        assert_eq!(r.find_child("c").unwrap().as_uint64(1), 1);
        assert_eq!(r.find_child("d").unwrap().as_uint64(1), 1);
    }

    #[test]
    fn as_double_and_float_prefix_semantics() {
        let doc = parse_doc("<r><a>1.5</a><b>12abc</b><c>.5</c><d></d><e>inf</e></r>");
        let r = &doc;
        assert_eq!(r.find_child("a").unwrap().as_double(0.0), 1.5);
        assert_eq!(r.find_child("b").unwrap().as_double(-1.0), 12.0);
        assert_eq!(r.find_child("c").unwrap().as_double(0.0), 0.5);
        assert_eq!(r.find_child("d").unwrap().as_double(-2.5), -2.5);
        assert!(r.find_child("e").unwrap().as_double(0.0).is_infinite());
        assert_eq!(r.find_child("a").unwrap().as_float(0.0), 1.5);
    }

    #[test]
    fn as_vector2_is_strict_stream_extraction() {
        let doc = parse_doc("<r><a>1 2</a><c>1 2 3</c><d>1</d><e>-1.5 2.5</e></r>");
        let r = &doc;
        assert_eq!(
            r.find_child("a").unwrap().as_vector2(Vector2::ZERO),
            Vector2::new(1.0, 2.0)
        );
        assert_eq!(
            r.find_child("c").unwrap().as_vector2(Vector2::ZERO),
            Vector2::ZERO,
            "extra number → not eof → default"
        );
        assert_eq!(
            r.find_child("d").unwrap().as_vector2(Vector2::ZERO),
            Vector2::ZERO,
            "missing second → fail → default"
        );
        assert_eq!(
            r.find_child("e").unwrap().as_vector2(Vector2::ZERO),
            Vector2::new(-1.5, 2.5)
        );

        // Trailing space can't survive parse-time trimming, so pin the eof
        // check with a programmatic value.
        let mut b = DataSection::new("b");
        b.set_string("1 2 ");
        assert_eq!(
            b.as_vector2(Vector2::ZERO),
            Vector2::ZERO,
            "trailing space → not eof → default"
        );
    }

    #[test]
    fn as_vector3_is_prefix_strtod() {
        let doc = parse_doc("<r><a>1 2 3</a><b>1 2 3 </b><c>1 2 3 junk</c><d>1 2</d><e>1x 2 3</e></r>");
        let r = &doc;
        let v3 = Vector3::new(1.0, 2.0, 3.0);
        assert_eq!(r.find_child("a").unwrap().as_vector3(Vector3::ZERO), v3);
        assert_eq!(
            r.find_child("b").unwrap().as_vector3(Vector3::ZERO),
            v3,
            "trailing space is fine for strtod"
        );
        assert_eq!(
            r.find_child("c").unwrap().as_vector3(Vector3::ZERO),
            v3,
            "trailing junk is fine for strtod"
        );
        assert_eq!(
            r.find_child("d").unwrap().as_vector3(Vector3::ZERO),
            Vector3::ZERO,
            "missing third number → default"
        );
        // "1x" parses as 1 but leaves the cursor at 'x', which fails the
        // SECOND strtod → default, exactly like the reference.
        assert_eq!(
            r.find_child("e").unwrap().as_vector3(Vector3::ZERO),
            Vector3::ZERO,
            "cursor after prefix junk breaks the next component"
        );
    }

    #[test]
    fn as_vector4_is_strict_like_vector2() {
        let doc = parse_doc("<r><a>1 2 3 4</a></r>");
        let r = &doc;
        assert_eq!(
            r.find_child("a").unwrap().as_vector4(Vector4::ZERO),
            Vector4::new(1.0, 2.0, 3.0, 4.0)
        );
        let mut b = DataSection::new("b");
        b.set_string("1 2 3 4 ");
        assert_eq!(
            b.as_vector4(Vector4::ZERO),
            Vector4::ZERO,
            "trailing space → not eof → default"
        );
    }

    #[test]
    fn read_vector3_finds_child_or_defaults() {
        let doc = parse_doc("<r><pos>1 2 3</pos><bad>9</bad></r>");
        assert_eq!(
            doc.read_vector3("pos", Vector3::ZERO),
            Vector3::new(1.0, 2.0, 3.0)
        );
        assert_eq!(
            doc.read_vector3("bad", Vector3::new(7.0, 8.0, 9.0)),
            Vector3::new(7.0, 8.0, 9.0)
        );
        assert_eq!(
            doc.read_vector3("missing", Vector3::new(7.0, 8.0, 9.0)),
            Vector3::new(7.0, 8.0, 9.0)
        );
    }

    // ------------------------------------------------------------------
    // Writers and save format
    // ------------------------------------------------------------------

    #[test]
    fn set_formats_match_the_reference() {
        let mut s = DataSection::new("v");
        s.set_bool(true);
        assert_eq!(s.as_string(), "true");
        s.set_int(-42);
        assert_eq!(s.as_string(), "-42");
        s.set_uint(7);
        assert_eq!(s.as_string(), "7");
        s.set_float(1.5);
        assert_eq!(s.as_string(), "1.500000", "setprecision(6) fixed");
        s.set_double(0.1);
        assert_eq!(s.as_string(), "0.10000000000000001", "precision(17) fixed");
        s.set_vector3(Vector3::new(1.0, -2.0, 3.25));
        assert_eq!(s.as_string(), "1.000000 -2.000000 3.250000");
        s.set_vector2(Vector2::new(1.0, 2.0));
        assert_eq!(s.as_string(), "1.000000 2.000000");
        s.set_vector4(Vector4::new(1.0, 2.0, 3.0, 4.0));
        assert_eq!(s.as_string(), "1.000000 2.000000 3.000000 4.000000");
    }

    #[test]
    fn save_layout_is_byte_exact() {
        // Build: root with value child, an attributed empty child and a
        // child that itself has children.
        let mut root = DataSection::new("root");
        let value = root.add_child("value");
        value.set_int(42);
        let holder = root.add_child("holder");
        {
            let attr = holder.add_child("x");
            attr.set_string("1");
            attr.is_attribute = true;
        }
        let group = root.add_child("group");
        group.add_child("leaf").set_string("deep");

        let expected = "<root>\n\
            \t<value>\t42\t</value>\n\
            \t<holder\n\
            \t\t\tx=\"1\" />\n\
            \t<group>\n\
            \t\t<leaf>\tdeep\t</leaf>\n\
            \t</group>\n\
            </root>\n";
        assert_eq!(root.write_to_string(), expected);
    }

    #[test]
    fn save_uses_empty_tag_form_when_value_and_children_empty() {
        let mut root = DataSection::new("root");
        root.add_child("a");
        assert_eq!(root.write_to_string(), "<root>\n\t<a />\n</root>\n");
    }

    #[test]
    fn save_escapes_values_and_attributes() {
        let mut root = DataSection::new("root");
        let a = root.add_child("a");
        a.set_string("x&<y>");
        {
            let q = a.add_child("q");
            q.set_string("\"s\"");
            q.is_attribute = true;
        }
        // Attribute-only children with a value still take the multi-line
        // form: the attribute is written after `<a`, then `>` + value, a
        // newline (children exist), and the closing tag at parent indent.
        let expected = "<root>\n\
            \t<a\n\
            \t\t\tq=\"&quot;s&quot;\">\tx&amp;&lt;y&gt;\n\
            \t</a>\n\
            </root>\n";
        assert_eq!(root.write_to_string(), expected);
    }

    #[test]
    fn save_rewrites_the_invalid_root_name() {
        let mut root = DataSection::new("1bad name");
        root.set_string("v");
        assert_eq!(
            root.write_to_string(),
            "<root>\tv\t</root>\n",
            "level 0 invalid names become 'root'"
        );
    }

    #[test]
    fn save_roundtrip_through_parse() {
        let mut root = DataSection::new("root");
        let a = root.add_child("nums");
        a.set_int(5);
        let f = root.add_child("flt");
        f.set_float(2.5);
        let g = root.add_child("group");
        g.add_child("inner").set_string("x");

        let out = root.write_to_string();
        let reparsed = parse_doc(&out);
        assert_eq!(
            reparsed.find_child("nums").unwrap().as_int(0),
            5,
            "saved output reparses"
        );
        assert_eq!(reparsed.find_child("flt").unwrap().as_float(0.0), 2.5);
        assert_eq!(
            reparsed
                .find_child("group")
                .unwrap()
                .find_child("inner")
                .unwrap()
                .as_string(),
            "x"
        );
    }

    // ------------------------------------------------------------------
    // is_valid_xml_tag
    // ------------------------------------------------------------------

    #[test]
    fn valid_xml_tag_rules() {
        assert!(is_valid_xml_tag("root"));
        assert!(is_valid_xml_tag("_x"));
        assert!(!is_valid_xml_tag(""));
        assert!(!is_valid_xml_tag("1x"), "leading digit");
        assert!(!is_valid_xml_tag("a b"), "space");
        assert!(!is_valid_xml_tag("a/b"), "slash");
        assert!(!is_valid_xml_tag("a\\b"), "backslash");
        assert!(!is_valid_xml_tag("\u{e9}"), "byte >= 0x80");
    }

    // ------------------------------------------------------------------
    // Remaining reference error paths
    // ------------------------------------------------------------------

    #[test]
    fn parse_error_displays_with_prefix() {
        let err = parse_err("<a><b>x</c></a>");
        let text = err.to_string();
        assert!(text.starts_with("xml parse error: "), "{text}");
        assert!(text.contains("do not match"), "{text}");
    }

    #[test]
    fn bad_comment_starter_is_a_parse_error() {
        // `<!-x`: the second character must be '-'.
        let err = parse_err("<a><!-x></a>");
        assert!(err.message.contains("bad comment"), "{}", err.message);
    }

    #[test]
    fn bad_cdata_starter_is_a_parse_error() {
        let err = parse_err("<a><![Xnope]]></a>");
        assert!(err.message.contains("CDATA"), "{}", err.message);
    }

    #[test]
    fn doctype_like_stub_consumes_two_chars_only() {
        // The reference consumes exactly `<!` + one char for unknown
        // `<!` bodies; the rest flows through the main loop as text.
        let doc = parse_doc("<a><!DOCTYPE html>tail</a>");
        assert_eq!(doc.as_string(), "OCTYPE html>tail");
    }

    #[test]
    fn unclosed_processing_instruction_is_a_parse_error() {
        let err = parse_err("<a><?xml");
        assert!(err.message.contains("not closed"), "{}", err.message);
    }

    #[test]
    fn attribute_name_at_end_of_tag_breaks_collection() {
        // Trailing junk without '=' stops attribute collection silently.
        let doc = parse_doc("<root><a x=\"1\" lone>text</a></root>");
        let a = doc.find_child("a").unwrap();
        assert_eq!(a.count_children(), 1, "only the well-formed attribute");
        assert_eq!(a.as_string(), "text");
    }

    #[test]
    fn attribute_without_value_breaks_collection() {
        // `x =>`: '=' present but the tag ends before a quoted value.
        let doc = parse_doc("<root><a x =>text</a></root>");
        let a = doc.find_child("a").unwrap();
        assert_eq!(a.count_children(), 0);
        assert_eq!(a.as_string(), "text");
    }

    #[test]
    fn duplicate_attribute_names_demote_on_save() {
        let mut root = parse_doc("<root><a x=\"1\" x=\"2\">v</a></root>");
        let out = root.write_to_string();
        // First x stays an attribute; the duplicate becomes a normal
        // child element in the output.
        assert!(out.contains("x=\"1\""), "{out}");
        assert!(out.contains("<x>\t2\t</x>"), "{out}");
    }

    // ------------------------------------------------------------------
    // Remaining numeric edge paths
    // ------------------------------------------------------------------

    #[test]
    fn as_uint64_overflow_saturates_like_sscanf() {
        let mut s = DataSection::new("v");
        s.set_string("99999999999999999999999");
        assert_eq!(s.as_uint64(1), u64::MAX, "glibc saturates the match");
    }

    #[test]
    fn as_int_accepts_c_whitespace_via_strtol() {
        let mut s = DataSection::new("v");
        s.set_string("\u{b}7\u{c}");
        assert_eq!(s.as_int(0), 7, "vertical tab / form feed are isspace");
    }

    #[test]
    fn as_double_inf_nan_and_exponent_forms() {
        let mut s = DataSection::new("v");
        s.set_string("-inf");
        assert_eq!(s.as_double(0.0), f64::NEG_INFINITY);
        s.set_string("NAN");
        assert!(s.as_double(0.0).is_nan());
        s.set_string("1e3");
        assert_eq!(s.as_double(0.0), 1000.0);
        s.set_string("1e");
        assert_eq!(s.as_double(0.0), 1.0, "exponent without digits is not consumed");
        s.set_string("+2.5");
        assert_eq!(s.as_double(0.0), 2.5);
        s.set_string("  3.5");
        assert_eq!(s.as_double(0.0), 3.5, "leading C whitespace is skipped");
        s.set_string(".");
        assert_eq!(s.as_double(-1.0), -1.0, "a lone dot parses nothing");
    }

    #[test]
    fn as_vector2_rejects_numeric_overflow_but_accepts_literal_inf() {
        let mut s = DataSection::new("v");
        s.set_string("1e40 2");
        assert_eq!(
            s.as_vector2(Vector2::new(-7.0, -7.0)),
            Vector2::new(-7.0, -7.0),
            "float overflow sets failbit"
        );
        s.set_string("inf 2");
        let v = s.as_vector2(Vector2::ZERO);
        assert!(v.x.is_infinite() && v.y == 2.0, "{v:?}");
    }

    // ------------------------------------------------------------------
    // Remaining writers
    // ------------------------------------------------------------------

    #[test]
    fn remaining_set_writers_format_like_the_reference() {
        let mut s = DataSection::new("v");
        s.set_uint(4294967295);
        assert_eq!(s.as_string(), "4294967295");
        s.set_long(-9223372036854775808);
        assert_eq!(s.as_string(), "-9223372036854775808");
        s.set_int64(9223372036854775807);
        assert_eq!(s.as_string(), "9223372036854775807");
        s.set_uint64(18446744073709551615);
        assert_eq!(s.as_string(), "18446744073709551615");
        s.set_double(2.0);
        assert_eq!(s.as_string(), "2.00000000000000000");
        s.set_bool(false);
        assert_eq!(s.as_string(), "false");
    }

    // ------------------------------------------------------------------
    // Coverage: parser edge paths
    // ------------------------------------------------------------------

    #[test]
    fn bang_marker_at_eof_is_an_error() {
        // `<!` runs off the document before the body type can be read.
        let err = parse_err("<r><!");
        assert!(err.message.contains("unexpected end of document"), "{err:?}");
    }

    #[test]
    fn cdata_with_nul_truncates_the_content() {
        let doc = parse("root", b"<r><c><![CDATA[ab\0cd]]></c></r>").unwrap();
        let c = doc.find_child("c").unwrap();
        assert_eq!(c.as_string(), "ab", "NUL truncates CDATA like strlen");
    }

    #[test]
    fn cdata_with_solo_bracket_keeps_scanning() {
        // A single ']' without a matching "]]>" stays in the content.
        let doc = parse_doc("<r><c><![CDATA[a]b]]></c></r>");
        let c = doc.find_child("c").unwrap();
        assert_eq!(c.as_string(), "a]b");
    }

    #[test]
    fn cdata_not_closed_is_fatal() {
        let err = parse_err("<r><c><![CDATA[xyz</c></r>");
        assert!(err.message.contains("CDATA"), "{err:?}");
    }

    #[test]
    fn processing_instruction_trailing_whitespace_is_trimmed() {
        let doc = parse_doc("<r><?pi x ?>v</r>");
        assert_eq!(doc.as_string(), "v", "the PI itself leaves no content");
    }

    #[test]
    fn attribute_spacing_before_equals_still_parses() {
        // Whitespace between the name and '=' must be skipped.
        let doc = parse_doc("<r a  =\"1\">v</r>");
        let a = doc.find_child("a").expect("attribute node kept");
        assert!(a.is_attribute());
        assert_eq!(a.as_string(), "1");
        assert_eq!(doc.as_string(), "v");
    }

    #[test]
    fn attribute_without_equals_is_dropped_but_tag_survives() {
        // Reference: error message then break out of the attribute loop;
        // the later `b="1"` attribute is never collected either.
        let doc = parse_doc("<r a b=\"1\">v</r>");
        assert_eq!(doc.count_children(), 0, "no attribute collected");
        assert_eq!(doc.as_string(), "v");
    }

    #[test]
    fn attribute_spacing_after_equals_still_parses() {
        let doc = parse_doc("<r a= \"1\">v</r>");
        let a = doc.find_child("a").expect("attribute node kept");
        assert_eq!(a.as_string(), "1");
    }

    #[test]
    fn tag_trailing_whitespace_is_trimmed() {
        // The tag text `r ` is trimmed before the name is extracted, so
        // this parses as an ordinary `<r>` element.
        // The single root tag is renamed to "root" by the parser, so the
        // value and child count carry the assertion.
        let doc = parse_doc("<r >v</r>");
        assert_eq!(doc.as_string(), "v");
        assert_eq!(doc.count_children(), 0);
    }

    #[test]
    fn attribute_with_unterminated_quote_is_dropped() {
        let doc = parse_doc("<r a=\"abc>v</r>");
        assert_eq!(doc.count_children(), 0);
        assert_eq!(doc.as_string(), "v");
    }

    #[test]
    fn text_value_with_nul_truncates() {
        let doc = parse("root", b"<r><v>a\0b</v></r>").unwrap();
        assert_eq!(doc.find_child("v").unwrap().as_string(), "a");
    }

    // ------------------------------------------------------------------
    // Coverage: numeric converter edge paths
    // ------------------------------------------------------------------

    #[test]
    fn converters_cover_the_glibc_edge_branches() {
        let mut s = DataSection::new("v");

        // strtoul: u64 overflow is ERANGE → default; negative wraps before
        // the u32 truncation; a body with no digits parses as 0 which is
        // not the literal "0" → default.
        s.set_string("99999999999999999999999");
        assert_eq!(s.as_uint(7), 7, "u64 overflow → ERANGE → default");
        assert_eq!(s.as_long(7), 7, "i64 overflow → ERANGE → default");
        assert_eq!(s.as_int64(7), 7);
        s.set_string("-1");
        assert_eq!(s.as_uint(7), u32::MAX, "wrap then truncate to u32");
        s.set_string("abc");
        assert_eq!(s.as_uint(7), 7, "no digits parses 0, not literal \"0\"");
        s.set_string("");
        assert_eq!(s.as_long(7), 7, "empty value → default");

        // as_int64: C whitespace before the sign, then a signed result.
        s.set_string("\u{b}+7");
        assert_eq!(s.as_int64(0), 7);
        s.set_string("-42");
        assert_eq!(s.as_int64(0), -42);

        // as_uint64 (probed against glibc sscanf %llu): whitespace, sign
        // wrap, overflow clamp.
        s.set_string("\t+9");
        assert_eq!(s.as_uint64(0), 9);
        s.set_string("-5");
        assert_eq!(s.as_uint64(0), u64::MAX - 4, "sscanf %llu wraps the sign");
        s.set_string("-99999999999999999999999");
        assert_eq!(s.as_uint64(0), u64::MAX, "negative overflow clamps");
    }

    #[test]
    fn vector4_needs_all_four_numbers() {
        let mut s = DataSection::new("v");
        s.set_string("1 2 3");
        assert_eq!(
            s.as_vector4(Vector4::new(-1.0, -1.0, -1.0, -1.0)),
            Vector4::new(-1.0, -1.0, -1.0, -1.0),
            "a missing fourth number fails extraction"
        );
    }

    #[test]
    fn strtod_accepts_infinity_words_and_signed_exponents() {
        let mut s = DataSection::new("v");
        s.set_string("infinity");
        assert!(s.as_double(0.0).is_infinite(), "8-character literal");
        s.set_string("-infinity");
        assert!(s.as_double(0.0).is_sign_negative() && s.as_double(0.0).is_infinite());
        s.set_string("1e+2");
        assert_eq!(s.as_double(0.0), 100.0);
        s.set_string("1e-2");
        assert_eq!(s.as_double(0.0), 0.01);
    }

    #[test]
    fn stream_float_rejects_numeric_overflow_but_takes_signed_inf() {
        let mut s = DataSection::new("v");
        // 1e400 overflows f64 to infinity, which is not a literal → failbit.
        s.set_string("1e400 2");
        assert_eq!(s.as_vector2(Vector2::new(-1.0, -1.0)), Vector2::new(-1.0, -1.0));
        // A signed literal inf passes through the sign-consume path.
        s.set_string("-inf 2");
        let v = s.as_vector2(Vector2::ZERO);
        assert!(v.x.is_infinite() && v.x.is_sign_negative());
        assert_eq!(v.y, 2.0);
    }

    // ------------------------------------------------------------------
    // Coverage: mutation accessors
    // ------------------------------------------------------------------

    #[test]
    fn child_mut_gives_mutable_access_by_index() {
        let mut doc = parse_doc("<r><a>1</a><b>2</b></r>");
        doc.child_mut(1).unwrap().set_string("renamed");
        assert_eq!(doc.open_child(1).unwrap().as_string(), "renamed");
        assert!(doc.child_mut(2).is_none());
    }
}
