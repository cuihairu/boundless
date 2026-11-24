//! `_AssetProcessor` port
//! (`tools/assetprocessor/asset_processor_script.cpp`): the tool DLL the
//! client's asset workflow drives through Python. The port exposes the
//! same function set as plain functions — the PyScript wrapper, the window
//! plumbing and the DllMain lifecycle are the seam.
//!
//! Fully portable faces: the chunk-grid identifier maths, the GUID format,
//! the base64/wide-string conversions and the shader workflow over the
//! [`EffectCompilerFace`] seam (validation errors message-exact). The Moo
//! visual/vertices internals — `updateBSPVersion`'s mesh→BSP
//! regeneration and `upgradeHardskinnedVertices` — wait on the physics
//! tree-builder and vertices-runtime faces.

use foundation::math::Vector3;
use resources::bwresource::BwResource;
use resources::datasection::DataSection;
use resources::zip::ZipFileSystem;

use crate::effect_converter::{
    EffectCompilerFace, EffectMacroSetting, check_modified, fxo_name, scan_includes,
    write_fxo_container,
};

/// `AssetProcessorScript::baseName`: the name up to its last extension
/// dot, backslashes normalised to slashes.
pub fn base_name(resource_id: &str) -> String {
    let cut = resource_id.rfind('.').unwrap_or(resource_id.len());
    resource_id[..cut].replace('\\', "/")
}

/// `DEFAULT_GRID_RESOLUTION` (`chunk_grid_size.hpp`).
pub const DEFAULT_GRID_RESOLUTION: f32 = 100.0;

/// `outsideChunkIdentifier`: the identifier of the chunk owning a world
/// position — `%04x%04xo`, prefixed by one `…/sep/` level per grid axis
/// whose u16 coordinate trips that level's wraparound window.
pub fn outside_chunk_identifier(position: Vector3, chunk_size: f32) -> String {
    let grid_x = (position.x / chunk_size).floor() as i32;
    let grid_z = (position.z / chunk_size).floor() as i32;
    let gridxs = grid_x as u16;
    let gridzs = grid_z as u16;

    let mut identifier = String::new();
    if gridxs.wrapping_add(4096) >= 8192 || gridzs.wrapping_add(4096) >= 8192 {
        identifier += &format!("{:01x}xxx{:01x}xxx/sep/", gridxs >> 12, gridzs >> 12);
    }
    if gridxs.wrapping_add(256) >= 512 || gridzs.wrapping_add(256) >= 512 {
        identifier += &format!("{:02x}xxx{:02x}xxx/sep/", gridxs >> 8, gridzs >> 8);
    }
    if gridxs.wrapping_add(16) >= 32 || gridzs.wrapping_add(16) >= 32 {
        identifier += &format!("{:03x}xx{:03x}xx/sep/", gridxs >> 4, gridzs >> 4);
    }
    identifier += &format!("{:04x}{:04x}o", gridxs, gridzs);
    identifier
}

/// `getGuid`: a fresh `UniqueID` in the `%08X.%08X.%08X.%08X` form. The
/// reference seeds from the OS GUID source; the port mixes the clock
/// through SplitMix64.
pub fn get_guid() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let mut state = nanos ^ u64::from_le_bytes((std::process::id() as u64).to_le_bytes());
    let mut next = move || {
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    };
    let (a, b, c, d) = (next() as u32, next() as u32, next() as u32, next() as u32);
    format!("{a:08X}.{b:08X}.{c:08X}.{d:08X}")
}

/// `decodeWideString`: a `!`-prefixed value is literal (bang stripped);
/// otherwise base64 → UTF-16LE. Undecodable values become `"error"`.
pub fn decode_wide_string(val: &str) -> String {
    if let Some(rest) = val.strip_prefix('!') {
        return rest.to_string();
    }
    let Some(bytes) = resources::base64::decode_strict(val) else {
        return "error".to_string();
    };
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .collect();
    String::from_utf16_lossy(&units)
}

