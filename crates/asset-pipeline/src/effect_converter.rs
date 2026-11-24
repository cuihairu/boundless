//! `EffectConverter` port
//! (`tools/asset_pipeline/converters/effect_converter/`): compiles `.fx`
//! effect sources into the `.fxo` zip containers the runtime loads
//! (`loadEffectBinary` → `openSection(fxoName).readBinary("effect")`).
//!
//! The D3DX preprocessor/compiler is Windows-bound, so two seams:
//! - `getIncludes` becomes a text-level `#include "..."` scan (the
//!   reference gets the list from the D3DX include callbacks during
//!   preprocessing);
//! - the compile itself sits behind the [`EffectCompilerFace`] trait; the
//!   shipped [`TextEffectCompiler`] face does no HLSL codegen — its payload
//!   is the source text headed by a line naming the selected macro options,
//!   so every macro combination yields distinct bytes.
//!
//! Everything around the seams is faithful: the `-debug` param read, the
//! macro combination enumeration (`numSettingCombinations` = the product of
//! the option counts, selection by modulo/division, `MF_ASSERT(index == 0)`),
//! `updateManagerInfix`, `fxoName`, and the fxo container (an erased-and-
//! rewritten zip with the `effect`, `hash` and per-include `depends`
//! children).

use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::rc::Rc;

use resources::bwresource::BwResource;
use resources::zip::ZipFileSystem;

use crate::compiler::Compiler;
use crate::converter::Converter;
use crate::dependency_list::DependencyList;

/// `EffectConverter::getTypeName`.
pub const TYPE_NAME: &str = "EffectConverter";

/// `EffectConverter::getTypeId` — `Hash64::compute( getTypeName() )`.
pub fn type_id() -> u64 {
    crate::hash64::compute_str(TYPE_NAME)
}

/// `EffectConverter::getVersion`.
pub const VERSION: &str = "2.7";

/// `Moo::EffectMacroSetting` (`effect_macro_setting.hpp`): one macro
/// switch with its option values; `active` is the selected option.
#[derive(Debug, Clone)]
pub struct EffectMacroSetting {
    name: String,
    options: Vec<String>,
    active: usize,
}

impl EffectMacroSetting {
    /// The reference reads its options from the graphics-settings manager;
    /// the port receives them at construction.
    pub fn new(name: &str, options: &[&str]) -> Self {
        Self {
            name: name.to_string(),
            options: options.iter().map(|option| option.to_string()).collect(),
            active: 0,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// `numMacroOptions`.
    pub fn num_macro_options(&self) -> usize {
        self.options.len()
    }

    /// `selectOption`.
    pub fn select_option(&mut self, option: usize) {
        self.active = option;
    }

    /// `activeOption`: the selected option's value.
    pub fn active_option(&self) -> &str {
        &self.options[self.active]
    }

    /// One option's value by index.
    pub fn option_value(&self, index: usize) -> &str {
        &self.options[index]
    }
}

/// `EffectMacroSetting::updateManagerInfix` + `EffectCompiler::fxoInfix`:
/// the infix is the concatenation of every setting's active option.
pub fn update_manager_infix(settings: &[EffectMacroSetting]) -> String {
    settings
        .iter()
        .map(|setting| setting.active_option())
        .collect()
}

/// `Moo::EffectHelpers::fxoName`: `removeExtension` plus the infix.
pub fn fxo_name(resource_id: &str, fxo_infix: &str) -> String {
    let file_name = resources::bwresource::remove_extension(resource_id);
    if fxo_infix.is_empty() {
        format!("{file_name}.fxo")
    } else {
        format!("{file_name}.{fxo_infix}.fxo")
    }
}

/// `EffectCompiler::getIncludes` port: a text-level `#include "name"`
/// scan. Includes resolve against the source's directory, the source itself
/// never lists (the reference skips it), duplicates collapse (the
/// reference's `src.unique()`).
pub fn scan_includes(resource_id: &str, text: &str) -> Vec<String> {
    let dir = match resource_id.rfind('/') {
        Some(at) => &resource_id[..at],
        None => "",
    };
    let mut includes = Vec::new();
    let mut seen = HashSet::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        let Some(rest) = trimmed.strip_prefix("#include") else {
            continue;
        };
        let rest = rest.trim_start();
        let Some(open) = rest.find('"') else {
            continue;
        };
        let Some(close) = rest[open + 1..].find('"') else {
            continue;
        };
        let name = &rest[open + 1..open + 1 + close];
        let resolved = if name.starts_with('/') || dir.is_empty() {
            name.to_string()
        } else {
            format!("{dir}/{name}")
        };
        if resolved == resource_id || !seen.insert(resolved.clone()) {
            continue;
        }
        includes.push(resolved);
    }
    includes
}

/// The compile seam (`Moo::EffectCompiler::compile` —
/// `D3DXCreateEffectCompilerFromFileW` + `CompileEffect` are D3DX-bound).
pub trait EffectCompilerFace {
    /// `compile`: the binary stored under the fxo's `effect` child.
    /// `macros` carries the selected `(name, option)` pairs; the flag is
    /// the converter's `-debug` read (`debugSymbols_` → `compileFlags_`).
    fn compile(
        &self,
        resource_id: &str,
        macros: &[(String, String)],
        debug_symbols: bool,
    ) -> Option<Vec<u8>>;

