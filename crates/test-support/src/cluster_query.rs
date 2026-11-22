//! BW-25-02: the cluster query and view contract.
//!
//! The CLI, CI and the IDE all read the cluster through the same query
//! interface: a single process by role, or the whole cluster, and targets
//! filtered by role. It is the read-only view the status collector feeds.
//!
//! The three consumers also parse the same JSON snapshot document. Its field
//! names are part of the contract — `snapshot_at`, and per process `role`,
//! `pid`, `readings`, each reading carrying `item`, `value`, `source` and
//! `collected_at` — and the document is produced deterministically, so every
//! consumer keys on the same names.

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

    /// The JSON snapshot of the cluster as it now stands: a document CLI, CI
    /// and the IDE all parse with the same field names.
    ///
    /// ```json
    /// {
    ///   "snapshot_at": 1000,
    ///   "processes": [
    ///     { "role": "cellapp01", "pid": 100,
    ///       "readings": [
    ///         { "item": "version", "value": "0.1.0",
    ///           "source": "build metadata", "collected_at": 1000 } ] } ]
    /// }
    /// ```
    ///
    /// Processes are ordered by role; readings keep the order they were
    /// taken. Values are escaped, so the document stays parseable whatever a
    /// reading holds.
    pub fn snapshot_json(&self) -> String {
        let mut out = String::new();
        out.push_str("{\"snapshot_at\":");
        out.push_str(&self.collector.now().to_string());
        out.push_str(",\"processes\":[");
        for (index, status) in self.cluster().iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            out.push_str("{\"role\":\"");
            out.push_str(&escape(&status.role));
            out.push_str("\",\"pid\":");
            match status.pid {
                Some(pid) => out.push_str(&pid.to_string()),
                None => out.push_str("null"),
            }
            out.push_str(",\"readings\":[");
            for (reading_index, reading) in status.readings.iter().enumerate() {
                if reading_index > 0 {
                    out.push(',');
                }
                out.push_str("{\"item\":\"");
                out.push_str(&escape(&reading.item));
                out.push_str("\",\"value\":\"");
                out.push_str(&escape(&reading.value));
                out.push_str("\",\"source\":\"");
                out.push_str(&escape(&reading.source));
                out.push_str("\",\"collected_at\":");
                out.push_str(&reading.collected_at.to_string());
                out.push('}');
            }
            out.push_str("]}");
        }
        out.push_str("]}");
        out
    }
}

/// Escape a value into a JSON string body: quotes, backslashes and control
/// characters become the escapes JSON readers understand.
fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if (ch as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", ch as u32)),
            ch => out.push(ch),
        }
    }
    out
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

    /// The value token after the first `"key":` in a JSON document, the way a
    /// consumer keys on a field.
    fn value_after<'a>(doc: &'a str, key: &str) -> &'a str {
        let needle = format!("\"{key}\":");
        let start = doc.find(&needle).expect("field present") + needle.len();
        &doc[start..]
    }

    #[test]
    fn the_snapshot_document_is_deterministic_and_uses_one_field_set() {
        let mut collector = StatusCollector::with_clock(|| 1_000);
        collector
            .register("cellapp01", Some(100))
            .register("baseapp01", None);
        collector.record("cellapp01", "version", "0.1.0", "build metadata");
        collector.record("cellapp01", "ready", "true", "readiness announcement");
        collector.record("baseapp01", "version", "0.1.0", "build metadata");

        let doc = ClusterQuery::new(&collector).snapshot_json();

        // One fixed field set, in a fixed order: whatever reads the document
        // keys on the same names.
        assert_eq!(
            doc,
            "{\"snapshot_at\":1000,\"processes\":[{\"role\":\"baseapp01\",\"pid\":null,\
             \"readings\":[{\"item\":\"version\",\"value\":\"0.1.0\",\
             \"source\":\"build metadata\",\"collected_at\":1000}]},\
             {\"role\":\"cellapp01\",\"pid\":100,\
             \"readings\":[{\"item\":\"version\",\"value\":\"0.1.0\",\
             \"source\":\"build metadata\",\"collected_at\":1000},\
             {\"item\":\"ready\",\"value\":\"true\",\
             \"source\":\"readiness announcement\",\"collected_at\":1000}]}]}"
        );
    }

    #[test]
    fn every_consumer_reads_the_same_field_names() {
        let collector = cluster();
        let doc = ClusterQuery::new(&collector).snapshot_json();

        // Each field the contract names is present and reads back a value.
        assert!(value_after(&doc, "snapshot_at").starts_with("1000,"));
        assert!(value_after(&doc, "role").starts_with("\"baseapp01\""));
        assert!(value_after(&doc, "pid").starts_with("200,"));
        assert!(value_after(&doc, "item").starts_with("\"version\""));
        assert!(value_after(&doc, "value").starts_with("\"0.1.0\""));
        assert!(value_after(&doc, "source").starts_with("\"build metadata\""));
        assert!(value_after(&doc, "collected_at").starts_with("1000"));
    }

    #[test]
    fn the_snapshot_document_escapes_values_for_any_consumer() {
        let mut collector = StatusCollector::with_clock(|| 0);
        collector.register("cellapp01", Some(1));
        collector.record(
            "cellapp01",
            "message",
            "a \"quote\"\nnewline\\backslash",
            "src",
        );

        let doc = ClusterQuery::new(&collector).snapshot_json();

        assert!(doc.contains("\\\"quote\\\""));
        assert!(doc.contains("\\nnewline"));
        assert!(doc.contains("\\\\backslash"));
        // The raw characters do not leak into the document.
        assert!(!doc.contains("\"a \"quote\""));
    }
}
