//! `Dependency` model port (`dependency/*.hpp|cpp`): the six dependency
//! kinds the asset pipeline tracks per conversion task.
//!
//! Reference quirks preserved:
//! - the enum order is fixed and serialised by discriminant (`DEPENDENCY_TYPES`
//!   is emitted in order; new types go at the end only);
//! - every dependency carries a `critical` flag serialised as a `Critical`
//!   attribute; a critical dependency fails the conversion when it errors;
//! - file paths are asserted relative in the reference (`pathIsRelative`),
//!   and file/pattern fields are serialised as string attributes.
//!
//! The reference stores the `critical_` flag on the base class and the
//! type-specific data on the subclasses; here the concrete data structs each
//! carry `critical` so a single `Dependency` enum can dispatch on type.

use resources::datasection::DataSection;

/// `DependencyError`: a dependency data section was malformed (a required
/// attribute was missing or the type tag was unknown). Mirrors the
/// reference's `bool` return, which reports the same failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DependencyError(pub &'static str);

impl std::fmt::Display for DependencyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "dependency serialisation error: {}", self.0)
    }
}

impl std::error::Error for DependencyError {}

/// `DependencyType` (`DEPENDENCY_TYPES`): the discriminant order is part of
/// the on-disk serialisation and must not be changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum DependencyType {
    SourceFile = 0,
    IntermediateFile = 1,
    OutputFile = 2,
    Converter = 3,
    ConverterParams = 4,
    Directory = 5,
}

impl DependencyType {
    /// All dependency types in `DEPENDENCY_TYPES` order.
    pub const ALL: [DependencyType; 6] = [
        DependencyType::SourceFile,
        DependencyType::IntermediateFile,
        DependencyType::OutputFile,
        DependencyType::Converter,
        DependencyType::ConverterParams,
        DependencyType::Directory,
    ];

    /// `InvalidDependencyType` (the count) — an unknown discriminant.
    pub const INVALID: u32 = 6;

    pub fn from_u32(value: u32) -> Option<DependencyType> {
        Self::ALL.iter().copied().find(|t| *t as u32 == value)
    }
}

/// `SourceFileDependency`: a source input file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFileDependency {
    file_name: String,
    critical: bool,
}

/// `IntermediateFileDependency`: a compiled intermediate input file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntermediateFileDependency {
    file_name: String,
    critical: bool,
}

/// `OutputFileDependency`: a final output input file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputFileDependency {
    file_name: String,
    critical: bool,
}

/// `ConverterDependency`: the converter id + version used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConverterDependency {
    converter_id: u64,
    converter_version: String,
    critical: bool,
}

/// `ConverterParamsDependency`: the converter parameter string used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConverterParamsDependency {
    converter_params: String,
    critical: bool,
}

/// `DirectoryDependency`: a directory scanned with a pattern/recursion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryDependency {
    directory: String,
    pattern: String,
    regex: bool,
    recursive: bool,
    critical: bool,
}

/// `Dependency`: one of the six dependency kinds, each carrying its data and
/// the shared `critical` flag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dependency {
    SourceFile(SourceFileDependency),
    IntermediateFile(IntermediateFileDependency),
    OutputFile(OutputFileDependency),
    Converter(ConverterDependency),
    ConverterParams(ConverterParamsDependency),
    Directory(DirectoryDependency),
}

// --- attribute tag names (per-file `*_Locals` namespaces) ---
const CRITICAL_TAG: &str = "Critical";
const FILE_TAG: &str = "File";
const ID_TAG: &str = "Id";
const VERSION_TAG: &str = "Version";
const PARAMS_TAG: &str = "Params";
const DIRECTORY_TAG: &str = "Directory";
const PATTERN_TAG: &str = "Pattern";
const REGEX_TAG: &str = "Regex";
const RECURSIVE_TAG: &str = "Recursive";

impl Dependency {
    /// `getType()`: the discriminant of this dependency.
    pub fn dependency_type(&self) -> DependencyType {
        match self {
            Dependency::SourceFile(_) => DependencyType::SourceFile,
            Dependency::IntermediateFile(_) => DependencyType::IntermediateFile,
            Dependency::OutputFile(_) => DependencyType::OutputFile,
            Dependency::Converter(_) => DependencyType::Converter,
            Dependency::ConverterParams(_) => DependencyType::ConverterParams,
            Dependency::Directory(_) => DependencyType::Directory,
        }
    }

