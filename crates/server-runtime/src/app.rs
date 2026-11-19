//! BW-00-03-05 minimal process lifecycle: validate the requested role and
//! bring it up, keeping `main` free of role logic.
//!
//! Full init chains (bw.xml, network interfaces, signal handling) arrive
//! with BW-01-04; this module fixes the role set and the startup contract.

/// Server processes of this BigWorld revision (`server/*/` directories),
/// in canonical order.
pub const KNOWN_ROLES: &[&str] = &[
    "baseapp",
    "baseappmgr",
    "cellapp",
    "cellappmgr",
    "dbapp",
    "dbappmgr",
    "loginapp",
    "reviver",
];

/// Why `run` refused to start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppError {
    /// Role not in `KNOWN_ROLES`.
    UnknownRole(String),
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AppError::UnknownRole(role) => write!(
                f,
                "unknown server role '{role}' (expected one of: {})",
                KNOWN_ROLES.join(", ")
            ),
        }
    }
}

impl std::error::Error for AppError {}

/// Brings the requested role up and returns once it is ready to run.
///
/// The reference starts each process with a startup log line naming the
/// app; the minimal entry keeps that observable behaviour.
pub fn run(role: &str) -> Result<(), AppError> {
    if !KNOWN_ROLES.contains(&role) {
        return Err(AppError::UnknownRole(role.to_string()));
    }
    eprintln!("[{role}] starting (boundless minimal entry)");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_reference_role_is_accepted() {
        for role in KNOWN_ROLES {
            assert_eq!(run(role), Ok(()), "role {role} must start");
        }
    }

    #[test]
    fn unknown_role_is_rejected_with_directions() {
        let err = run("cellap").unwrap_err();
        assert_eq!(err, AppError::UnknownRole("cellap".to_string()));
        let msg = err.to_string();
        assert!(msg.contains("cellap"), "echoes the bad role: {msg}");
        assert!(msg.contains("cellapp"), "suggests the correct role: {msg}");
    }

    #[test]
    fn empty_role_is_unknown() {
        assert_eq!(run(""), Err(AppError::UnknownRole(String::new())));
    }

    #[test]
    fn role_list_covers_the_reference_server_dirs() {
        // The dirs under programming/bigworld/server minus build plumbing.
        let expected = [
            "baseapp",
            "baseappmgr",
            "cellapp",
            "cellappmgr",
            "dbapp",
            "dbappmgr",
            "loginapp",
            "reviver",
        ];
        assert_eq!(KNOWN_ROLES, expected);
    }
}
