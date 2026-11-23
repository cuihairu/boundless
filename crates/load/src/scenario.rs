//! Load-test scenario input (BW-17-02-01).
//!
//! A scenario is the fixed input a load-test run is driven by: a seed, an
//! account pool, and a concurrency shape (ramp, warmup, duration). Scenarios
//! are saved as XML documents under `tests/load/scenarios/` and loaded through
//! [`Scenario::load_xml`], reusing the reference config parser (`DataSection`,
//! the `BWConfig` style the reference Bots read through `bots_config`).

use std::fmt;
use std::time::Duration;

use resources::datasection::{DataSection, XmlParseError, parse};

/// Error naming the section path that could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScenarioError {
    /// The offending path, e.g. `seed` or `accountPool/mode`.
    pub path: String,
    /// What went wrong at that path.
    pub message: String,
}

impl ScenarioError {
    fn missing(path: &str) -> Self {
        Self {
            path: path.to_string(),
            message: "expected a value".to_string(),
        }
    }

    fn bad(path: &str, message: impl Into<String>) -> Self {
        Self {
            path: path.to_string(),
            message: message.into(),
        }
    }

    fn xml(err: XmlParseError) -> Self {
        Self {
            path: "xml".to_string(),
            message: format!("malformed document: {err}"),
        }
    }
}

impl fmt::Display for ScenarioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.path, self.message)
    }
}

impl std::error::Error for ScenarioError {}

/// A load-test scenario: the fixed input a run is driven by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scenario {
    /// The scenario name, e.g. `login-hurricane`.
    pub name: String,
    /// Deterministic seed for the run's RNG (fixed → reproducible run).
    pub fixed_seed: u64,
    /// The accounts the run draws bots from.
    pub account_pool: AccountPool,
    /// Peak number of concurrent bots.
    pub concurrency: u64,
    /// Bots added per second during the ramp (`0` = instant ramp).
    pub ramp_rate: u64,
    /// Time before the run starts generating load.
    pub warmup: Duration,
    /// How long the run keeps generating load.
    pub duration: Duration,
}

impl Scenario {
    /// Parse a scenario document (`<scenario>…</scenario>`).
    pub fn load_xml(raw: &[u8]) -> Result<Scenario, ScenarioError> {
        let section = parse("scenario", raw).map_err(ScenarioError::xml)?;
        Scenario::from_section(&section)
    }

    /// Build a scenario from an already-parsed section.
    pub fn from_section(section: &DataSection) -> Result<Scenario, ScenarioError> {
        let name = read_string(section, "name")?;
        let fixed_seed = read_uint(section, "seed")?;
        let account_pool = AccountPool::from_section(section)?;
        let concurrency = read_uint(section, "concurrency")?;
        let ramp_rate = read_uint_opt(section, "rampRate").unwrap_or(0);
        let warmup = Duration::from_secs(read_uint_opt(section, "warmup").unwrap_or(0));
        let duration = Duration::from_secs(read_uint(section, "duration")?);
        Ok(Scenario {
            name,
            fixed_seed,
            account_pool,
            concurrency,
            ramp_rate,
            warmup,
            duration,
        })
    }
}

/// The accounts a run draws bots from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountPool {
    /// `<prefix><n>` for `n` in `0..count`, all sharing `password`.
    Generated {
        prefix: String,
        count: u64,
        password: String,
    },
    /// An explicit list of usernames sharing `password`.
    Explicit {
        accounts: Vec<String>,
        password: String,
    },
}

impl AccountPool {
    /// How many distinct accounts the pool provides.
    pub fn count(&self) -> u64 {
        match self {
            AccountPool::Generated { count, .. } => *count,
            AccountPool::Explicit { accounts, .. } => accounts.len() as u64,
        }
    }

    fn from_section(section: &DataSection) -> Result<AccountPool, ScenarioError> {
        let pool = section
            .find_child("accountPool")
            .ok_or_else(|| ScenarioError::missing("accountPool"))?;
        let mode = read_string_opt(pool, "mode").unwrap_or_else(|| "generated".to_string());
        // The reference Bots default the password to "" (bots_config), so an
        // absent/empty password is valid.
        let password = read_string_opt(pool, "password").unwrap_or_default();
        match mode.as_str() {
            "generated" => Ok(AccountPool::Generated {
                prefix: read_string_reporting(pool, "prefix", "accountPool/prefix")?,
                count: read_uint_reporting(pool, "count", "accountPool/count")?,
                password,
            }),
            "explicit" => {
                let list = read_string_reporting(pool, "accounts", "accountPool/accounts")?;
                let accounts: Vec<String> = list
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect();
                if accounts.is_empty() {
                    return Err(ScenarioError::bad(
                        "accountPool/accounts",
                        "expected a non-empty comma-separated list",
                    ));
                }
                Ok(AccountPool::Explicit { accounts, password })
            }
            other => Err(ScenarioError::bad(
                "accountPool/mode",
                format!("unknown mode {other:?}"),
            )),
        }
    }
}