    pub fn is_critical(&self) -> bool {
        match self {
            Dependency::SourceFile(d) => d.critical,
            Dependency::IntermediateFile(d) => d.critical,
            Dependency::OutputFile(d) => d.critical,
            Dependency::Converter(d) => d.critical,
            Dependency::ConverterParams(d) => d.critical,
            Dependency::Directory(d) => d.critical,
        }
    }

    pub fn set_critical(&mut self, critical: bool) {
        match self {
            Dependency::SourceFile(d) => d.critical = critical,
            Dependency::IntermediateFile(d) => d.critical = critical,
            Dependency::OutputFile(d) => d.critical = critical,
            Dependency::Converter(d) => d.critical = critical,
            Dependency::ConverterParams(d) => d.critical = critical,
            Dependency::Directory(d) => d.critical = critical,
        }
    }

    /// `serialiseOut`: write the type-specific children, then the base
    /// `Critical` attribute (the reference writes Critical last).
    pub fn serialise_out(&self, section: &mut DataSection) -> Result<(), DependencyError> {
        match self {
            Dependency::SourceFile(d) => {
                write_file_attr(section, FILE_TAG, &d.file_name)?;
            }
            Dependency::IntermediateFile(d) => {
                write_file_attr(section, FILE_TAG, &d.file_name)?;
            }
            Dependency::OutputFile(d) => {
                write_file_attr(section, FILE_TAG, &d.file_name)?;
            }
            Dependency::Converter(d) => {
                write_uint64_attr(section, ID_TAG, d.converter_id)?;
                write_str_attr(section, VERSION_TAG, &d.converter_version)?;
            }
            Dependency::ConverterParams(d) => {
                write_str_attr(section, PARAMS_TAG, &d.converter_params)?;
            }
            Dependency::Directory(d) => {
                write_str_attr(section, DIRECTORY_TAG, &d.directory)?;
                write_str_attr(section, PATTERN_TAG, &d.pattern)?;
                write_bool_attr(section, REGEX_TAG, d.regex)?;
                write_bool_attr(section, RECURSIVE_TAG, d.recursive)?;
            }
        }
        write_bool_attr(section, CRITICAL_TAG, self.is_critical())
    }

    /// `serialiseIn`-equivalent on the concrete kind: read the type-specific
    /// children and the base `Critical` attribute.
    fn serialise_common(section: &DataSection, critical: &mut bool) -> Result<(), DependencyError> {
        let critical_attr = section
            .find_child(CRITICAL_TAG)
            .ok_or(DependencyError("missing Critical attribute"))?;
        if !critical_attr.is_attribute() {
            return Err(DependencyError("Critical is not an attribute"));
        }
        *critical = critical_attr.as_bool(false);
        Ok(())
    }

    /// `serialiseIn` dispatch (`CreateDependency[type]()->serialiseIn`): the
    /// reference reads `Type` in the list and constructs the concrete type
    /// through a factory before calling in, so the kind arrives as a
    /// parameter here and only the concrete children plus `Critical` are
    /// read from the section.
    pub fn serialise_in(
        section: &DataSection,
        dep_type: DependencyType,
    ) -> Result<Dependency, DependencyError> {
        let mut critical = false;
        Self::serialise_common(section, &mut critical)?;

        Ok(match dep_type {
            DependencyType::SourceFile => Dependency::SourceFile(SourceFileDependency {
                file_name: read_file_attr(section)?,
                critical,
            }),
            DependencyType::IntermediateFile => {
                Dependency::IntermediateFile(IntermediateFileDependency {
                    file_name: read_file_attr(section)?,
                    critical,
                })
            }
            DependencyType::OutputFile => Dependency::OutputFile(OutputFileDependency {
                file_name: read_file_attr(section)?,
                critical,
            }),
            DependencyType::Converter => Dependency::Converter(ConverterDependency {
                converter_id: read_uint64_attr(section, ID_TAG)?,
                converter_version: read_str_attr(section, VERSION_TAG)?,
                critical,
            }),
            DependencyType::ConverterParams => {
                Dependency::ConverterParams(ConverterParamsDependency {
                    converter_params: read_str_attr(section, PARAMS_TAG)?,
                    critical,
                })
            }
            DependencyType::Directory => Dependency::Directory(DirectoryDependency {
                directory: read_str_attr(section, DIRECTORY_TAG)?,
                pattern: read_str_attr(section, PATTERN_TAG)?,
                regex: read_bool_attr(section, REGEX_TAG)?,
                recursive: read_bool_attr(section, RECURSIVE_TAG)?,
                critical,
            }),
        })
    }
}

