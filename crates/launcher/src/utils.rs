//! The launcher's string toolbox, a port of the reference
//! `tools/bwlauncher/utils.cpp` (the Win32 process/registry/dialog faces
//! live behind the [`LauncherHost`](crate::LauncherHost) seam; this module
//! is the pure string layer).

/// `BW::vector<tstring>` (`StringVector`).
pub type ArgVector = Vec<String>;

/// `splitCommandArgs` (`utils.cpp:66`): Windows command-line parsing.
///
/// - splits on delimiters (default whitespace), which are ignored between
///   matching binding characters (default `"`),
/// - `\"` escapes the quote,
/// - `\\"` escapes the slash only when inside a quoted block.
///
/// Bound tokens always push (empty strings included); unbound buffers
/// only push non-empty. An escape that does not bind the next character
/// falls through and is consumed as itself.
pub fn split_command_args(
    text: &str,
    out: &mut ArgVector,
    delim: &str,
    bind: &str,
    escape: char,
) -> usize {
    let prev_size = out.len();

    let mut buf = String::new();
    let mut binding = '\0';

    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];

        // Process \" (escaped binding char)
        if c == escape && i < chars.len() - 1 && bind.contains(chars[i + 1]) {
            buf.push(chars[i + 1]);
            i += 2;
            continue;
        }

        // Two modes: bound, or unbound.
        if binding != '\0' {
            // Process \\" (escaped slash-with-a-quote): the reference
            // requires i < size-2 so a trailing `\\` never rebinds.
            if c == escape
                && i < chars.len() - 2
                && chars[i + 1] == escape
                && bind.contains(chars[i + 2])
            {
                buf.push(escape);
                i += 2;
                continue;
            }

            if c == binding {
                // Hit matching binding char, add buffer to output
                // (always - empty strings included).
                out.push(std::mem::take(&mut buf));
                binding = '\0';
            } else {
                buf.push(c);
            }
        } else if delim.contains(c) {
            // Hit a delimeter: flush only non-empty buffers.
            if !buf.is_empty() {
                out.push(std::mem::take(&mut buf));
            }
        } else if bind.contains(c) {
            // Hit binding character: flush non-empty buffer, then bind.
            if !buf.is_empty() {
                out.push(std::mem::take(&mut buf));
            }
            binding = c;
        } else {
            buf.push(c);
        }

        i += 1;
    }

    if !buf.is_empty() {
        out.push(buf);
    }

    out.len() - prev_size
}

/// `splitCommandArgs` with the reference defaults (`" \t\r\n"`, `"\""`,
/// `'\\'`).
pub fn split_command_args_default(text: &str, out: &mut ArgVector) -> usize {
    split_command_args(text, out, " \t\r\n", "\"", '\\')
}

/// `findArg` (`utils.cpp:167`): case-insensitive index of `arg`, `None`
/// when absent (`_tcsicmp`).
pub fn find_arg(arg: &str, args: &ArgVector) -> Option<usize> {
    args.iter().position(|a| a.eq_ignore_ascii_case(arg))
}

/// `gotArg` (`utils.cpp:183`): case-insensitive membership test.
pub fn got_arg(arg: &str, args: &ArgVector) -> bool {
    find_arg(arg, args).is_some()
}

/// `getFilename` (`utils.cpp:191`): the trailing component after the last
/// backslash (the launcher is Windows-only; forward slashes are not
/// separators here).
pub fn get_filename(src: &str) -> String {
    match src.rfind('\\') {
        Some(pos) => src[pos + 1..].to_string(),
        None => src.to_string(),
    }
}

/// `getBasePath` (`utils.cpp:205`): everything before the last backslash;
/// no separator → the input unchanged (the reference quirk).
pub fn get_base_path(src: &str) -> String {
    match src.rfind('\\') {
        Some(last_slash) => src[..last_slash].to_string(),
        None => src.to_string(),
    }
}

/// `splitUrl` (`utils.cpp:248`): `bigworld://command/remainder`.
///
/// The `bigworld:` prefix (9 characters) is optional; any run of `/` after
/// it is skipped; the first `/` after that splits the command from the
/// remainder. Always succeeds (`true` in the reference).
pub fn split_url(url: &str) -> (String, String) {
    let bytes = url.as_bytes();
    let mut start = 0;

    if url.len() >= 9 && bytes[..9].eq_ignore_ascii_case(b"bigworld:") {
        start = 9;
    }

    while start < url.len() && bytes[start] == b'/' {
        start += 1;
    }

    match url[start..].find('/') {
        Some(end) => {
            let cmd = url[start..start + end].to_string();
            let remainder = url[start + end + 1..].to_string();
            (cmd, remainder)
        }
        None => (url[start..].to_string(), String::new()),
    }
}

/// `splitString` (`utils.cpp:586`): Python-like `split`, single-character
/// delimiter. Everything before the first delimiter becomes the empty
/// first part; a trailing delimiter does not append an empty tail **but**
/// an all-consumed input yields exactly one empty part.
pub fn split_string(input: &str, delim: &str) -> Vec<String> {
    let mut output = Vec::new();

    let mut pos = 0;
    while let Some(next) = input[pos..].find(delim) {
        let next = pos + next;
        output.push(input[pos..next].to_string());
        pos = next + 1;
    }

    if pos < input.len() {
        output.push(input[pos..].to_string());
    }

    if output.is_empty() {
        output.push(String::new());
    }

    output
}