/// Read a required, non-empty string child (`path` is used for the lookup
/// and the reported error path).
fn read_string(section: &DataSection, path: &str) -> Result<String, ScenarioError> {
    read_string_reporting(section, path, path)
}

/// Read a required, non-empty string child, reporting errors under `report`
/// when the lookup name differs from the locatable path.
fn read_string_reporting(
    section: &DataSection,
    lookup: &str,
    report: &str,
) -> Result<String, ScenarioError> {
    let value = read_string_opt(section, lookup).ok_or_else(|| ScenarioError::missing(report))?;
    if value.is_empty() {
        return Err(ScenarioError::bad(report, "expected a non-empty value"));
    }
    Ok(value)
}

/// Read an optional string child (absent → `None`).
fn read_string_opt(section: &DataSection, path: &str) -> Option<String> {
    section
        .find_child(path)
        .map(|s| s.as_string().trim().to_string())
}

/// Read a required unsigned integer child.
fn read_uint(section: &DataSection, path: &str) -> Result<u64, ScenarioError> {
    read_uint_reporting(section, path, path)
}

/// Read a required unsigned integer child, reporting errors under `report`.
fn read_uint_reporting(
    section: &DataSection,
    lookup: &str,
    report: &str,
) -> Result<u64, ScenarioError> {
    let raw = read_string_reporting(section, lookup, report)?;
    raw.trim().parse().map_err(|_| {
        ScenarioError::bad(report, format!("expected an unsigned integer, got {raw:?}"))
    })
}