/// `convertBase64DataSection`: decode the section's string in place;
/// `true` when the value decoded (the legacy base64 → text upgrade).
pub fn convert_base64_data_section(section: &mut DataSection) -> bool {
    let val = decode_wide_string(section.as_string());
    if val != "error" {
        section.set_string(&val);
        return true;
    }
    false
}

/// `convertVisual`: Collada is no longer a supported format — the
/// reference's `NotImplementedError`, preserved.
pub fn convert_visual(_visual_file: &str, _collada_file: &str) -> Result<(), &'static str> {
    Err(" Collada is no longer a supported format.")
}

/// `getShaderMacros`: `(name, option count)` pairs.
pub fn get_shader_macros(settings: &[EffectMacroSetting]) -> Vec<(String, usize)> {
    settings
        .iter()
        .map(|setting| (setting.name().to_string(), setting.num_macro_options()))
        .collect()
}

/// The shared selection walk behind `compileShader` /
/// `shaderNeedsRecompile`: one option index per setting, concatenated into
/// the fxo infix (`selectOption` + `updateManagerInfix`).
fn select_combination(
    macro_settings: &[EffectMacroSetting],
    macro_combination: &[usize],
) -> (Vec<(String, String)>, String) {
    let mut macros = Vec::new();
    let mut infix = String::new();
    for (setting, index) in macro_settings.iter().zip(macro_combination) {
        // The reference hands the raw index to selectOption; a
        // out-of-range index clamps to the last option here.
        let option = (*index).min(setting.num_macro_options().saturating_sub(1));
        macros.push((
            setting.name().to_string(),
            setting.option_value(option).to_string(),
        ));
        infix.push_str(setting.option_value(option));
    }
    (macros, infix)
}

/// The include list of a source (the container's depends children ride on
/// the same scan).
fn source_includes(resource: &BwResource, resource_id: &str) -> Vec<String> {
    resource
        .file_system()
        .read_file(resource_id)
        .and_then(|raw| String::from_utf8(raw).ok())
        .map(|text| scan_includes(resource_id, &text))
        .unwrap_or_default()
}

/// `compileShader`: validate the combination, then compile when `force` or
/// the fxo is out of date. Returns `(compiled, message)`; `Err` is the
/// validation error (the reference sets it as the Python error).
#[allow(clippy::too_many_arguments)]
pub fn compile_shader(
    resource: &BwResource,
    face: &dyn EffectCompilerFace,
    macro_settings: &[EffectMacroSetting],
    resource_id: &str,
    macro_combination: &[usize],
    force: bool,
) -> Result<(bool, String), &'static str> {
    if macro_combination.len() != macro_settings.len() {
        return Err(
            "compileShader macroCombination tuple is the wrong length. Must be len(getShaderMacros).",
        );
    }

    // selectOption + updateManagerInfix.
    let (macros, infix) = select_combination(macro_settings, macro_combination);

    let mut compile_result = String::new();
    let mut success = false;
    // The reference's `EffectCompiler compiler( true )` — purge-on-compile,
    // no debug symbols.
    if force || check_modified(resource, face, resource_id, &infix) {
        let outputfile = fxo_name(resource_id, &infix);
        let includes = source_includes(resource, resource_id);
        match face.compile(resource_id, &macros, false) {
            Some(payload) => {
                // The compile tail stores the digest of the SOURCE (the
                // reference's hashResource before compiling).
                let hash = face.hash_resource(resource_id).unwrap_or_default();
                if write_fxo_container(resource, &outputfile, &payload, &hash, &includes, face) {
                    success = true;
                    compile_result = "Compilation successful.".to_string();
                } else {
                    compile_result = "Unable to compile effect.".to_string();
                }
            }
            None => {
                compile_result = "Unable to create effect compiler.".to_string();
            }
        }
    }
    Ok((success, compile_result))
}

/// `shaderNeedsRecompile`: validate the combination, then `checkModified`
/// for the selected combination's fxo.
pub fn shader_needs_recompile(
    resource: &BwResource,
    face: &dyn EffectCompilerFace,
    macro_settings: &[EffectMacroSetting],
    resource_id: &str,
    macro_combination: &[usize],
) -> Result<bool, &'static str> {
    if macro_combination.len() != macro_settings.len() {
        return Err(
            "shaderNeedsRecompile macroCombination tuple is the wrong length. Must be len(getShaderMacros).",
        );
    }
    let (_, infix) = select_combination(macro_settings, macro_combination);
    Ok(check_modified(resource, face, resource_id, &infix))
}