/// `replaceSubStrings` (`utils.cpp:614`): replace every occurrence of each
/// tag key, continuing after the replacement (so replacements are never
/// re-scanned), keys applied in map order.
pub fn replace_sub_strings(
    input: &str,
    tags: &std::collections::BTreeMap<String, String>,
) -> String {
    let mut output = input.to_string();

    for (key, value) in tags {
        let mut pos = 0;
        while let Some(found) = output[pos..].find(key.as_str()) {
            let found = pos + found;
            output.replace_range(found..found + key.len(), value);
            pos = found + value.len();
        }
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_command_args_splits_on_whitespace_and_keeps_quotes() {
        let mut out = ArgVector::new();
        split_command_args_default("a b  c", &mut out);
        assert_eq!(out, vec!["a", "b", "c"]);
    }

    #[test]
    fn split_command_args_quotes_group_and_allow_empty() {
        let mut out = ArgVector::new();
        split_command_args_default("x \"\" y \"a b\" z", &mut out);
        // Bound tokens always push: the empty pair produces "".
        assert_eq!(out, vec!["x", "", "y", "a b", "z"]);
    }

    #[test]
    fn split_command_args_escape_sequences() {
        // \" is an escaped quote; \\" binds only the trailing quote.
        let mut out = ArgVector::new();
        split_command_args_default("\"a\\\"b\" \"c\\\\\"d", &mut out);
        assert_eq!(out, vec!["a\"b", "c\\", "d"]);
    }

    #[test]
    fn split_command_args_bound_runs_join_delimiters() {
        let mut out = ArgVector::new();
        split_command_args_default("\"a b\"c d", &mut out);
        // The closing quote pushes the bound token; 'c' then starts a
        // fresh unbound buffer flushed at the next delimiter.
        assert_eq!(out, vec!["a b", "c", "d"]);
    }

    #[test]
    fn split_command_args_returns_the_added_count() {
        let mut out = vec!["pre".to_string()];
        let added = split_command_args_default("one two", &mut out);
        assert_eq!(added, 2);
        assert_eq!(out, vec!["pre", "one", "two"]);
    }

    #[test]
    fn find_arg_and_got_arg_are_case_insensitive() {
        let args: ArgVector = vec!["/install".into(), "rest".into()];
        assert_eq!(find_arg("/INSTALLALL", &args), None);
        assert_eq!(find_arg("/Install", &args), Some(0));
        assert!(got_arg("/install", &args));
        assert!(!got_arg("/installAll", &args));
    }

    #[test]
    fn get_filename_and_base_path_follow_backslashes() {
        assert_eq!(get_filename("C:\\x\\y.exe"), "y.exe");
        assert_eq!(get_filename("plain"), "plain");
        assert_eq!(get_base_path("C:\\x\\y.exe"), "C:\\x");
        // Reference quirk: no separator returns the input unchanged.
        assert_eq!(get_base_path("plain"), "plain");
    }

    #[test]
    fn split_url_strips_the_scheme_and_slashes() {
        assert_eq!(
            split_url("bigworld://launch/game/user/pass"),
            ("launch".to_string(), "game/user/pass".to_string())
        );
        assert_eq!(
            split_url("bigworld:/browse/game"),
            ("browse".to_string(), "game".to_string())
        );
        assert_eq!(
            split_url("launch/game"),
            ("launch".to_string(), "game".to_string())
        );
        assert_eq!(split_url("bigworld://"), ("".to_string(), "".to_string()));
    }

    #[test]
    fn split_string_is_python_like() {
        assert_eq!(split_string("a/b/c", "/"), vec!["a", "b", "c"]);
        assert_eq!(split_string("a//c", "/"), vec!["a", "", "c"]);
        // Trailing delimiter: the tail is consumed, nothing appended.
        assert_eq!(split_string("a/", "/"), vec!["a"]);
        // Fully consumed input: one empty part.
        assert_eq!(split_string("", "/"), vec![""]);
    }

    #[test]
    fn replace_sub_strings_substitutes_every_occurrence() {
        let mut tags = std::collections::BTreeMap::new();
        tags.insert("%(GameID)".to_string(), "crab".to_string());
        let url = replace_sub_strings(
            "http://games.bigworldtech.com:8080/library/%(GameID)/",
            &tags,
        );
        assert_eq!(url, "http://games.bigworldtech.com:8080/library/crab/");
    }

    #[test]
    fn replace_sub_strings_never_rescans_replacements() {
        let mut tags = std::collections::BTreeMap::new();
        tags.insert("a".to_string(), "aa".to_string());
        // "aaa" → replace(0,1,"aa")="aaaa" → replace(2,1,"aa")="aaaaa" →
        // replace(4,1,"aa")="aaaaaa"; scanning resumes after each
        // replacement, so the tail keeps growing by one a per pass.
        assert_eq!(replace_sub_strings("aaa", &tags), "aaaaaa");
    }
}
