//! BW-23-03-03: flame graph data.
//!
//! Renders sampled hot stacks as flame graph input — the collapsed-stack
//! format one line per distinct stack, frames joined by `;` and weighted by
//! the sample count (`frame;frame;... samples`). The frames are the section
//! names the sampler saw open, so a hot stack's leaf names the Rust function
//! — or, when a script's call frames are sampled, the script function — the
//! time actually went to.

use crate::cpu_sampler::HotStack;

/// Collapse aggregated hot stacks into flame graph data: one
/// `frame;frame;... samples` line per stack, most-sampled first (the
/// aggregate's own order). Blank stacks are skipped.
pub fn collapse(hot_stacks: &[HotStack]) -> String {
    hot_stacks
        .iter()
        .filter(|hot| !hot.stack.is_empty())
        .map(|hot| format!("{} {}", hot.stack.join(";"), hot.samples))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpu_sampler::{CpuSampler, aggregate};
    use foundation::profiler::Profiler;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    type Pinned = Arc<AtomicU64>;

    fn pinned() -> Pinned {
        Arc::new(AtomicU64::new(0))
    }

    fn profiler_with(source: &Pinned) -> Profiler {
        let source = source.clone();
        Profiler::with_clock(move || source.load(Ordering::SeqCst))
    }

    fn advance(source: &Pinned, by: u64) {
        source.fetch_add(by, Ordering::SeqCst);
    }

    #[test]
    fn hot_stacks_collapse_into_flame_graph_lines() {
        let source = pinned();
        let prof = profiler_with(&source);
        let sampler_clock = source.clone();
        let mut sampler =
            CpuSampler::with_clock(&[&prof], 2, move || sampler_clock.load(Ordering::SeqCst));

        // A fixed load: inside `ServerApp::tick`, `updateCells` burns nine
        // stamps, then a one-stamp `submitOrders` blip, ten times over.
        let mut samples = Vec::new();
        for _ in 0..10 {
            let _tick = prof.scoped("ServerApp::tick");
            {
                let _hot = prof.scoped("updateCells");
                for _ in 0..9 {
                    advance(&source, 1);
                    sampler.collect_due(&mut samples);
                }
            }
            {
                let _blip = prof.scoped("submitOrders");
                advance(&source, 1);
                sampler.collect_due(&mut samples);
            }
        }

        let data = collapse(&aggregate(&samples));
        let lines: Vec<&str> = data.lines().collect();
        assert_eq!(
            lines,
            vec![
                "ServerApp::tick;updateCells 40",
                "ServerApp::tick;submitOrders 10",
            ],
            "{data}"
        );

        // The hot spot: the heaviest line's leaf is the Rust function the
        // time went to, and it outweighs the blip.
        let (top, weight) = lines[0].rsplit_once(' ').expect("line ends with a count");
        assert!(top.ends_with("updateCells"));
        assert!(weight.parse::<u32>().unwrap() > 10);
    }

    #[test]
    fn script_function_frames_keep_their_names() {
        // A script's sampled call frames — the leaf names the script function
        // (the name `python_profile` reports for it) — collapse like any
        // other stack.
        let hot = HotStack {
            stack: vec![
                "ServerApp::tick".to_string(),
                "runScript".to_string(),
                "burn".to_string(),
            ],
            samples: 25,
        };
        let cold = HotStack {
            stack: vec!["ServerApp::tick".to_string()],
            samples: 5,
        };

        let data = collapse(&[hot, cold]);
        let lines: Vec<&str> = data.lines().collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "ServerApp::tick;runScript;burn 25");

        // The script function is the leaf of the heaviest stack.
        let (top, _) = lines[0].rsplit_once(' ').expect("line ends with a count");
        assert_eq!(top.rsplit(';').next(), Some("burn"));
    }

    #[test]
    fn an_empty_aggregate_collapse_to_nothing() {
        assert_eq!(collapse(&[]), "");
        let empty = HotStack {
            stack: Vec::new(),
            samples: 3,
        };
        assert_eq!(collapse(&[empty]), "", "a blank stack is skipped");
    }
}
