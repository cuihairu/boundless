//! `DependencyList` port (`dependency/dependency_list.hpp|cpp`): the input
//! dependencies and outputs a conversion task tracks.
//!
//! Reference quirks preserved:
//! - inputs are `(dependency, hash)` pairs; the primary hashes are computed
//!   via the compiler's `getHash` at add time, secondary hashes are `0` and
//!   filled in later by the task processor;
//! - `getInputHash(includeSecondary)` folds the primary hashes (always) and
//!   the secondary hashes (only when `includeSecondary`) with `Hash64::combine`;
//! - serialisation writes `PrimaryInputs`/`SecondaryInputs` sections of
//!   `Dependency` nodes (each `Type`+`Hash` plus the concrete children) and
//!   `IntermediateOutputs`/`Outputs` sections of `Compiled` nodes
//!   (`File`+`Hash`), exactly in that order.
//!
//! The reference holds a `Compiler&` to compute dependency hashes; here that
//! is abstracted behind [`HashProvider`] so the list is testable without a
//! real compiler.

use resources::datasection::DataSection;

use crate::dependency::{Dependency, DependencyError};
use crate::hash64;

/// `HashProvider`: the compiler-side hash oracle for a dependency
/// (`AssetCompiler::getHash`).
pub trait HashProvider {
    /// Hash a dependency (file contents, converter id/version, params, or
    /// directory contents, per the dependency's kind).
    fn get_hash(&self, dependency: &Dependency) -> u64;
}

/// A `(Dependency, hash)` input pair.
pub type Input = (Dependency, u64);
/// A `(filename, hash)` output pair.
pub type Output = (String, u64);

/// Serialisation tag names (`DependencyList_Locals`).
const PRIMARY_INPUTS_TAG: &str = "PrimaryInputs";
const SECONDARY_INPUTS_TAG: &str = "SecondaryInputs";
const INTERMEDIATE_OUTPUTS_TAG: &str = "IntermediateOutputs";
const OUTPUTS_TAG: &str = "Outputs";
const DEPENDENCY_TAG: &str = "Dependency";
const TYPE_TAG: &str = "Type";
const HASH_TAG: &str = "Hash";
const COMPILED_TAG: &str = "Compiled";
const FILE_TAG: &str = "File";

/// `DependencyList`: primary and secondary inputs plus the files they build
/// into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DependencyList<H: HashProvider> {
    compiler: H,
    primary_inputs: Vec<Input>,
    secondary_inputs: Vec<Input>,
    intermediate_outputs: Vec<Output>,
    outputs: Vec<Output>,
}

impl<H: HashProvider> DependencyList<H> {
    pub fn new(compiler: H) -> Self {
        Self {
            compiler,
            primary_inputs: Vec::new(),
            secondary_inputs: Vec::new(),
            intermediate_outputs: Vec::new(),
            outputs: Vec::new(),
        }
    }

    pub fn primary_inputs(&self) -> &[Input] {
        &self.primary_inputs
    }

    pub fn secondary_inputs(&self) -> &[Input] {
        &self.secondary_inputs
    }

    pub fn intermediate_outputs(&self) -> &[Output] {
        &self.intermediate_outputs
    }

    pub fn outputs(&self) -> &[Output] {
        &self.outputs
    }

    /// `reset()`: clear every input and output.
    pub fn reset(&mut self) {
        self.primary_inputs.clear();
        self.secondary_inputs.clear();
        self.intermediate_outputs.clear();
        self.outputs.clear();
    }

    /// `initialise( source, converterId, converterVersion, converterParams )`:
    /// reset, then add the primary source, converter and params dependencies
    /// (all critical).
    pub fn initialise(
        &mut self,
        source: impl Into<String>,
        converter_id: u64,
        converter_version: impl Into<String>,
        converter_params: impl Into<String>,
    ) {
        self.reset();
        self.add_primary_source_file_dependency(source);
        self.add_primary_converter_dependency(converter_id, converter_version);
        self.add_primary_converter_params_dependency(converter_params);
    }

