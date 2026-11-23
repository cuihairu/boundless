//! The `tools/process_defs` command-line contract (BW-04-04-01).
//!
//! Option extraction mirrors `getOption`/`hasFlag` in
//! `tools/process_defs/main.cpp`, the resource paths mirror the
//! `-r`/`--res` scan in `BWResource::init`, and the usage text plus the
//! exit codes are the stock binary's own behaviour, captured live on this
//! machine (`--help` -> usage on stderr and success; a load failure ->
//! "Failed to parse .def files" and failure; a successful dump -> success).

use crate::diagnostics::Diagnostics;
use crate::dump::load_and_dump;
use std::io::Write;
use std::path::PathBuf;

/// `PROCESS_NAME` in `main.cpp`.
pub const PROCESS_NAME: &str = "process_defs";

/// The `USAGE` text `printUsage()` writes to stderr, byte-equal to the stock
/// binary's `--help` output (note the trailing space after `in ` on the
/// `-p` line, present in the original).
pub const USAGE: &str = "usage: process_defs [OPTION] scriptArgs\n\nThis program parses the BigWorld entity definition files. A Python object is\nthen created representing the entity descriptions. This is passed to a Python\ncallback moduleName.functionName. By default, this is ProcessDefs.process.\n\nOptions:\n  -r             directory     Specifies a resource path. Can be used\n                               multiple times, in order of decreasing\n                               priority, replacing the paths provided in\n                               ~/.bwmachined.conf\n  -f, --function funcName      The Python function to call. \"process\" by\n                               default.\n  -m, --module   moduleName    The Python module to load. \"ProcessDefs\"\n                               by default.\n  -p, --path     directory     The directory to find the module.\n                               \"tools/process_defs/resources/scripts\" in \n                               the resource tree is the default.\n  --use-stdout                 Send messages to stdout rather then stderr\n  -v, --verbose                Displays more verbose output.\n  -h, --help                   Displays this message.";

/// `EXIT_SUCCESS`.
pub const EXIT_SUCCESS: i32 = 0;
/// `EXIT_FAILURE`.
pub const EXIT_FAILURE: i32 = 1;

const FLAGS_HELP: [&str; 2] = ["-h", "--help"];
const FLAGS_VERBOSE: [&str; 2] = ["-v", "--verbose"];
const FLAGS_PATH: [&str; 2] = ["-p", "--path"];
const FLAGS_MODULE: [&str; 2] = ["-m", "--module"];
const FLAGS_FUNCTION: [&str; 2] = ["-f", "--function"];
const FLAGS_USE_STDOUT: [&str; 1] = ["--use-stdout"];

/// Every option `main()` (and `BWResource::init`) understands, with the
/// reference defaults filled in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliOptions {
    /// `-m, --module` — `"ProcessDefs"` by default.
    pub module: String,
    /// `-f, --function` — `"process"` by default.
    pub function: String,
    /// `-p, --path` — `"."` by default (the code default; the usage text
    /// advertises the resource-tree scripts directory instead, as in the
    /// reference).
    pub path: String,
    /// `-h, --help`.
    pub help: bool,
    /// `-v, --verbose`.
    pub verbose: bool,
    /// `--use-stdout`.
    pub use_stdout: bool,
    /// Every `-r`/`--res` value, in the order given (decreasing priority).
    pub res_paths: Vec<String>,
    /// Whatever the scans did not consume: the reference hands these on as
    /// `sys.argv` script arguments.
    pub script_args: Vec<String>,
}

/// `getOption`: scan for any of `flags`, taking the following argument as the
/// value and removing both. Like the reference, the loop "deliberately stops
/// one short" — a flag in the last position can carry no value, so it is left
/// behind and the default survives.
fn get_option(args: &mut Vec<String>, flags: &[&str], default: &str) -> String {
    if args.len() >= 2
        && let Some(position) = args[..args.len() - 1]
            .iter()
            .position(|arg| flags.contains(&arg.as_str()))
    {
        let value = args[position + 1].clone();
        args.remove(position + 1);
        args.remove(position);
        return value;
    }
    default.to_string()
}

/// `hasFlag`: remove the first occurrence of any of `flags` and report it.
fn has_flag(args: &mut Vec<String>, flags: &[&str]) -> bool {
    let Some(position) = args.iter().position(|arg| flags.contains(&arg.as_str())) else {
        return false;
    };
    args.remove(position);
    true
}

/// The `-r`/`--res` scan from `BWResource::init`: collect every value in the
/// order given and remove the pairs. Like `getOption`, the scan stops one
/// short of the end.
fn extract_res_paths(args: &mut Vec<String>) -> Vec<String> {
    let mut paths = Vec::new();
    let mut position = 0;
    while position + 1 < args.len() {
        if args[position] == "-r" || args[position] == "--res" {
            paths.push(args[position + 1].clone());
            args.remove(position + 1);
            args.remove(position);
            continue;
        }
        position += 1;
    }
    paths
}

/// The `main()` option order: module, function, help, verbose,
/// `--use-stdout`, then the resource scan, then the module path. Whatever
/// survives is the script argument list.
pub fn parse_options(mut args: Vec<String>) -> CliOptions {
    let module = get_option(&mut args, &FLAGS_MODULE, "ProcessDefs");
    let function = get_option(&mut args, &FLAGS_FUNCTION, "process");
    let help = has_flag(&mut args, &FLAGS_HELP);
    let verbose = has_flag(&mut args, &FLAGS_VERBOSE);
    let use_stdout = has_flag(&mut args, &FLAGS_USE_STDOUT);
    let res_paths = extract_res_paths(&mut args);
    let path = get_option(&mut args, &FLAGS_PATH, ".");
    CliOptions {
        module,
        function,
        path,
        help,
        verbose,
        use_stdout,
        res_paths,
        script_args: args,
    }
}

