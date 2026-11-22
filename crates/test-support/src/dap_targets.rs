//! BW-27-02-03: discovering debug targets across a multi-process world.
//!
//! A world runs many processes of the same role — several CellApps, several
//! BaseApps — and the editor must list them separately: attach to one and it
//! must not be confused with the other. Each discovered process gets an id
//! that no two share, built from its role and pid.

use crate::dap_session::Adapter;

/// One process a discovery pass saw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessInfo {
    /// The process id.
    pub pid: u32,
    /// The role the process plays: `baseapp`, `cellapp`, ...
    pub role: String,
    /// The adapter that can drive it.
    pub adapter: Adapter,
    /// Where its debug port listens.
    pub address: String,
}

/// One debug target the editor can list and attach to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DebugTarget {
    /// The target's identity: role and pid, so instances of one role stay apart.
    pub id: String,
    /// The process the target is.
    pub pid: u32,
    /// The role the target plays.
    pub role: String,
    /// The adapter that drives the target.
    pub adapter: Adapter,
    /// Where the target's debug port listens.
    pub address: String,
}

impl DebugTarget {
    /// The label the editor lists the target under.
    pub fn label(&self) -> String {
        format!("{} (pid {})", self.role, self.pid)
    }
}

/// Discover debug targets from the processes seen, in pid order.
pub fn discover(processes: &[ProcessInfo]) -> Vec<DebugTarget> {
    let mut targets: Vec<DebugTarget> = processes
        .iter()
        .map(|process| DebugTarget {
            id: format!("{}-{}", process.role, process.pid),
            pid: process.pid,
            role: process.role.clone(),
            adapter: process.adapter,
            address: process.address.clone(),
        })
        .collect();
    targets.sort_by_key(|target| target.pid);
    targets
}

/// The targets that play one role, in the same order `discover` returned them.
pub fn by_role<'a>(targets: &'a [DebugTarget], role: &str) -> Vec<&'a DebugTarget> {
    targets
        .iter()
        .filter(|target| target.role == role)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn world() -> Vec<ProcessInfo> {
        vec![
            ProcessInfo {
                pid: 1102,
                role: "cellapp".into(),
                adapter: Adapter::Rust,
                address: "127.0.0.1:12346".into(),
            },
            ProcessInfo {
                pid: 2201,
                role: "baseapp".into(),
                adapter: Adapter::Python,
                address: "127.0.0.1:5678".into(),
            },
            ProcessInfo {
                pid: 1101,
                role: "cellapp".into(),
                adapter: Adapter::Rust,
                address: "127.0.0.1:12345".into(),
            },
            ProcessInfo {
                pid: 2202,
                role: "baseapp".into(),
                adapter: Adapter::Python,
                address: "127.0.0.1:5679".into(),
            },
        ]
    }

    #[test]
    fn instances_of_one_role_get_distinct_ids() {
        let targets = discover(&world());

        // Two cellapps and two baseapps, in pid order: four targets, no
        // shared id.
        let ids: Vec<&str> = targets.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "cellapp-1101",
                "cellapp-1102",
                "baseapp-2201",
                "baseapp-2202"
            ]
        );
        assert_eq!(
            ids.len(),
            ids.into_iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
        );
    }

    #[test]
    fn one_role_lists_only_its_own_instances() {
        let targets = discover(&world());
        let cellapps = by_role(&targets, "cellapp");

        assert_eq!(cellapps.len(), 2);
        assert_eq!(cellapps[0].pid, 1101);
        assert_eq!(cellapps[1].pid, 1102);
        // Same role, different instances: their ids and ports stay apart.
        assert_ne!(cellapps[0].id, cellapps[1].id);
        assert_ne!(cellapps[0].address, cellapps[1].address);
        assert_eq!(by_role(&targets, "monolith").len(), 0);
    }

    #[test]
    fn an_empty_world_lists_nothing() {
        assert!(discover(&[]).is_empty());
    }

    #[test]
    fn a_target_lists_itself_stably() {
        let targets = discover(&world());
        let first = by_role(&targets, "cellapp")[0];

        // The editor shows the target by its role and pid, and discovers it
        // the same way a second time.
        assert_eq!(first.label(), "cellapp (pid 1101)");
        assert_eq!(discover(&world())[0].id, first.id);
    }
}