    /// `addPrimarySourceFileDependency`.
    pub fn add_primary_source_file_dependency(&mut self, filename: impl Into<String>) {
        let mut dependency =
            Dependency::SourceFile(crate::dependency::SourceFileDependency::new(filename));
        dependency.set_critical(true);
        let hash = self.compiler.get_hash(&dependency);
        self.primary_inputs.push((dependency, hash));
    }

    /// `addPrimaryConverterDependency`.
    pub fn add_primary_converter_dependency(
        &mut self,
        converter_id: u64,
        converter_version: impl Into<String>,
    ) {
        let mut dependency = Dependency::Converter(crate::dependency::ConverterDependency::new(
            converter_id,
            converter_version,
        ));
        dependency.set_critical(true);
        let hash = self.compiler.get_hash(&dependency);
        self.primary_inputs.push((dependency, hash));
    }

    /// `addPrimaryConverterParamsDependency`.
    pub fn add_primary_converter_params_dependency(&mut self, converter_params: impl Into<String>) {
        let mut dependency = Dependency::ConverterParams(
            crate::dependency::ConverterParamsDependency::new(converter_params),
        );
        dependency.set_critical(true);
        let hash = self.compiler.get_hash(&dependency);
        self.primary_inputs.push((dependency, hash));
    }

    /// `addSecondarySourceFileDependency`.
    pub fn add_secondary_source_file_dependency(
        &mut self,
        filename: impl Into<String>,
        critical: bool,
    ) {
        let mut dependency =
            Dependency::SourceFile(crate::dependency::SourceFileDependency::new(filename));
        dependency.set_critical(critical);
        self.secondary_inputs.push((dependency, 0));
    }

    /// `addSecondaryIntermediateFileDependency`.
    pub fn add_secondary_intermediate_file_dependency(
        &mut self,
        filename: impl Into<String>,
        critical: bool,
    ) {
        let mut dependency = Dependency::IntermediateFile(
            crate::dependency::IntermediateFileDependency::new(filename),
        );
        dependency.set_critical(critical);
        self.secondary_inputs.push((dependency, 0));
    }

    /// `addSecondaryOutputFileDependency`.
    pub fn add_secondary_output_file_dependency(
        &mut self,
        filename: impl Into<String>,
        critical: bool,
    ) {
        let mut dependency =
            Dependency::OutputFile(crate::dependency::OutputFileDependency::new(filename));
        dependency.set_critical(critical);
        self.secondary_inputs.push((dependency, 0));
    }

    /// `addSecondaryDirectoryDependency`.
    pub fn add_secondary_directory_dependency(
        &mut self,
        directory: impl Into<String>,
        pattern: impl Into<String>,
        regex: bool,
        recursive: bool,
        critical: bool,
    ) {
        let mut dependency = Dependency::Directory(crate::dependency::DirectoryDependency::new(
            directory, pattern, regex, recursive,
        ));
        dependency.set_critical(critical);
        self.secondary_inputs.push((dependency, 0));
    }

    /// Record an intermediate output `(file, hash)` (used by the processor).
    pub fn add_intermediate_output(&mut self, file: impl Into<String>, hash: u64) {
        self.intermediate_outputs.push((file.into(), hash));
    }

    /// Record a final output `(file, hash)` (used by the processor).
    pub fn add_output(&mut self, file: impl Into<String>, hash: u64) {
        self.outputs.push((file.into(), hash));
    }