/// The `main()` flow: the help branch prints the usage to stderr and always
/// succeeds; otherwise the definitions load and the dump — the Rust
/// counterpart of the Python callback — goes to stdout. A load failure
/// reports the parse stage and fails.
pub fn run(args: Vec<String>, out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    let options = parse_options(args);
    if options.help {
        let _ = writeln!(err, "{USAGE}");
        return EXIT_SUCCESS;
    }

    let root = PathBuf::from(options.res_paths.first().cloned().unwrap_or_default());
    let mut diag = Diagnostics::new();
    match load_and_dump(&root, &mut diag) {
        Ok(dump) => {
            let _ = out.write_all(dump.as_bytes());
            EXIT_SUCCESS
        }
        Err(_) => {
            let _ = writeln!(err, "Failed to parse .def files");
            EXIT_FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_options_match_the_reference_defaults_and_forms() {
        let options = parse_options(Vec::new());
        assert_eq!(options.module, "ProcessDefs");
        assert_eq!(options.function, "process");
        assert_eq!(options.path, ".");
        assert!(!options.help && !options.verbose && !options.use_stdout);
        assert!(options.res_paths.is_empty());

        let options = parse_options(
            [
                "--module",
                "M1",
                "-m",
                "M2",
                "--function",
                "F1",
                "-f",
                "F2",
                "--path",
                "P1",
                "-p",
                "P2",
                "--use-stdout",
                "-v",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
        );
        assert_eq!(options.module, "M1");
        assert_eq!(options.function, "F1");
        assert_eq!(options.path, "P1");
        assert!(options.verbose && options.use_stdout && !options.help);
        // Each scan removes only its FIRST matching pair; a second occurrence
        // survives into the script argument list, as in the reference.
        assert_eq!(options.script_args, ["-m", "M2", "-f", "F2", "-p", "P2"]);
    }

    #[test]
    fn the_value_hunt_is_blind_and_stops_one_short() {
        // `getOption` "deliberately stops one short", so a lone trailing flag
        // can carry no value: the default survives and the flag is left in
        // the arguments.
        let options = parse_options(["-m"].into_iter().map(String::from).collect());
        assert_eq!(options.module, "ProcessDefs");
        assert_eq!(options.script_args, ["-m"]);

        // The hunt itself is blind: any following argument becomes the value,
        // even another flag, and the help scan then finds nothing left.
        let options = parse_options(["-m", "-h"].into_iter().map(String::from).collect());
        assert_eq!(options.module, "-h");
        assert!(!options.help);
        assert!(options.script_args.is_empty());
    }

    #[test]
    fn has_flag_removes_one_occurrence_per_scan() {
        let options = parse_options(["-h", "--help"].into_iter().map(String::from).collect());
        assert!(options.help);
        assert_eq!(options.script_args, ["--help"]);
    }

    #[test]
    fn resource_paths_collect_in_the_order_given() {
        let options = parse_options(
            ["-r", "one", "--res", "two", "tail"]
                .into_iter()
                .map(String::from)
                .collect(),
        );
        assert_eq!(options.res_paths, ["one", "two"]);
        assert_eq!(options.script_args, ["tail"]);
        // A resource flag in the last position carries no value either.
        let options = parse_options(["-r"].into_iter().map(String::from).collect());
        assert!(options.res_paths.is_empty());
        assert_eq!(options.script_args, ["-r"]);
    }

    #[test]
    fn the_help_branch_prints_the_reference_usage_and_exits_successfully() {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run(["--help"].map(String::from).to_vec(), &mut out, &mut err);
        assert_eq!(code, EXIT_SUCCESS);
        assert!(out.is_empty());
        assert_eq!(String::from_utf8(err).unwrap(), format!("{USAGE}\n"));
        // Byte pins from the stock binary's own output.
        assert!(USAGE.starts_with("usage: process_defs [OPTION] scriptArgs\n"));
        assert!(USAGE.ends_with("  -h, --help                   Displays this message."));
        assert!(USAGE.contains(
            "\n                               \"tools/process_defs/resources/scripts\" in \n"
        ));
    }

    #[test]
    fn a_dump_success_exits_successfully_and_a_load_failure_exits_failure() {
        let mut out = Vec::new();
        let mut err = Vec::new();
        // No res path: the load fails and the tool reports the parse stage.
        let code = run(Vec::new(), &mut out, &mut err);
        assert_eq!(code, EXIT_FAILURE);
        assert!(out.is_empty());
        assert!(
            String::from_utf8(err)
                .unwrap()
                .ends_with("Failed to parse .def files\n")
        );

        // The in-repo fixture: the dump goes to stdout byte for byte as the
        // captured oracle.
        let mut out = Vec::new();
        let mut err = Vec::new();
        let root = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../docs/review/fixtures/service_def/res"
        );
        let code = run(["-r", root].map(String::from).to_vec(), &mut out, &mut err);
        assert_eq!(code, EXIT_SUCCESS);
        assert!(err.is_empty());
        let oracle = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../docs/review/reference/service_entitydef.json"
        ))
        .unwrap();
        assert_eq!(out, oracle);
    }
}