// --- concrete accessors (mirror the reference getters) ---

impl SourceFileDependency {
    pub fn new(file_name: impl Into<String>) -> Self {
        Self {
            file_name: file_name.into(),
            critical: false,
        }
    }
    pub fn file_name(&self) -> &str {
        &self.file_name
    }
}

impl IntermediateFileDependency {
    pub fn new(file_name: impl Into<String>) -> Self {
        Self {
            file_name: file_name.into(),
            critical: false,
        }
    }
    pub fn file_name(&self) -> &str {
        &self.file_name
    }
}

impl OutputFileDependency {
    pub fn new(file_name: impl Into<String>) -> Self {
        Self {
            file_name: file_name.into(),
            critical: false,
        }
    }
    pub fn file_name(&self) -> &str {
        &self.file_name
    }
}

impl ConverterDependency {
    pub fn new(converter_id: u64, converter_version: impl Into<String>) -> Self {
        Self {
            converter_id,
            converter_version: converter_version.into(),
            critical: false,
        }
    }
    pub fn converter_id(&self) -> u64 {
        self.converter_id
    }
    pub fn converter_version(&self) -> &str {
        &self.converter_version
    }
}

impl ConverterParamsDependency {
    pub fn new(converter_params: impl Into<String>) -> Self {
        Self {
            converter_params: converter_params.into(),
            critical: false,
        }
    }
    pub fn converter_params(&self) -> &str {
        &self.converter_params
    }
}

impl DirectoryDependency {
    pub fn new(
        directory: impl Into<String>,
        pattern: impl Into<String>,
        regex: bool,
        recursive: bool,
    ) -> Self {
        Self {
            directory: directory.into(),
            pattern: pattern.into(),
            regex,
            recursive,
            critical: false,
        }
    }
    pub fn directory(&self) -> &str {
        &self.directory
    }
    pub fn pattern(&self) -> &str {
        &self.pattern
    }
    pub fn is_regex(&self) -> bool {
        self.regex
    }
    pub fn is_recursive(&self) -> bool {
        self.recursive
    }
}

// --- attribute helpers (the reference's findChild + isAttribute checks) ---

fn read_attr<'a>(section: &'a DataSection, tag: &str) -> Result<&'a DataSection, DependencyError> {
    let attr = section
        .find_child(tag)
        .ok_or(DependencyError("missing attribute"))?;
    if !attr.is_attribute() {
        return Err(DependencyError("expected attribute"));
    }
    Ok(attr)
}

fn read_file_attr(section: &DataSection) -> Result<String, DependencyError> {
    Ok(read_attr(section, FILE_TAG)?.as_string().to_string())
}

fn read_str_attr(section: &DataSection, tag: &str) -> Result<String, DependencyError> {
    Ok(read_attr(section, tag)?.as_string().to_string())
}

fn read_bool_attr(section: &DataSection, tag: &str) -> Result<bool, DependencyError> {
    Ok(read_attr(section, tag)?.as_bool(false))
}

fn read_uint64_attr(section: &DataSection, tag: &str) -> Result<u64, DependencyError> {
    Ok(read_attr(section, tag)?.as_uint64(0))
}

fn write_attr<'a>(section: &'a mut DataSection, tag: &str) -> &'a mut DataSection {
    let attr = section.add_child(tag);
    attr.set_attribute(true);
    attr
}

fn write_str_attr(
    section: &mut DataSection,
    tag: &str,
    value: &str,
) -> Result<(), DependencyError> {
    write_attr(section, tag).set_string(value);
    Ok(())
}

fn write_file_attr(
    section: &mut DataSection,
    tag: &str,
    value: &str,
) -> Result<(), DependencyError> {
    write_str_attr(section, tag, value)
}

fn write_bool_attr(
    section: &mut DataSection,
    tag: &str,
    value: bool,
) -> Result<(), DependencyError> {
    write_attr(section, tag).set_bool(value);
    Ok(())
}