    /// `getInputHash( includeSecondary )`: fold the stored hashes with
    /// `Hash64::combine`, always including primary inputs and adding the
    /// secondary inputs (and their count) only when requested.
    pub fn get_input_hash(&self, include_secondary: bool) -> u64 {
        let mut hash = 0u64;
        for (dependency, dep_hash) in &self.primary_inputs {
            let _ = dependency;
            hash64::combine(&mut hash, *dep_hash);
        }
        if include_secondary {
            hash64::combine(&mut hash, self.secondary_inputs.len() as u64);
            for (_dependency, dep_hash) in &self.secondary_inputs {
                hash64::combine(&mut hash, *dep_hash);
            }
        }
        hash
    }

    /// `serialiseOut`: clear the section and write the four sections in
    /// order (the reference `delChildren()` first).
    pub fn serialise_out(&self, section: &mut DataSection) -> Result<(), DependencyError> {
        section.del_children();
        serialise_inputs_out(section.add_child(PRIMARY_INPUTS_TAG), &self.primary_inputs)?;
        serialise_inputs_out(
            section.add_child(SECONDARY_INPUTS_TAG),
            &self.secondary_inputs,
        )?;
        serialise_outputs_out(
            section.add_child(INTERMEDIATE_OUTPUTS_TAG),
            &self.intermediate_outputs,
        )?;
        serialise_outputs_out(section.add_child(OUTPUTS_TAG), &self.outputs)?;
        Ok(())
    }

    /// `serialiseIn`: reset and read the four sections in order.
    pub fn serialise_in(&mut self, section: &DataSection) -> Result<(), DependencyError> {
        self.reset();
        let primary = section
            .find_child(PRIMARY_INPUTS_TAG)
            .ok_or(DependencyError("missing PrimaryInputs"))?;
        let secondary = section
            .find_child(SECONDARY_INPUTS_TAG)
            .ok_or(DependencyError("missing SecondaryInputs"))?;
        let intermediate = section
            .find_child(INTERMEDIATE_OUTPUTS_TAG)
            .ok_or(DependencyError("missing IntermediateOutputs"))?;
        let outputs = section
            .find_child(OUTPUTS_TAG)
            .ok_or(DependencyError("missing Outputs"))?;

        self.primary_inputs = serialise_inputs_in(primary)?;
        self.secondary_inputs = serialise_inputs_in(secondary)?;
        self.intermediate_outputs = serialise_outputs_in(intermediate)?;
        self.outputs = serialise_outputs_in(outputs)?;
        Ok(())
    }
}

fn serialise_inputs_out(
    section: &mut DataSection,
    inputs: &[Input],
) -> Result<(), DependencyError> {
    for (dependency, hash) in inputs {
        let node = section.add_child(DEPENDENCY_TAG);
        let type_attr = node.add_child(TYPE_TAG);
        type_attr.set_attribute(true);
        type_attr.set_uint(dependency.dependency_type() as u32);
        let hash_attr = node.add_child(HASH_TAG);
        hash_attr.set_attribute(true);
        hash_attr.set_uint64(*hash);
        dependency.serialise_out(node)?;
    }
    Ok(())
}

fn serialise_inputs_in(section: &DataSection) -> Result<Vec<Input>, DependencyError> {
    let mut inputs = Vec::new();
    for i in 0..section.count_children() {
        let node = section
            .open_child(i)
            .ok_or(DependencyError("missing dependency"))?;
        // A malformed node without a Type/Hash is skipped (the reference
        // ignores such children rather than failing the whole read).
        let type_attr = match node.find_child(TYPE_TAG) {
            Some(t) if t.is_attribute() => t,
            _ => continue,
        };
        let hash_attr = match node.find_child(HASH_TAG) {
            Some(h) if h.is_attribute() => h,
            _ => continue,
        };
        let type_value = type_attr.as_uint(crate::dependency::DependencyType::INVALID);
        let dep_type = match crate::dependency::DependencyType::from_u32(type_value) {
            Some(t) => t,
            // Unknown type: ignore the node (the reference `continue`s).
            None => continue,
        };
        let hash = hash_attr.as_uint64(0);
        let dependency = Dependency::serialise_in(node, dep_type)?;
        inputs.push((dependency, hash));
    }
    Ok(inputs)
}

