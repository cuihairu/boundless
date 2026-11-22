//! BW-25-02: the cluster query contract.
//!
//! The CLI, CI and the IDE all read the cluster through the same query
//! interface: a single process by role, or the whole cluster, and targets
//! filtered by role. It is the read-only view the status collector feeds.

use crate::process_status::{ProcessStatus, StatusCollector};

/// The read-only view of the cluster a query answers. It borrows the
/// collector, so a query always reads the current state.
pub struct ClusterQuery<'a> {
    collector: &'a StatusCollector,
}

impl<'a> ClusterQuery<'a> {
    /// A query over the cluster as the collector knows it.
    pub fn new(collector: &'a StatusCollector) -> Self {
        Self { collector }
    }

    /// Query one process by role.
    pub fn single(&self, role: &str) -> Option<&'a ProcessStatus> {
        self.collector.status(role)
    }

    /// Query the whole cluster: every process, ordered by role.
    pub fn cluster(&self) -> Vec<&'a ProcessStatus> {
        self.collector
            .roles()
            .into_iter()
            .map(|role| {
                self.collector
                    .status(role)
                    .expect("a listed role is registered")
            })
            .collect()
    }

    /// The targets whose role contains `pattern`, ordered by role: the role
    /// filter the CLI applies.
    pub fn filter_by_role(&self, pattern: &str) -> Vec<&'a ProcessStatus> {
        self.cluster()
            .into_iter()
            .filter(|status| status.role.contains(pattern))
            .collect()
    }

    /// Every process role, ascending.
    pub fn roles(&self) -> Vec<&'a str> {
        self.collector.roles()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cluster() -> StatusCollector {
        let mut collector = StatusCollector::with_clock(|| 1_000);
        collector
            .register("cellapp01", Some(100))
            .register("cellapp02", Some(101))
            .register("baseapp01", Some(200))
            .register("loginapp01", Some(300));
        for role in ["cellapp01", "cellapp02", "baseapp01", "loginapp01"] {
            collector.record(role, "version", "0.1.0", "build metadata");
        }
        collector
    }

    #[test]
    fn filtering_targets_by_role_selects_matching_processes() {
        let collector = cluster();
        let query = ClusterQuery::new(&collector);

        // The role filter picks out the matching processes only.
        let cells: Vec<&str> = query
            .filter_by_role("cellapp")
            .iter()
            .map(|status| status.role.as_str())
            .collect();
        assert_eq!(cells, vec!["cellapp01", "cellapp02"]);

        let bases: Vec<&str> = query
            .filter_by_role("baseapp")
            .iter()
            .map(|status| status.role.as_str())
            .collect();
        assert_eq!(bases, vec!["baseapp01"]);

        assert!(query.filter_by_role("nosuchrole").is_empty());
        assert_eq!(
            query.roles(),
            vec!["baseapp01", "cellapp01", "cellapp02", "loginapp01"]
        );
    }

    #[test]
    fn a_single_process_query_names_its_role() {
        let collector = cluster();
        let query = ClusterQuery::new(&collector);
        let single = query.single("cellapp01").unwrap();
        assert_eq!(single.role, "cellapp01");
        assert_eq!(single.pid, Some(100));
        assert_eq!(single.reading("version").unwrap().value, "0.1.0");
        assert!(query.single("missing").is_none());
    }

    #[test]
    fn a_cluster_query_lists_every_process_ordered_by_role() {
        let collector = cluster();
        let query = ClusterQuery::new(&collector);
        let roles: Vec<&str> = query
            .cluster()
            .iter()
            .map(|status| status.role.as_str())
            .collect();
        assert_eq!(
            roles,
            vec!["baseapp01", "cellapp01", "cellapp02", "loginapp01"]
        );
    }
}
