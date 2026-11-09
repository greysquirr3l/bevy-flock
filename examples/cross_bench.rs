//! The cross-engine fixture: one frame of the whole crowd, reported in the shared JSON schema.
//!
//! This is the `bevy-flock` side of `docs/benchmarks/cross-engine.md`. It runs the documented workload — a spawn burst
//! followed by steady-state ticks — and reports the same six statistics every other engine's harness must report, so the
//! numbers can be put in one table instead of two blog posts.
//!
//! ```text
//! cargo run --release --example cross_bench -- --agents 10000,100000 --ticks 1000
//! cargo run --release --example cross_bench -- --agents 1000000 --ticks 200 --json
//! ```
//!
//! The frame measured is `App::update` over the plugin's own schedule, which is the closest thing this crate has to the
//! number a game loop sees — not a kernel microbenchmark. It excludes everything the crate does not ship: rendering,
//! animation, audio, networking. The Godot and Unreal harnesses in the same document measure their engine's equivalent
//! frame, and the document says in one place what that asymmetry is worth.
use bevy_app::prelude::*;
use bevy_flock::adapters::primary::plugin::{CrowdPreset, FlockConfig, FlockPlugin};
use bevy_flock::domain::ids::SimSeed;
use bevy_flock::domain::num;
use bevy_flock::domain::population::{PopulationMode, PopulationSize};
use std::time::Instant;

/// The frame budget every harness reports against, in milliseconds: 60 Hz.
const BUDGET_MS: f64 = 16.6;

struct Args {
    agents: Vec<usize>,
    ticks: usize,
    warmup: usize,
    json: bool,
}

fn args() -> Args {
    let mut parsed = Args {
        agents: vec![10_000],
        ticks: 1_000,
        warmup: 100,
        json: false,
    };
    let mut rest = std::env::args().skip(1);
    while let Some(flag) = rest.next() {
        match flag.as_str() {
            "--agents" => {
                if let Some(list) = rest.next() {
                    let sizes: Vec<usize> =
                        list.split(',').filter_map(|v| v.parse().ok()).collect();
                    if !sizes.is_empty() {
                        parsed.agents = sizes;
                    }
                }
            }
            "--ticks" => {
                if let Some(value) = rest.next().and_then(|v| v.parse().ok()) {
                    parsed.ticks = value;
                }
            }
            "--warmup" => {
                if let Some(value) = rest.next().and_then(|v| v.parse().ok()) {
                    parsed.warmup = value;
                }
            }
            "--json" => parsed.json = true,
            other => eprintln!("ignoring unknown argument {other}"),
        }
    }
    parsed
}

/// Nearest-rank percentile over a sorted slice, taking the rank as an exact fraction so the arithmetic stays integral:
/// `ceil(numerator * n / denominator)` is `(numerator * n + denominator - 1) / denominator`, with nothing cast.
fn percentile(sorted: &[f64], numerator: usize, denominator: usize) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = (numerator * sorted.len()).div_ceil(denominator).max(1);
    sorted.get(rank - 1).copied().unwrap_or(0.0)
}

fn machine() -> String {
    let cpu = std::fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|text| {
            // x86 says "model name", aarch64 says "Processor" or nothing at all.
            ["model name", "Processor", "Hardware"]
                .iter()
                .find_map(|key| {
                    text.lines()
                        .find(|line| line.starts_with(key))
                        .and_then(|line| line.split(':').nth(1))
                        .map(|name| name.trim().to_owned())
                })
        })
        .unwrap_or_else(|| "unknown cpu".to_owned());
    let cores = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    format!("{cpu}, {cores} threads")
}

fn run(agents: usize, ticks: usize, warmup: usize, json: bool) {
    let config = FlockConfig {
        population: PopulationSize::clamped(agents),
        spawn_budget: 1_000_000,
        mode: PopulationMode::Unique,
        seed: SimSeed::new(0xC0FF_EE00),
        ..FlockConfig::preset(CrowdPreset::CityAmbient)
    };
    let mut app = App::new();
    app.add_plugins(FlockPlugin::new(config));
    // One update drains the spawn queue, so the timed ticks are steady state rather than a spawn burst.
    app.update();
    for _ in 0..warmup {
        app.update();
    }

    let mut samples: Vec<f64> = Vec::with_capacity(ticks);
    for _ in 0..ticks {
        let started = Instant::now();
        app.update();
        samples.push(started.elapsed().as_secs_f64() * 1e3);
    }
    samples.sort_by(f64::total_cmp);

    let median = percentile(&samples, 50, 100);
    let p95 = percentile(&samples, 95, 100);
    let p99 = percentile(&samples, 99, 100);
    let max = samples.last().copied().unwrap_or(0.0);
    let over = samples.iter().filter(|sample| **sample > BUDGET_MS).count();
    // Percentages as integer ratios so no count is widened to f64 to divide by another count.
    let over_pct = if samples.is_empty() {
        0.0
    } else {
        num::usize_to_f64(over * 1000 / samples.len()) / 10.0
    };

    if json {
        println!(
            "{{\"engine\":\"bevy-flock\",\"engine_version\":\"{}\",\"workload\":\"W1-fixed-tick\",\"agents\":{},\
             \"ticks\":{},\"warmup\":{},\"budget_ms\":{},\"median_ms\":{:.3},\"p95_ms\":{:.3},\"p99_ms\":{:.3},\
             \"max_ms\":{:.3},\"over_budget_pct\":{:.2},\"ns_per_agent\":{:.0},\"machine\":\"{}\",\
             \"notes\":\"App::update over the plugin schedule; no rendering, animation, physics or audio\"}}",
            env!("CARGO_PKG_VERSION"),
            agents,
            ticks,
            warmup,
            BUDGET_MS,
            median,
            p95,
            p99,
            max,
            over_pct,
            median * 1e6 / num::usize_to_f64(agents),
            machine(),
        );
    } else {
        println!(
            "{agents} agents: median {median:.3} ms, p95 {p95:.3}, p99 {p99:.3}, max {max:.3}, \
             {over_pct:.2}% over {BUDGET_MS} ms, {:.0} ns/agent",
            median * 1e6 / num::usize_to_f64(agents)
        );
    }
}

fn main() {
    let parsed = args();
    for agents in parsed.agents {
        run(agents, parsed.ticks, parsed.warmup, parsed.json);
    }
}