fn serialise_outputs_out(
    section: &mut DataSection,
    outputs: &[Output],
) -> Result<(), DependencyError> {
    for (file, hash) in outputs {
        let node = section.add_child(COMPILED_TAG);
        let file_attr = node.add_child(FILE_TAG);
        file_attr.set_attribute(true);
        file_attr.set_string(file);
        let hash_attr = node.add_child(HASH_TAG);
        hash_attr.set_attribute(true);
        hash_attr.set_uint64(*hash);
    }
    Ok(())
}

fn serialise_outputs_in(section: &DataSection) -> Result<Vec<Output>, DependencyError> {
    let mut outputs = Vec::new();
    for i in 0..section.count_children() {
        let node = section
            .open_child(i)
            .ok_or(DependencyError("missing output"))?;
        let file_attr = match node.find_child(FILE_TAG) {
            Some(f) if f.is_attribute() => f,
            _ => continue,
        };
        let hash_attr = match node.find_child(HASH_TAG) {
            Some(h) if h.is_attribute() => h,
            _ => continue,
        };
        outputs.push((file_attr.as_string().to_string(), hash_attr.as_uint64(0)));
    }
    Ok(outputs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dependency::{ConverterDependency, ConverterParamsDependency, SourceFileDependency};

    /// A deterministic hash provider: files hash via a lookup table (so the
    /// test can "modify" a file), the converter id is its own hash, and the
    /// params hash their own string.
    #[derive(Clone, Debug, Default)]
    struct MockHasher {
        files: std::collections::HashMap<String, u64>,
    }

    impl MockHasher {
        fn with_file(mut self, name: &str, hash: u64) -> Self {
            self.files.insert(name.to_string(), hash);
            self
        }
    }

    impl HashProvider for MockHasher {
        fn get_hash(&self, dependency: &Dependency) -> u64 {
            match dependency {
                Dependency::SourceFile(d) => self.files.get(d.file_name()).copied().unwrap_or(0),
                Dependency::IntermediateFile(d) => {
                    self.files.get(d.file_name()).copied().unwrap_or(0)
                }
                Dependency::OutputFile(d) => self.files.get(d.file_name()).copied().unwrap_or(0),
                Dependency::Converter(d) => {
                    let mut seed = d.converter_id();
                    hash64::combine(&mut seed, hash64::compute_str(d.converter_version()));
                    seed
                }
                Dependency::ConverterParams(d) => hash64::compute_str(d.converter_params()),
                Dependency::Directory(d) => hash64::compute_str(d.directory()),
            }
        }
    }

    /// The primary hashes are computed at add time and the combined input
    /// hash changes when an input file's content hash changes.
    #[test]
    fn primary_input_hash_tracks_source_content() {
        let mut list = DependencyList::new(MockHasher::default().with_file("a.model", 100));
        list.initialise("a.model", 1, "1.0", "");
        let before = list.get_input_hash(false);
        // Modify the only source: the combined hash must change.
        list.reset();
        let mut list = DependencyList::new(MockHasher::default().with_file("a.model", 101));
        list.initialise("a.model", 1, "1.0", "");
        let after = list.get_input_hash(false);
        assert_ne!(before, after);
    }

    /// Unchanged inputs yield a stable combined hash (cache reuse key).
    #[test]
    fn unchanged_inputs_stable_hash() {
        let build = || {
            let mut list = DependencyList::new(MockHasher::default().with_file("a.model", 100));
            list.initialise("a.model", 1, "1.0", "");
            list.get_input_hash(false)
        };
        assert_eq!(build(), build());
    }

    /// Primary inputs are always included; secondary inputs only when asked.
    #[test]
    fn secondary_inputs_toggle_included() {
        let mut list = DependencyList::new(MockHasher::default());
        list.initialise("a.model", 1, "1.0", "");
        list.add_secondary_source_file_dependency("tex.png", true);
        let with_secondary = list.get_input_hash(true);
        let without_secondary = list.get_input_hash(false);
        assert_ne!(with_secondary, without_secondary);
    }

    /// The serialised structure matches the reference: four sections in
    /// order, `Dependency` nodes with `Type`/`Hash` before the concrete
    /// children, `Compiled` nodes with `File`/`Hash`.
    #[test]
    fn serialise_round_trip_preserves_data() {
        let mut list = DependencyList::new(MockHasher::default().with_file("a.model", 42));
        list.initialise("a.model", 0xABC12345, "1.0", "-x");
        list.add_secondary_source_file_dependency("b.tex", true);
        list.add_intermediate_output("a.model.tmp", 7);
        list.add_output("a.model", 42);

        let mut section = DataSection::new("DependencyList");
        list.serialise_out(&mut section).expect("serialise out");

        let mut back = DependencyList::new(MockHasher::default());
        back.serialise_in(&section).expect("serialise in");
        assert_eq!(back.primary_inputs().len(), list.primary_inputs().len());
        assert_eq!(back.secondary_inputs().len(), list.secondary_inputs().len());
        assert_eq!(back.intermediate_outputs(), list.intermediate_outputs());
        assert_eq!(back.outputs(), list.outputs());
        // The primary dependency data round-trips. `initialise` marks the
        // primary deps critical (the reference's `addPrimary*`), so the
        // round-tripped copies keep the flag.
        let mut source = Dependency::SourceFile(SourceFileDependency::new("a.model"));
        source.set_critical(true);
        let mut converter = Dependency::Converter(ConverterDependency::new(0xABC12345, "1.0"));
        converter.set_critical(true);
        let mut params = Dependency::ConverterParams(ConverterParamsDependency::new("-x"));
        params.set_critical(true);
        assert_eq!(back.primary_inputs()[0].0, source);
        assert_eq!(back.primary_inputs()[1].0, converter);
        assert_eq!(back.primary_inputs()[2].0, params);
    }

    /// A serialised input node has `Type` and `Hash` first, then the
    /// dependency's own children (`File`, `Critical`).
    #[test]
    fn input_node_child_order() {
        let mut list = DependencyList::new(MockHasher::default());
        list.add_primary_source_file_dependency("a.model");
        let mut section = DataSection::new("DependencyList");
        list.serialise_out(&mut section).unwrap();

        let primary = section.find_child(PRIMARY_INPUTS_TAG).unwrap();
        let node = primary.open_child(0).unwrap();
        let names: Vec<&str> = (0..node.count_children())
            .map(|i| node.open_child(i).unwrap().section_name())
            .collect();
        assert_eq!(names, vec!["Type", "Hash", "File", "Critical"]);
    }

    /// An unknown dependency type in a stored node is ignored (the reference
    /// `continue`s), so a newer writer's file still loads the known subset.
    #[test]
    fn unknown_input_type_is_skipped() {
        let mut list = DependencyList::new(MockHasher::default());
        list.add_primary_source_file_dependency("a.model");
        let mut section = DataSection::new("DependencyList");
        list.serialise_out(&mut section).unwrap();

        // Inject an unknown Type node into PrimaryInputs (child 0).
        let primary = section.child_mut(0).unwrap();
        let node = primary.add_child(DEPENDENCY_TAG);
        let t = node.add_child(TYPE_TAG);
        t.set_attribute(true);
        t.set_uint(99);
        let h = node.add_child(HASH_TAG);
        h.set_attribute(true);
        h.set_uint64(5);
        let c = node.add_child("Critical");
        c.set_attribute(true);
        c.set_bool(true);

        let mut back = DependencyList::new(MockHasher::default());
        back.serialise_in(&section).unwrap();
        // Only the known SourceFile node survives.
        assert_eq!(back.primary_inputs().len(), 1);
    }
}