/// Read an optional unsigned integer child (absent or unparseable → `None`).
fn read_uint_opt(section: &DataSection, path: &str) -> Option<u64> {
    section
        .find_child(path)
        .and_then(|s| s.as_string().trim().parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    const GENERATED: &str = r#"<scenario>
      <name>login-hurricane</name>
      <seed>42</seed>
      <concurrency>500</concurrency>
      <rampRate>50</rampRate>
      <warmup>10</warmup>
      <duration>300</duration>
      <accountPool mode="generated">
        <prefix>Bot</prefix>
        <count>1000</count>
        <password>secret</password>
      </accountPool>
    </scenario>"#;

    fn doc(inner: &str) -> String {
        format!("<scenario>{inner}</scenario>")
    }

    fn pool(inner: &str) -> String {
        doc(&format!(
            "<name>a</name><seed>1</seed><concurrency>1</concurrency><duration>1</duration><accountPool>{inner}</accountPool>"
        ))
    }

    #[test]
    fn a_generated_pool_scenario_loads_all_fields() {
        let s = Scenario::load_xml(GENERATED.as_bytes()).unwrap();
        assert_eq!(s.name, "login-hurricane");
        assert_eq!(s.fixed_seed, 42);
        assert_eq!(s.concurrency, 500);
        assert_eq!(s.ramp_rate, 50);
        assert_eq!(s.warmup, Duration::from_secs(10));
        assert_eq!(s.duration, Duration::from_secs(300));
        assert_eq!(
            s.account_pool,
            AccountPool::Generated {
                prefix: "Bot".into(),
                count: 1000,
                password: "secret".into(),
            }
        );
        assert_eq!(s.account_pool.count(), 1000);
    }

    #[test]
    fn the_acceptance_seed_pool_and_concurrency_load() {
        let s = Scenario::load_xml(GENERATED.as_bytes()).unwrap();
        assert_eq!(s.fixed_seed, 42);
        assert_eq!(s.account_pool.count(), 1000);
        assert_eq!(s.concurrency, 500);
    }

    #[test]
    fn an_explicit_pool_scenario_loads_the_list() {
        let s = Scenario::load_xml(
            doc(
                "<name>steady-move</name><seed>7</seed><concurrency>100</concurrency>\
                 <duration>120</duration>\
                 <accountPool mode=\"explicit\"><accounts>Bot1,Bot2,Bot3</accounts>\
                 <password>pw</password></accountPool>",
            )
            .as_bytes(),
        )
        .unwrap();
        assert_eq!(
            s.account_pool,
            AccountPool::Explicit {
                accounts: vec!["Bot1".into(), "Bot2".into(), "Bot3".into()],
                password: "pw".into(),
            }
        );
        assert_eq!(s.account_pool.count(), 3);
    }

    #[test]
    fn ramp_and_warmup_default_to_zero_and_pool_defaults_are_generated() {
        let s = Scenario::load_xml(pool("<prefix>B</prefix><count>1</count>").as_bytes()).unwrap();
        assert_eq!(s.ramp_rate, 0);
        assert_eq!(s.warmup, Duration::ZERO);
        // Absent `mode` → generated; absent `password` → "" (reference default).
        assert_eq!(
            s.account_pool,
            AccountPool::Generated {
                prefix: "B".into(),
                count: 1,
                password: "".into(),
            }
        );
    }

    #[test]
    fn a_missing_required_field_names_its_path() {
        let err = Scenario::load_xml(
            doc(
                "<name>a</name><concurrency>1</concurrency><duration>1</duration>\
                 <accountPool><prefix>B</prefix><count>1</count></accountPool>",
            )
            .as_bytes(),
        )
        .unwrap_err();
        assert_eq!(err.path, "seed");
        assert_eq!(err.message, "expected a value");
        assert_eq!(err.to_string(), "seed: expected a value");
    }

    #[test]
    fn a_missing_duration_is_reported() {
        let err = Scenario::load_xml(
            doc("<name>a</name><seed>1</seed><concurrency>1</concurrency>\
                 <accountPool><prefix>B</prefix><count>1</count></accountPool>")
            .as_bytes(),
        )
        .unwrap_err();
        assert_eq!(err.path, "duration");
    }

    #[test]
    fn a_non_numeric_seed_is_rejected_with_the_path() {
        let err = Scenario::load_xml(
            doc(
                "<name>a</name><seed>x</seed><concurrency>1</concurrency><duration>1</duration>\
                 <accountPool><prefix>B</prefix><count>1</count></accountPool>",
            )
            .as_bytes(),
        )
        .unwrap_err();
        assert_eq!(err.path, "seed");
        assert!(err.message.contains("unsigned integer"));
    }

    #[test]
    fn a_non_numeric_count_is_rejected() {
        let err = Scenario::load_xml(pool("<prefix>B</prefix><count>many</count>").as_bytes())
            .unwrap_err();
        assert_eq!(err.path, "accountPool/count");
    }

    #[test]
    fn an_empty_required_name_is_rejected() {
        let err = Scenario::load_xml(
            doc(
                "<name></name><seed>1</seed><concurrency>1</concurrency><duration>1</duration>\
                 <accountPool><prefix>B</prefix><count>1</count></accountPool>",
            )
            .as_bytes(),
        )
        .unwrap_err();
        assert_eq!(err.path, "name");
        assert!(err.message.contains("non-empty"));
    }

    #[test]
    fn an_unknown_pool_mode_is_rejected() {
        let err = Scenario::load_xml(
            doc(
                "<name>a</name><seed>1</seed><concurrency>1</concurrency><duration>1</duration>\
                 <accountPool mode=\"warp\"><prefix>B</prefix><count>1</count></accountPool>",
            )
            .as_bytes(),
        )
        .unwrap_err();
        assert_eq!(err.path, "accountPool/mode");
        assert!(err.message.contains("unknown mode"));
    }

    #[test]
    fn an_empty_explicit_list_is_rejected() {
        let err = Scenario::load_xml(
            doc(
                "<name>a</name><seed>1</seed><concurrency>1</concurrency><duration>1</duration>\
                 <accountPool mode=\"explicit\"><accounts></accounts><password>p</password>\
                 </accountPool>",
            )
            .as_bytes(),
        )
        .unwrap_err();
        assert_eq!(err.path, "accountPool/accounts");
        assert!(err.message.contains("non-empty"));
    }

    #[test]
    fn a_generated_pool_requires_count() {
        let err = Scenario::load_xml(pool("<prefix>B</prefix>").as_bytes()).unwrap_err();
        assert_eq!(err.path, "accountPool/count");
    }

    #[test]
    fn a_missing_pool_is_reported() {
        let err = Scenario::load_xml(
            doc("<name>a</name><seed>1</seed><concurrency>1</concurrency><duration>1</duration>")
                .as_bytes(),
        )
        .unwrap_err();
        assert_eq!(err.path, "accountPool");
    }

    #[test]
    fn malformed_xml_is_reported_as_an_xml_error() {
        let err = Scenario::load_xml(b"<scenario><name>a</name>").unwrap_err();
        assert_eq!(err.path, "xml");
        assert!(err.message.contains("malformed"));
    }

    #[test]
    fn from_section_builds_the_same_scenario() {
        let section = parse("scenario", GENERATED.as_bytes()).unwrap();
        let direct = Scenario::from_section(&section).unwrap();
        let parsed = Scenario::load_xml(GENERATED.as_bytes()).unwrap();
        assert_eq!(direct, parsed);
    }

    #[test]
    fn the_error_is_a_std_error() {
        let err = ScenarioError::missing("seed");
        let _: &dyn std::error::Error = &err;
    }
}