/// The runtime read behind the workflow's verification: open the infix-named
/// fxo and hand back its `effect` child.
pub fn load_compiled_effect(
    resource: &BwResource,
    resource_id: &str,
    fxo_infix: &str,
) -> Option<Vec<u8>> {
    let raw = resource
        .file_system()
        .read_file(&fxo_name(resource_id, fxo_infix))?;
    let zip = ZipFileSystem::open("", raw)?;
    zip.read_file("effect")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect_converter::TextEffectCompiler;
    use std::fs;
    use std::path::PathBuf;
    use std::rc::Rc;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("aproc-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_file(path: &std::path::Path, contents: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn make_fixture(tag: &str) -> (PathBuf, Rc<BwResource>) {
        let root = temp_root(tag);
        write_file(
            &root.join("shaders/tree.fx"),
            "#include \"common.h\"\nfloat4 main() : COLOR0 { return 0; }\n",
        );
        write_file(&root.join("shaders/common.h"), "#define MAX_LIGHTS 4\n");
        let mut resource = BwResource::init_paths(&[root.to_str().unwrap()]);
        resource.enable_editor_default_root();
        (root, Rc::new(resource))
    }

    fn macro_settings() -> Vec<EffectMacroSetting> {
        vec![
            EffectMacroSetting::new("QUALITY", &["low", "high"]),
            EffectMacroSetting::new("SHADOWS", &["off", "on"]),
        ]
    }

    /// The grid identifier: plain cells, each sep level, negatives and a
    /// custom resolution.
    #[test]
    fn outside_chunk_identifier_matches_the_reference_maths() {
        let at = |x: f32, z: f32| outside_chunk_identifier(Vector3::new(x, 0.0, z), 100.0);

        assert_eq!(at(0.0, 0.0), "00000000o");
        assert_eq!(at(250.0, 250.0), "00020002o");
        // x grid 16 trips the third level.
        assert_eq!(at(1650.0, 20.0), "001xx000xx/sep/00100000o");
        // x grid 4096 trips all three levels.
        assert_eq!(
            at(409_600.0, 20.0),
            "1xxx0xxx/sep/10xxx00xxx/sep/100xx000xx/sep/10000000o"
        );
        // Negative grids truncate into the u16 top.
        assert_eq!(at(-100.0, -100.0), "ffffffffo");
        // A custom resolution rescales the grid.
        assert_eq!(
            outside_chunk_identifier(Vector3::new(5.0, 0.0, 5.0), 5.0),
            "00010001o"
        );
    }

    /// The GUID format is four upper-case hex words; fresh calls differ.
    #[test]
    fn guid_format_is_dotted_hex() {
        let first = get_guid();
        let words: Vec<&str> = first.split('.').collect();
        assert_eq!(words.len(), 4);
        for word in &words {
            assert_eq!(word.len(), 8);
            assert!(word.chars().all(|c| c.is_ascii_hexdigit()));
        }
        assert_ne!(get_guid(), first);
    }

    /// Base64 → UTF-16LE decode, the bang passthrough and the error value.
    #[test]
    fn decode_wide_string_round_trips() {
        // "Hi" in UTF-16LE, base64-encoded.
        let encoded = resources::base64::encode(b"H\x00i\x00");
        assert_eq!(decode_wide_string(&encoded), "Hi");
        // The bang marks a hand-edited literal; the bang is stripped.
        assert_eq!(decode_wide_string("!shaders/common"), "shaders/common");
        // Undecodable values report the sentinel.
        assert_eq!(decode_wide_string("???"), "error");
        // A trailing odd byte is dropped (len / sizeof(wchar_t)).
        let odd = resources::base64::encode(&[0x48, 0x00, 0x69, 0x00, 0x21]);
        assert_eq!(decode_wide_string(&odd), "Hi");
    }

    /// The data-section conversion replaces decoded values in place.
    #[test]
    fn convert_base64_converts_in_place() {
        let encoded = resources::base64::encode(&[0x48, 0x00, 0x69, 0x00]);
        let mut section = DataSection::new("desc");
        section.set_string(&encoded);
        assert!(convert_base64_data_section(&mut section));
        assert_eq!(section.as_string(), "Hi");
        assert!(section.as_string().contains("Hi"));

        // The sentinel stays put and reports false.
        let mut section = DataSection::new("desc");
        section.set_string("???");
        assert!(!convert_base64_data_section(&mut section));
        assert_eq!(section.as_string(), "???");
    }

    /// The shader workflow: macro shape, exact validation errors, compile,
    /// staleness checks and the fxo read-back.
    #[test]
    fn shader_workflow_compiles_and_tracks_staleness() {
        let (_root, resource) = make_fixture("shader");
        let face = TextEffectCompiler::new(Rc::clone(&resource));
        let settings = macro_settings();

        assert_eq!(
            get_shader_macros(&settings),
            vec![
                ("QUALITY".to_string(), 2usize),
                ("SHADOWS".to_string(), 2usize),
            ]
        );

        // Validation errors, message-exact.
        assert_eq!(
            compile_shader(&resource, &face, &settings, "shaders/tree.fx", &[1], true),
            Err(
                "compileShader macroCombination tuple is the wrong length. Must be len(getShaderMacros)."
            )
        );
        assert_eq!(
            shader_needs_recompile(&resource, &face, &settings, "shaders/tree.fx", &[1, 2, 3]),
            Err(
                "shaderNeedsRecompile macroCombination tuple is the wrong length. Must be len(getShaderMacros)."
            )
        );

        // No fxo yet: needs a recompile, and a forced compile lands the
        // infix-named container.
        let combination = [1usize, 1usize]; // high, on
        assert_eq!(
            shader_needs_recompile(&resource, &face, &settings, "shaders/tree.fx", &combination),
            Ok(true)
        );
        let (compiled, message) = compile_shader(
            &resource,
            &face,
            &settings,
            "shaders/tree.fx",
            &combination,
            true,
        )
        .unwrap();
        assert!(compiled);
        assert_eq!(message, "Compilation successful.");

        let payload = load_compiled_effect(&resource, "shaders/tree.fx", "highon").unwrap();
        let source = fs::read(_root.join("shaders/tree.fx")).unwrap();
        assert!(payload.ends_with(&source));

        // Freshly compiled: no recompile needed. Touching the source (or
        // the include) flips it back.
        assert_eq!(
            shader_needs_recompile(&resource, &face, &settings, "shaders/tree.fx", &combination),
            Ok(false)
        );
        write_file(&_root.join("shaders/common.h"), "#define MAX_LIGHTS 8\n");
        assert_eq!(
            shader_needs_recompile(&resource, &face, &settings, "shaders/tree.fx", &combination),
            Ok(true)
        );

        // Not forced + out of date → the compile runs again.
        let (compiled, message) = compile_shader(
            &resource,
            &face,
            &settings,
            "shaders/tree.fx",
            &combination,
            false,
        )
        .unwrap();
        assert!(compiled);
        assert_eq!(message, "Compilation successful.");

        // The other combination's fxo does not exist yet.
        assert_eq!(
            shader_needs_recompile(&resource, &face, &settings, "shaders/tree.fx", &[0, 0]),
            Ok(true)
        );
    }

    /// The quirks: a missing source reports "not modified", and
    /// `convertVisual` always refuses.
    #[test]
    fn missing_source_and_convert_visual_quirks() {
        let (_root, resource) = make_fixture("quirks");
        let face = TextEffectCompiler::new(Rc::clone(&resource));
        let settings = macro_settings();

        // .fx missing: checkModified answers false with or without a fxo.
        assert_eq!(
            shader_needs_recompile(&resource, &face, &settings, "shaders/gone.fx", &[0, 0]),
            Ok(false)
        );

        assert_eq!(
            convert_visual("any.visual", "any.dae"),
            Err(" Collada is no longer a supported format.")
        );
    }
}
