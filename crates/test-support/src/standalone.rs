//! Standalone build check for application entries (BW-16-01-03).
//!
//! Every entry point under `apps/` must compile by itself, so building one
//! role never drags in another role's code. The check shells out to
//! `cargo build -p <package>` for each application package.

use std::path::Path;
use std::process::Command;

use crate::layering::scan_workspace;

/// Names of the application packages listed in the root manifest.
pub fn application_packages(root: &Path) -> Vec<String> {
    scan_workspace(root)
        .into_iter()
        .filter(|manifest| {
            manifest
                .path
                .parent()
                .and_then(Path::parent)
                .is_some_and(|dir| dir.ends_with("apps"))
        })
        .filter_map(|manifest| manifest.name)
        .collect()
}

/// Build one package in isolation and report why it failed.
pub fn build(root: &Path, package: &str) -> Result<(), String> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let output = Command::new(&cargo)
        .args(["build", "-p", package])
        .current_dir(root)
        .output()
        .map_err(|error| error.to_string())?;

    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "'{cargo} build -p {package}' exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

/// Build every application package and return `(package, reason)` failures.
pub fn build_all(root: &Path) -> Vec<(String, String)> {
    application_packages(root)
        .into_iter()
        .filter_map(|package| match build(root, &package) {
            Ok(()) => None,
            Err(reason) => Some((package, reason)),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    #[test]
    fn application_packages_are_discovered() {
        let packages = application_packages(&root());
        assert_eq!(packages, vec!["boundless-server".to_string()]);
    }

    #[test]
    fn application_crates_build_independently() {
        let failures = build_all(&root());
        assert!(
            failures.is_empty(),
            "standalone builds failed: {failures:?}"
        );
    }

    #[test]
    fn unknown_package_reports_the_error() {
        let error = build(&root(), "no-such-package").expect_err("must fail");
        assert!(error.contains("no-such-package"));
    }
}