fn write_uint64_attr(
    section: &mut DataSection,
    tag: &str,
    value: u64,
) -> Result<(), DependencyError> {
    write_attr(section, tag).set_uint64(value);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Port of `DependencyUnitTest::testDependencySerialisation`: for each
    /// type, serialise out, serialise in, and verify the critical flag and
    /// the concrete data round-trip.
    fn round_trip(dep: Dependency, critical: bool) {
        let mut dep = dep;
        dep.set_critical(critical);

        let mut section = DataSection::new("Dependency");
        dep.serialise_out(&mut section).expect("serialise out");

        let back = Dependency::serialise_in(&section, dep.dependency_type()).expect("serialise in");
        assert_eq!(
            back,
            dep,
            "round-trip mismatch for {:?}",
            dep.dependency_type()
        );
        assert_eq!(back.is_critical(), critical);
    }

    #[test]
    fn source_file_round_trip() {
        round_trip(
            Dependency::SourceFile(SourceFileDependency::new("TestFileName")),
            false,
        );
        round_trip(
            Dependency::SourceFile(SourceFileDependency::new("TestFileName")),
            true,
        );
    }

    #[test]
    fn intermediate_file_round_trip() {
        round_trip(
            Dependency::IntermediateFile(IntermediateFileDependency::new("TestFileName")),
            false,
        );
        round_trip(
            Dependency::IntermediateFile(IntermediateFileDependency::new("TestFileName")),
            true,
        );
    }

    #[test]
    fn output_file_round_trip() {
        round_trip(
            Dependency::OutputFile(OutputFileDependency::new("TestFileName")),
            false,
        );
        round_trip(
            Dependency::OutputFile(OutputFileDependency::new("TestFileName")),
            true,
        );
    }

    #[test]
    fn converter_round_trip() {
        round_trip(
            Dependency::Converter(ConverterDependency::new(0xABC12345, "dEf.67890")),
            false,
        );
        round_trip(
            Dependency::Converter(ConverterDependency::new(0xABC12345, "dEf.67890")),
            true,
        );
    }

    #[test]
    fn converter_params_round_trip() {
        round_trip(
            Dependency::ConverterParams(ConverterParamsDependency::new("-a -b true -c 0 --def")),
            false,
        );
        round_trip(
            Dependency::ConverterParams(ConverterParamsDependency::new("-a -b true -c 0 --def")),
            true,
        );
    }

    #[test]
    fn directory_round_trip() {
        round_trip(
            Dependency::Directory(DirectoryDependency::new("directory", "pattern", true, true)),
            false,
        );
        round_trip(
            Dependency::Directory(DirectoryDependency::new("directory", "pattern", true, true)),
            true,
        );
    }

    #[test]
    fn all_types_have_distinct_discriminants() {
        for (i, t) in DependencyType::ALL.iter().enumerate() {
            assert_eq!(*t as u32, i as u32);
        }
    }

    fn attr<'a>(section: &'a mut DataSection, tag: &str) -> &'a mut DataSection {
        let a = section.add_child(tag);
        a.set_attribute(true);
        a
    }

    /// Reading with the wrong concrete kind (e.g. a Directory section read
    /// as a SourceFile) fails on the missing `File` attribute.
    #[test]
    fn wrong_type_is_rejected() {
        let dep = Dependency::Directory(DirectoryDependency::new("d", "p", true, true));
        let mut section = DataSection::new("Dependency");
        dep.serialise_out(&mut section).unwrap();
        assert!(Dependency::serialise_in(&section, DependencyType::SourceFile).is_err());
    }

    #[test]
    fn missing_critical_is_rejected() {
        let mut section = DataSection::new("Dependency");
        attr(&mut section, "File").set_string("x");
        assert!(Dependency::serialise_in(&section, DependencyType::SourceFile).is_err());
    }

    /// The serialised attribute set matches the reference exactly: the
    /// type-specific children come before the `Critical` attribute.
    #[test]
    fn serialise_writes_expected_children() {
        let dep = Dependency::SourceFile(SourceFileDependency::new("model/hero.model"));
        let mut section = DataSection::new("Dependency");
        dep.serialise_out(&mut section).unwrap();
        // Children order: File, Critical (Type/Hash are added by the list,
        // not the dependency itself).
        let names: Vec<&str> = (0..section.count_children())
            .map(|i| section.open_child(i).unwrap().section_name())
            .collect();
        assert_eq!(names, vec!["File", "Critical"]);
        assert_eq!(
            section.find_child("File").unwrap().as_string(),
            "model/hero.model"
        );
    }
}
