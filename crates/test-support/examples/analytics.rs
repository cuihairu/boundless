//! BW-26-03-04: the analytics query examples, runnable as one target.
//!
//! `cargo run -p test-support --example analytics` — each example shown in the
//! VitePress page `docs/operations/analytics.md` runs here and prints its
//! result, so a reader reproduces the metrics口径 by hand.

use test_support::event_aggregate;
use test_support::event_contract::{EventRecorder, ExportPolicy, export};
use test_support::event_filter::{Filter, validate};
use test_support::event_partition::partition_by_day;
use test_support::login_funnel::{FunnelStage, LoginFunnel};
use test_support::session_duration::{DurationSummary, crosses_day};
use test_support::session_metrics::{SessionRegistry, SessionWindow};
use test_support::space_heat::{HOT_AT, SpaceHeat, bucket_of};

fn main() {
    // One fixed fixture: the events and sessions a single day of play records.
    let mut recorder = EventRecorder::new();
    let events = vec![
        recorder.login(100, "baseapp01", "alice"),
        recorder.login(150, "baseapp01", "bob"),
        recorder.login(200, "baseapp01", "alice"),
        recorder.login(400, "baseapp01", "carol"),
        recorder.movement(120, "cellapp01", "alice", 1, 2),
        recorder.movement(220, "cellapp01", "bob", 3, 2),
        recorder.movement(320, "cellapp01", "alice", 2, 4),
        recorder.movement(450, "cellapp01", "carol", 1, 2),
        recorder.custom(
            1_000,
            "cellapp01",
            "purchase",
            &[("price", "100")],
            &["price"],
        ),
        recorder.custom(
            1_100,
            "cellapp01",
            "purchase",
            &[("price", "150")],
            &["price"],
        ),
        recorder.custom(1_250, "cellapp01", "telemetry", &[], &[]),
        recorder.custom(2_050, "baseapp01", "join", &[("ms", "50")], &["ms"]),
    ];

    // Validation and filtering (BW-26-02-01): the bad events are named and a
    // filter selects the valid ones, reproducibly.
    let mut with_bad = events.clone();
    with_bad[0].version = "2".to_string();
    let (valid, invalid) = validate(&with_bad);
    println!(
        "validation: {} valid, {} invalid",
        valid.len(),
        invalid.len()
    );
    let movements = Filter::new().kind(test_support::event_contract::EventKind::Movement);
    println!("filter movements: {}", movements.select(&with_bad).len());

    // Online and concurrency (BW-26-02-02): fixed session windows.
    let windows = vec![
        SessionWindow {
            session: "s-1".into(),
            player: "alice".into(),
            started: 100,
            ended: Some(300),
        },
        SessionWindow {
            session: "s-2".into(),
            player: "bob".into(),
            started: 200,
            ended: Some(250),
        },
        SessionWindow {
            session: "s-3".into(),
            player: "carol".into(),
            started: 400,
            ended: None,
        },
    ];
    let mut sessions = SessionRegistry::new();
    for window in &windows {
        sessions.add(window.clone());
    }
    println!("online at 250: {}", sessions.online_at(250));
    println!("peak concurrency: {}", sessions.peak());

    // Session duration (BW-26-02-03): cross-day and still-open rules.
    let summary = DurationSummary::of(&windows);
    println!(
        "sessions closed {} open {}, cross-day {}",
        summary.closed,
        summary.open,
        crosses_day(995, 1_010, |at| at / 1_000)
    );
    println!("longest session: {}", summary.longest);

    // Login funnel (BW-26-02-04): a retried login counts once.
    let mut funnel = LoginFunnel::new();
    for event in &events {
        funnel.record(event);
    }
    println!("funnel: {}", funnel.render());
    println!(
        "funnel first-move: {}",
        funnel.count(FunnelStage::FirstMove)
    );

    // Space heat (BW-26-02-05): fixed locations fall into buckets.
    let mut heat = SpaceHeat::new();
    for event in &events {
        heat.record(event);
    }
    println!(
        "space 2 {} ({}), space 3 {} ({}), hot at {}",
        match bucket_of(heat.occupancy(2)) {
            test_support::space_heat::HeatBucket::Cold => "cold",
            test_support::space_heat::HeatBucket::Warm => "warm",
            test_support::space_heat::HeatBucket::Hot => "hot",
        },
        heat.occupancy(2),
        match bucket_of(heat.occupancy(3)) {
            test_support::space_heat::HeatBucket::Cold => "cold",
            test_support::space_heat::HeatBucket::Warm => "warm",
            test_support::space_heat::HeatBucket::Hot => "hot",
        },
        heat.occupancy(3),
        HOT_AT,
    );

    // Custom event aggregation (BW-26-02-06): same input, same report.
    println!(
        "report:\n{}",
        event_aggregate::render(&event_aggregate::aggregate(&events, "price"))
    );

    // Re-import and date partitioning (BW-26-03): the aggregate does not
    // change with how the stream is split.
    let mut once = test_support::event_import::Import::new();
    once.import(&events);
    let mut again = test_support::event_import::Import::new();
    for day_batch in partition_by_day(&events, |at| at / 1_000) {
        let owned: Vec<_> = day_batch.iter().map(|event| (*event).clone()).collect();
        again.import(&owned);
    }
    println!(
        "reimport same report: {}",
        once.report("price") == again.report("price")
    );

    // Batch export with boundary redaction (BW-26-01-03): no sensitive value
    // leaks.
    let audit = recorder.custom(
        3_000,
        "baseapp01",
        "login_audit",
        &[("account", "alice"), ("password", "hunter2")],
        &["account", "password"],
    );
    let sensitive = export(&[audit], &ExportPolicy::default_policy());
    println!(
        "redaction leaks nothing: {}",
        sensitive.leaks(&["hunter2"]).is_none()
    );
}
