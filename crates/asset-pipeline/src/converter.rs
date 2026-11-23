//! `Converter` port (`conversion/converter.hpp`): the plugin interface the
//! asset pipeline drives to build a source file's dependency list and to
//! convert the source into intermediate and output files.
//!
//! Reference quirks preserved:
//! - a converter is constructed with a `params` string (from the task's
//!   `converterParams_`), which the converter owns for its lifetime;
//! - `createDependencies` fills a `DependencyList&` in place and the
//!   processor initialises the primary dependencies (source, converter,
//!   params) before calling it — the converter adds the secondary ones;
//! - `convert` reports its products through the `intermediateFiles` and
//!   `outputFiles` vectors (relative paths the compiler resolves);
//! - both methods report success/failure with a `bool` (the processor also
//!   treats an error/warning flag raised on the compiler as a failure).

use crate::compiler::Compiler;
use crate::dependency_list::DependencyList;

/// A plugin object that generates dependencies of assets and converts assets.
pub trait Converter {
    /// `createDependencies( sourcefile, compiler, dependencies )`: build the
    /// dependency list for a source file.
    fn create_dependencies(
        &self,
        source_file: &str,
        compiler: &dyn Compiler,
        dependencies: &mut DependencyList<&dyn Compiler>,
    ) -> bool;

    /// `convert( sourcefile, compiler, intermediateFiles, outputFiles )`:
    /// convert a source file, reporting the files it generated.
    fn convert(
        &self,
        source_file: &str,
        compiler: &dyn Compiler,
        intermediate_files: &mut Vec<String>,
        output_files: &mut Vec<String>,
    ) -> bool;
}