    /// `hashResource`: the digest stored under the fxo's `hash` child.
    fn hash_resource(&self, resource_id: &str) -> Option<String>;
}

/// The device-free face: no HLSL codegen — the payload is the source text
/// with a header naming the selected macro options; the hash is a std
/// hasher over the file bytes (the reference uses MD5).
pub struct TextEffectCompiler {
    resource: Rc<BwResource>,
}

impl TextEffectCompiler {
    pub fn new(resource: Rc<BwResource>) -> Self {
        Self { resource }
    }
}

impl EffectCompilerFace for TextEffectCompiler {
    fn compile(
        &self,
        resource_id: &str,
        macros: &[(String, String)],
        _debug_symbols: bool,
    ) -> Option<Vec<u8>> {
        let raw = self.resource.file_system().read_file(resource_id)?;
        let mut out = Vec::new();
        if !macros.is_empty() {
            let names: Vec<String> = macros
                .iter()
                .map(|(name, value)| format!("{name}={value}"))
                .collect();
            out.extend_from_slice(format!("// macros: {}\n", names.join(" ")).as_bytes());
        }
        out.extend_from_slice(&raw);
        Some(out)
    }

    fn hash_resource(&self, resource_id: &str) -> Option<String> {
        let raw = self.resource.file_system().read_file(resource_id)?;
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        raw.hash(&mut hasher);
        Some(format!("{:016x}", hasher.finish()))
    }
}

/// `EffectConverter`.
pub struct EffectConverter {
    resource: Rc<BwResource>,
    compiler_face: Rc<dyn EffectCompilerFace>,
    debug_symbols: bool,
    macro_settings: Vec<EffectMacroSetting>,
}

impl EffectConverter {
    /// The reference is constructed with its `params` string
    /// (`debugSymbols_ = params.find( "-debug" ) != npos`) and reads the
    /// macro settings from the global graphics-settings manager; the port
    /// receives both at construction.
    pub fn new(
        params: &str,
        resource: Rc<BwResource>,
        compiler_face: Rc<dyn EffectCompilerFace>,
        macro_settings: Vec<EffectMacroSetting>,
    ) -> Self {
        Self {
            resource,
            compiler_face,
            debug_symbols: params.contains("-debug"),
            macro_settings,
        }
    }

