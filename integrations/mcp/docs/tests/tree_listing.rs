//! The listing parser must survive a git that answers badly: lines without a
//! tab, lines without a blob id, entries that are not documents.

use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::PathBuf;

use boundless_docs_mcp::source::GitSource;

#[test]
fn the_listing_parser_skips_what_is_not_a_document() {
    // A stand-in git, so the listing comes back exactly as the source gives it.
    let real_git = which_git().expect("find git");
    let shim_dir = tempfile::tempdir().expect("tempdir");
    let shim = shim_dir.path().join("git");
    std::fs::write(
        &shim,
        format!(
            "#!/bin/sh\n\
             for a in \"$@\"; do\n\
             case \"$a\" in\n\
             ls-tree) printf '%s\\t%s\\n' '100644 blob 42' 'docs/guide/good.md'; \
             printf '%s\\n' 'a line without a tab'; \
             printf '%s\\n' 'no-separator-here'; \
             printf '%s\\t%s\\n' '100644 blob 66' 'docs/guide/notes.txt'; exit 0;; esac; done\n\
             exec {} --\n",
            real_git.display(),
        ),
    )
    .expect("write the shim");
    let mut mode = std::fs::metadata(&shim).unwrap().mode();
    mode |= 0o111;
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(mode))
        .expect("executable");

    let repo = tempfile::tempdir().expect("tempdir");
    let saved = std::env::var("PATH").ok();
    // Only this test binary runs here, so the process-wide PATH is ours to borrow.
    unsafe { std::env::set_var("PATH", shim_dir.path()) };
    let source = GitSource::new(repo.path());

    let entries = source.entries("HEAD", "docs").expect("entries");
    let paths: Vec<&str> = entries.iter().map(|entry| entry.path.as_str()).collect();
    assert_eq!(paths, ["guide/good.md"]);
    assert_eq!(entries[0].blob, "42");

    // The same listing, asked for as paths only.
    let listed = source
        .list_markdown("HEAD", "docs")
        .expect("list_markdown");
    assert_eq!(listed, ["guide/good.md"]);

    if let Some(path) = saved {
        unsafe { std::env::set_var("PATH", path) };
    }
}

fn which_git() -> Option<PathBuf> {
    std::path::Path::new("/usr/bin/git").exists().then(|| "/usr/bin/git".into())
        .or_else(|| std::path::Path::new("/bin/git").exists().then(|| "/bin/git".into()))
}