    /// The plugin's default face.
    pub fn with_default_face(
        params: &str,
        resource: Rc<BwResource>,
        macro_settings: Vec<EffectMacroSetting>,
    ) -> Self {
        let face = Rc::new(TextEffectCompiler::new(Rc::clone(&resource)));
        Self::new(params, resource, face, macro_settings)
    }
}

impl Converter for EffectConverter {
    /// `createDependencies`: the text-scan includes, each resolved and
    /// added as a primary source dependency.
    fn create_dependencies(
        &self,
        source_file: &str,
        compiler: &dyn Compiler,
        dependencies: &mut DependencyList<&dyn Compiler>,
    ) -> bool {
        let mut resource_id = source_file.to_string();
        compiler.resolve_relative_path(&mut resource_id);
        let Some(raw) = self.resource.file_system().read_file(&resource_id) else {
            eprintln!("Could not get includes from effect {source_file}");
            return false;
        };
        let Ok(text) = String::from_utf8(raw) else {
            eprintln!("Could not get includes from effect {source_file}");
            return false;
        };

        for include in scan_includes(&resource_id, &text) {
            let mut include = include;
            compiler.resolve_relative_path(&mut include);
            dependencies.add_primary_source_file_dependency(include);
        }
        true
    }

    /// `convert`: one fxo per macro combination.
    fn convert(
        &self,
        source_file: &str,
        compiler: &dyn Compiler,
        _intermediate_files: &mut Vec<String>,
        output_files: &mut Vec<String>,
    ) -> bool {
        let mut resource_id = source_file.to_string();
        compiler.resolve_relative_path(&mut resource_id);

        // The include list doubles as the container's depends children.
        let includes = self
            .resource
            .file_system()
            .read_file(&resource_id)
            .and_then(|raw| String::from_utf8(raw).ok())
            .map(|text| scan_includes(&resource_id, &text))
            .unwrap_or_default();

        let mut num_setting_combinations = 1usize;
        for setting in &self.macro_settings {
            num_setting_combinations *= setting.num_macro_options();
        }
        for i in 0..num_setting_combinations {
            // The reference's selectOption walk: option = index % count,
            // index /= count.
            let mut index = i;
            let mut macros = Vec::new();
            let mut infix = String::new();
            for setting in &self.macro_settings {
                let option = index % setting.num_macro_options();
                index /= setting.num_macro_options();
                macros.push((setting.name().to_string(), setting.options[option].clone()));
                infix.push_str(&setting.options[option]);
            }
            assert!(index == 0, "MF_ASSERT( index == 0 )");

            // updateManagerInfix + fxoName.
            let mut outputfile = fxo_name(source_file, &infix);
            compiler.resolve_output_path(&mut outputfile);
            self.resource.ensure_absolute_path_exists(&outputfile);

            let Some(payload) =
                self.compiler_face
                    .compile(&resource_id, &macros, self.debug_symbols)
            else {
                eprintln!("Failed to compiler effect {source_file} to {outputfile}");
                return false;
            };
            if !self.write_fxo(&outputfile, &payload, &includes) {
                return false;
            }
            output_files.push(outputfile);
        }
        true
    }
}

impl EffectConverter {
    /// The compile-tail container write.
    fn write_fxo(&self, outputfile: &str, payload: &[u8], includes: &[String]) -> bool {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        payload.hash(&mut hasher);
        let hash = format!("{:016x}", hasher.finish());
        write_fxo_container(
            &self.resource,
            outputfile,
            payload,
            &hash,
            includes,
            &*self.compiler_face,
        )
    }
}

/// The compile-tail container write: erase the old file, build the zip
/// (`effect`, `hash`, one `depends` per include with its `name` and `hash`
/// children) and save it.
pub fn write_fxo_container(
    resource: &BwResource,
    outputfile: &str,
    payload: &[u8],
    hash: &str,
    includes: &[String],
    face: &dyn EffectCompilerFace,
) -> bool {
    let mut fs = match ZipFileSystem::open("", ZipFileSystem::empty_archive()) {
        Some(fs) => fs,
        None => return false,
    };
    fs.write_to_structure("effect", payload.to_vec());
    fs.write_to_structure("hash", hash.as_bytes().to_vec());
    for include in includes {
        fs.write_to_structure("depends/name", include.clone().into_bytes());
        let dep_hash = face.hash_resource(include).unwrap_or_default();
        fs.write_to_structure("depends/hash", dep_hash.into_bytes());
    }
    resource
        .file_system()
        .write_file(outputfile, &fs.create_zip())
}

/// Open an fxo's zip container (the zip sniff inside `open_section` is the
/// resources crate's open later task).
fn open_fxo(resource: &BwResource, fxo_name: &str) -> Option<ZipFileSystem> {
    let raw = resource.file_system().read_file(fxo_name)?;
    ZipFileSystem::open("", raw)
}

/// `Moo::EffectHelpers::loadEffectBinary`: read the effect's fxo and hand
/// back its `effect` binary child. The reference walks the section tree
/// (`openSection(fxoName).readBinary("effect")`); the port opens the
/// container through the zip face.
pub fn load_effect_binary(resource: &BwResource, resource_id: &str) -> Option<Vec<u8>> {
    let zip = open_fxo(resource, &fxo_name(resource_id, ""))?;
    zip.read_file("effect")
}

/// `Moo::EffectCompiler::checkModified`: does the effect need a rebuild?
///
/// The reference's paths, in order: no source hash (`.fx` unreadable) →
/// `false` whether or not the fxo exists (the runtime falls back to the
/// fxo); no fxo → `true`; no `hash` child → `true`; stored digest mismatch
/// → `true`; no `effect` child → `true`; any include's stored hash mismatch
/// → `true`; else `false`.
pub fn check_modified(
    resource: &BwResource,
    face: &dyn EffectCompilerFace,
    resource_id: &str,
    fxo_infix: &str,
) -> bool {
    let Some(res_digest) = face.hash_resource(resource_id) else {
        // If the .fx doesn't exist, whether the fxo exists or not the
        // answer stays "not modified".
        return false;
    };

    let fxo_name = fxo_name(resource_id, fxo_infix);
    let Some(zip) = open_fxo(resource, &fxo_name) else {
        return true; // fxo doesn't exist, build it.
    };

    let Some(stored_hash) = zip.read_file("hash") else {
        return true; // must be an old fxo, rebuild it.
    };
    let Ok(stored_hash) = String::from_utf8(stored_hash) else {
        return true; // bad data, rebuild it.
    };
    if stored_hash != res_digest {
        return true; // hash digest mismatch, rebuild it.
    }

    if zip.read_file("effect").is_none() {
        return true; // force recompile for old file
    }

    // The dependencies: the `depends` directory lists `name`/`hash` pairs
    // (repeat rows carry the duplicate encoding's ` <n>` suffix). Reads go
    // by position — `read_file` resolves duplicates to the last row.
    let Some(listing) = zip.read_directory("depends") else {
        return false;
    };
    for (index, name_entry) in listing.iter().enumerate() {
        if !name_entry.starts_with("name") {
            continue;
        }
        let suffix = &name_entry["name".len()..];
        let Some(hash_index) = listing
            .iter()
            .position(|entry| *entry == format!("hash{suffix}"))
        else {
            return true;
        };
        let Some(dep_name) = zip.read_file_at("depends", index) else {
            return true;
        };
        let Ok(dep_name) = String::from_utf8(dep_name) else {
            return true;
        };
        let Some(dep_hash) = zip.read_file_at("depends", hash_index) else {
            return true;
        };
        let Ok(dep_hash) = String::from_utf8(dep_hash) else {
            return true;
        };
        match face.hash_resource(&dep_name) {
            Some(current) if current == dep_hash => {}
            _ => return true,
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dependency::Dependency;
    use std::fs;
    use std::path::PathBuf;

    struct StubCompiler;

    impl Compiler for StubCompiler {
        fn get_resource_paths(&self) -> &[String] {
            &[]
        }
        fn get_hash(&self, _dependency: &Dependency) -> u64 {
            0
        }
        fn get_file_hash(&self, _file_name: &str, _force_update: bool) -> u64 {
            0
        }
        fn get_directory_hash(
            &self,
            _directory: &str,
            _pattern: &str,
            _regex: bool,
            _recursive: bool,
        ) -> u64 {
            0
        }
        fn check_file_hash_up_to_date(&self, _file_name: &str) -> bool {
            true
        }
        fn ensure_up_to_date(
            &self,
            _dependency: &Dependency,
        ) -> (bool, Option<crate::conversion_task::TaskHandle>) {
            (true, None)
        }
        fn get_source_file(&self, _file: &str) -> Option<String> {
            None
        }
        fn queue_task(&self, _task: crate::conversion_task::TaskHandle) {}
        fn get_next_task(&self) -> Option<crate::conversion_task::TaskHandle> {
            None
        }
        fn has_tasks(&self) -> bool {
            false
        }
        fn set_error(&self) {}
        fn set_warning(&self) {}
        fn reset_error_flags(&self) {}
        fn has_error(&self) -> bool {
            false
        }
        fn has_warning(&self) -> bool {
            false
        }
        fn resolve_relative_path(&self, _path: &mut String) -> bool {
            true
        }
        fn resolve_source_path(&self, _path: &mut String) -> bool {
            true
        }
        fn resolve_intermediate_path(&self, _path: &mut String) -> bool {
            true
        }
        fn resolve_output_path(&self, _path: &mut String) -> bool {
            true
        }
        fn cache_read(&self, _file_name: &str, _hash: u64) -> bool {
            false
        }
        fn cache_write(&self, _file_name: &str, _hash: u64) -> bool {
            false
        }
        fn should_iterate_file(&self, _file: &str) -> bool {
            true
        }
        fn should_iterate_directory(&self, _directory: &str) -> bool {
            true
        }
    }

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("fxconv-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_file(path: &std::path::Path, contents: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    /// fxoName carries the infix; scan_includes resolves, skips the source
    /// itself and dedupes.
    #[test]
    fn fxo_name_and_include_scan_follow_the_reference() {
        assert_eq!(fxo_name("shaders/tree.fx", ""), "shaders/tree.fxo");
        assert_eq!(fxo_name("shaders/tree.fx", "high"), "shaders/tree.high.fxo");

        let text = "#include \"common.h\"\n#include <stdio.h>\n#include \"common.h\"\n#include \"sub/extra.h\"\n#include \"tree.fx\"\n";
        assert_eq!(
            scan_includes("shaders/tree.fx", text),
            vec![
                "shaders/common.h".to_string(),
                "shaders/sub/extra.h".to_string(),
            ]
        );
        // A root-level source has no directory to join.
        assert_eq!(
            scan_includes("root.fx", "#include \"leaf.h\"\n"),
            vec!["leaf.h".to_string()]
        );
        // updateManagerInfix concatenates the active options.
        let settings = vec![
            EffectMacroSetting::new("QUALITY", &["low", "high"]),
            EffectMacroSetting::new("SHADOWS", &["off", "on"]),
        ];
        assert_eq!(update_manager_infix(&settings), "lowoff");
    }

    /// The round trip: `.fx` → `.fxo` per macro combination, each fxo
    /// reading back through `readBinary("effect")` like `loadEffectBinary`.
    #[test]
    fn convert_round_trips_fx_through_the_fxo_container() {
        let root = temp_root("roundtrip");
        write_file(
            &root.join("shaders/common.h"),
            "// shared\n#define MAX_LIGHTS 4\n",
        );
        write_file(
            &root.join("shaders/tree.fx"),
            "#include \"common.h\"\nfloat4 main() : COLOR0 { return 0; }\n",
        );
        let mut resource = BwResource::init_paths(&[root.to_str().unwrap()]);
        resource.enable_editor_default_root();
        let resource = Rc::new(resource);

        let settings = vec![
            EffectMacroSetting::new("QUALITY", &["low", "high"]),
            EffectMacroSetting::new("SHADOWS", &["off", "on"]),
        ];
        let converter = EffectConverter::with_default_face("", Rc::clone(&resource), settings);
        let compiler = StubCompiler;
        let mut outputs = Vec::new();
        assert!(converter.convert("shaders/tree.fx", &compiler, &mut Vec::new(), &mut outputs));
        assert_eq!(
            outputs,
            vec![
                "shaders/tree.lowoff.fxo".to_string(),
                "shaders/tree.highoff.fxo".to_string(),
                "shaders/tree.lowon.fxo".to_string(),
                "shaders/tree.highon.fxo".to_string(),
            ]
        );

        // loadEffectBinary: open each fxo, read its "effect" child.
        let source = fs::read(root.join("shaders/tree.fx")).unwrap();
        for name in &outputs {
            let payload = load_effect_binary(&resource, name).unwrap();
            let text = String::from_utf8(payload).unwrap();
            let (header, body) = text.split_once('\n').unwrap();
            assert!(header.starts_with("// macros: QUALITY="), "{name}");
            assert!(body.as_bytes() == source, "{name} payload body");

            // The hash child and the depends children ride along.
            let raw = resource.file_system().read_file(name).unwrap();
            let zip = ZipFileSystem::open("", raw).unwrap();
            assert!(zip.read_file("hash").is_some());
            assert_eq!(
                zip.read_file("depends/name").unwrap(),
                b"shaders/common.h".to_vec(),
            );
        }

        // Two combinations differ in their bytes (the macro enumeration is
        // observable).
        let lowoff = load_effect_binary(&resource, "shaders/tree.lowoff.fxo").unwrap();
        let highon = load_effect_binary(&resource, "shaders/tree.highon.fxo").unwrap();
        assert_ne!(lowoff, highon);
    }

    /// `-debug` flips the flag; a missing source fails both faces; zero
    /// settings produce exactly one infix-less fxo.
    #[test]
    fn debug_flag_and_failure_paths() {
        let root = temp_root("flags");
        write_file(&root.join("solo.fx"), "void main() {}\n");
        let mut resource = BwResource::init_paths(&[root.to_str().unwrap()]);
        resource.enable_editor_default_root();
        let resource = Rc::new(resource);

        let converter =
            EffectConverter::with_default_face("-debug", Rc::clone(&resource), Vec::new());
        assert!(converter.debug_symbols);

        let compiler = StubCompiler;
        let compiler_dyn: &dyn Compiler = &compiler;
        // Empty settings: one combination, no infix.
        let mut outputs = Vec::new();
        assert!(converter.convert("solo.fx", &compiler, &mut Vec::new(), &mut outputs));
        assert_eq!(outputs, vec!["solo.fxo".to_string()]);

        // A missing source fails both faces.
        let mut dependencies: DependencyList<&dyn Compiler> = DependencyList::new(compiler_dyn);
        assert!(!converter.create_dependencies("gone.fx", &compiler, &mut dependencies));
        assert!(dependencies.primary_inputs().is_empty());
        let mut outputs = Vec::new();
        assert!(!converter.convert("gone.fx", &compiler, &mut Vec::new(), &mut outputs));

        // The include scan face: a source with includes adds them as
        // primary dependencies.
        write_file(
            &root.join("withinc.fx"),
            "#include \"solo.fx\"\nvoid main() {}\n",
        );
        let mut dependencies: DependencyList<&dyn Compiler> = DependencyList::new(compiler_dyn);
        assert!(converter.create_dependencies("withinc.fx", &compiler, &mut dependencies));
        assert_eq!(dependencies.primary_inputs().len(), 1);
        assert!(matches!(&dependencies.primary_inputs()[0].0,
            Dependency::SourceFile(dep) if dep.file_name() == "solo.fx"));
    }
}
