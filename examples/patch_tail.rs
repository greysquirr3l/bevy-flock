//! Tail latency of the patched rebuild under heavy random crosser loads, at two slack rules.
//!
//! [`SpatialGrid::build_incremental`] either patches the bodies that changed cell or falls back to a full rebuild, and the
//! fallback is what a frame's tail is made of. Two things decide how often it happens: how many bodies cross a cell
//! boundary in one tick, and how much slack each cell's run was given when the table was laid out. This harness measures
//! both, with arrivals that are either uniformly random or clustered into one cell per tick, which is the pattern the
//! patch
//! is worst at and therefore the one worth putting a number on.
//!
//! ```text
//! cargo run --release --example patch_tail
//! cargo run --release --example patch_tail -- --bodies 1000000 --crossers 64,512,2048 --slack 25,50 --floor 0,1,2,4
//!
//! `--arrival random|cluster` picks where a tick's crossers land. `random` scatters them over the whole field, which is
//! the pattern the patch was measured against first; `cluster` puts them inside a hotspot `--cluster-radius` cells wide
//! around one point per tick, which is a crowd flowing into one district. The crossers are the same bodies either way.
//! cargo run --release --example patch_tail -- --json
//! ```
//!
//! Each row is one `(bodies, crossers, slack)` cell of the sweep: the share of ticks that patched, the share that had to
//! rebuild, and the median, p95, p99 and worst tick. `order_slots` is the table's width, so the columns show what the
//! slack costs in memory as well as what it buys in tail.

use std::io::{self, Write};

use bevy_flock::domain::NpcId;
use bevy_flock::domain::num;
use bevy_flock::domain::spatial::{GridEntry, SpatialGrid, SpatialGridConfig};
use bevy_math::Vec3;

/// How far the field reaches on each axis, so a teleported body is a plausible body rather than a corner case.
const EXTENT: f32 = 512.0;

/// The cell size every row uses. Small cells mean many boundaries per unit of motion, which is the regime a moving crowd
/// is in when it is spread out.
const CELL: f32 = 8.0;

/// Ticks measured per row. Enough for a p99 that is not just the worst of a handful.
const DEFAULT_TICKS: usize = 300;

/// Warm-up ticks, discarded. The first patch after a rebuild pays for a cold table.
const WARMUP: usize = 30;

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            let _ = writeln!(io::stderr(), "patch_tail: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

/// Where a tick's arrivals land: uniformly through the field, or all inside one cell.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Arrival {
    /// Uniformly random, which is the worst case for a particle system and the best case for a grid.
    Random,
    /// All of them around one point, which is the correlated arrival the borrow budget was added for.
    Cluster,
}

impl Arrival {
    fn label(self) -> &'static str {
        match self {
            Self::Random => "random",
            Self::Cluster => "cluster",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        match text {
            "random" => Some(Self::Random),
            "cluster" => Some(Self::Cluster),
            _ => None,
        }
    }
}

/// A displacement inside the hotspot: `radius` cells on each axis around the tick's hot point.
fn jitter(rng: &mut Rng, radius: f32) -> Vec3 {
    let unit = |rng: &mut Rng| num::u32_to_f32(rng.below(1_000_001)) / 1_000_000.0;
    let reach = CELL * radius;
    Vec3::new(
        (unit(rng) * 2.0 - 1.0) * reach,
        (unit(rng) * 2.0 - 1.0) * reach,
        (unit(rng) * 2.0 - 1.0) * reach,
    )
}

/// One row of the sweep: a workload, a slack rule, and what that combination cost.
struct Row {
    bodies: u32,
    crossers: u32,
    arrival: Arrival,
    slack: u8,
    floor: u32,
    workers: usize,
    ticks: usize,
    patched: usize,
    rebuilt: usize,
    unchanged: usize,
    rebuild_ms: f64,
    median_ms: f64,
    p95_ms: f64,
    p99_ms: f64,
    worst_ms: f64,
    order_slots: usize,
}

fn run() -> io::Result<()> {
    let args = Args::parse(std::env::args().skip(1))?;
    let mut out = io::stdout();
    if !args.json {
        writeln!(
            out,
            "patch_tail: workers {:?}, {} measured ticks after {WARMUP} warm-ups, {:.0} units of field, \
             {CELL} unit cells",
            args.workers,
            args.ticks,
            EXTENT * 2.0
        )?;
        writeln!(
            out,
            "{:>7} {:>9} {:>8} {:>5} {:>5} {:>7} {:>8} {:>8} {:>8} {:>7} {:>7} {:>7} {:>7} {:>9}",
            "arrival",
            "bodies",
            "crossers",
            "slack",
            "floor",
            "workers",
            "patched",
            "rebuilt",
            "same",
            "median",
            "p95",
            "p99",
            "worst",
            "order"
        )?;
    }
    for &bodies in &args.bodies {
        for &arrival in &args.arrival {
            for &crossers in &args.crossers {
                for &slack in &args.slack {
                    for &floor in &args.floor {
                        for &workers in &args.workers {
                            let row = measure(
                                bodies,
                                crossers,
                                slack,
                                floor,
                                workers,
                                args.ticks,
                                arrival,
                                args.radius,
                            )?;
                            if args.json {
                                writeln!(out, "{}", row.to_json())?;
                            } else {
                                writeln!(
                                    out,
                                    "{:>7} {:>9} {:>8} {:>4}% {:>5} {:>7} {:>7.1}% {:>7.1}% {:>7.1}% {:>6.3} \
                                 {:>6.3} {:>6.3} {:>7.3} {:>9}",
                                    row.arrival.label(),
                                    row.bodies,
                                    row.crossers,
                                    row.slack,
                                    row.floor,
                                    row.workers,
                                    percent_of(row.patched, row.ticks),
                                    percent_of(row.rebuilt, row.ticks),
                                    percent_of(row.unchanged, row.ticks),
                                    row.median_ms,
                                    row.p95_ms,
                                    row.p99_ms,
                                    row.worst_ms,
                                    row.order_slots
                                )?;
                            }
                        }
                    }
                }
            }
        }
    }
    if !args.json {
        writeln!(
            out,
            "\ncolumns: patched/rebuilt/unchanged are shares of the measured ticks; times are one tick's \
             `build_incremental`, in ms;\norder is the table's width in slots, so the slack's cost is visible next to \
             its tail"
        )?;
    }
    Ok(())
}

impl Row {
    fn to_json(&self) -> String {
        format!(
            "{{\"schema_version\":1,\"kind\":\"patch_tail\",\"arrival\":\"{}\",\"bodies\":{},\"crossers\":{},\"slack_percent\":{},\"slack_floor\":{},\
             \"workers\":{},\"ticks\":{},\"patched\":{},\"rebuilt\":{},\"unchanged\":{},\"rebuild_ms\":{:.3},\
             \"median_ms\":{:.3},\"p95_ms\":{:.3},\"p99_ms\":{:.3},\"worst_ms\":{:.3},\"order_slots\":{}}}",
            self.arrival.label(),
            self.bodies,
            self.crossers,
            self.slack,
            self.floor,
            self.workers,
            self.ticks,
            self.patched,
            self.rebuilt,
            self.unchanged,
            self.rebuild_ms,
            self.median_ms,
            self.p95_ms,
            self.p99_ms,
            self.worst_ms,
            self.order_slots
        )
    }
}

/// Build a crowd, then teleport `crossers` random bodies per tick and time the incremental rebuild.
fn measure(
    bodies: u32,
    crossers: u32,
    slack: u8,
    floor: u32,
    workers: usize,
    ticks: usize,
    arrival: Arrival,
    radius: f32,
) -> io::Result<Row> {
    let config = SpatialGridConfig::try_new(Vec3::splat(-EXTENT), Vec3::splat(EXTENT), CELL)
        .map_err(other)?;
    let mut grid = SpatialGrid::new(config)
        .map_err(other)?
        .with_threads(workers)
        .with_slack_percent(slack)
        .with_slack_floor(floor);
    let mut rng = Rng::new(0x5EED_1234_ABCD_0001_u64 ^ u64::from(bodies));
    for index in 0..bodies {
        grid.insert(GridEntry {
            id: NpcId::unique(index),
            position: random_position(&mut rng),
            velocity: Vec3::ZERO,
        });
    }
    let started = std::time::Instant::now();
    grid.build_measured();
    let rebuild_ms = started.elapsed().as_secs_f64() * 1_000.0;
    let order_slots = grid.capacity();

    let mut ticks_ms = Vec::with_capacity(ticks);
    let mut patched = 0_usize;
    let mut rebuilt = 0_usize;
    let mut unchanged = 0_usize;
    for tick in 0..(WARMUP + ticks) {
        // One hot point per tick: in cluster mode every arrival lands inside the cell it falls in, which is a crowd
        // pouring through one door rather than a crowd teleporting uniformly.
        let hot = random_position(&mut rng);
        for _ in 0..crossers {
            let index = rng.below(bodies);
            let position = match arrival {
                Arrival::Random => random_position(&mut rng),
                Arrival::Cluster => hot + jitter(&mut rng, radius),
            };
            if let Some(entry) = grid.entries_mut().get_mut(index as usize) {
                entry.position = position;
            }
        }
        let started = std::time::Instant::now();
        let outcome = grid.build_incremental();
        let elapsed = started.elapsed().as_secs_f64() * 1_000.0;
        if tick < WARMUP {
            continue;
        }
        ticks_ms.push(elapsed);
        match classify(outcome) {
            RebuildOutcomeKind::Patched => patched += 1,
            RebuildOutcomeKind::Rebuilt => rebuilt += 1,
            RebuildOutcomeKind::Unchanged => unchanged += 1,
        }
    }
    ticks_ms.sort_by(f64::total_cmp);
    Ok(Row {
        bodies,
        crossers,
        arrival,
        slack,
        floor,
        workers,
        ticks,
        patched,
        rebuilt,
        unchanged,
        rebuild_ms,
        median_ms: percentile(&ticks_ms, 50),
        p95_ms: percentile(&ticks_ms, 95),
        p99_ms: percentile(&ticks_ms, 99),
        worst_ms: ticks_ms.last().copied().unwrap_or(0.0),
        order_slots,
    })
}

/// The three outcomes, flattened so this harness does not need to name the enum's payloads.
enum RebuildOutcomeKind {
    Patched,
    Rebuilt,
    Unchanged,
}

/// Split an outcome into the facts this harness reports. The payloads are what the tests assert on; the shape is all a
/// latency sweep needs.
///
/// Written as a match on the public enum rather than on an unstable discriminant so a change to the payloads is a
/// compile error here too.
const fn classify(outcome: bevy_flock::domain::spatial::RebuildOutcome) -> RebuildOutcomeKind {
    match outcome {
        bevy_flock::domain::spatial::RebuildOutcome::Unchanged => RebuildOutcomeKind::Unchanged,
        bevy_flock::domain::spatial::RebuildOutcome::Patched { .. } => RebuildOutcomeKind::Patched,
        bevy_flock::domain::spatial::RebuildOutcome::Rebuilt { .. } => RebuildOutcomeKind::Rebuilt,
    }
}

fn percent_of(part: usize, whole: usize) -> f64 {
    if whole == 0 {
        return 0.0;
    }
    num::usize_to_f64(part) * 100.0 / num::usize_to_f64(whole)
}

/// The nearest-rank percentile of a sorted slice, which is what a frame budget is read off.
fn percentile(sorted: &[f64], percent: usize) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let last = sorted.len().saturating_sub(1);
    let rank = last.saturating_mul(percent.min(100)) / 100;
    sorted.get(rank).copied().unwrap_or(0.0)
}

fn random_position(rng: &mut Rng) -> Vec3 {
    let unit = |rng: &mut Rng| num::u32_to_f32(rng.below(1_000_001)) / 1_000_000.0;
    Vec3::new(
        unit(rng) * EXTENT * 2.0 - EXTENT,
        unit(rng) * EXTENT * 2.0 - EXTENT,
        unit(rng) * EXTENT * 2.0 - EXTENT,
    )
}

/// A splitmix64, because a benchmark should not pull a dependency in to be reproducible.
struct Rng(u64);

impl Rng {
    const fn new(seed: u64) -> Self {
        Self(seed)
    }

    const fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A value in `0..bound`, or zero for an empty range.
    fn below(&mut self, bound: u32) -> u32 {
        if bound == 0 {
            return 0;
        }
        let draw = self.next_u64() >> 32;
        num::u64_to_u32(draw % u64::from(bound))
    }
}

/// The command line, parsed by hand: four flags and no dependency is the right trade for a harness.
struct Args {
    bodies: Vec<u32>,
    crossers: Vec<u32>,
    arrival: Vec<Arrival>,
    /// How wide the hotspot is in cells, for the clustered arrival pattern.
    radius: f32,
    slack: Vec<u8>,
    floor: Vec<u32>,
    workers: Vec<usize>,
    ticks: usize,
    json: bool,
}

impl Args {
    fn parse(args: impl Iterator<Item = String>) -> io::Result<Self> {
        let mut parsed = Self {
            bodies: vec![100_000, 1_000_000],
            crossers: vec![64, 512, 2_048],
            arrival: vec![Arrival::Random, Arrival::Cluster],
            radius: 4.0,
            slack: vec![25, 50],
            floor: vec![0, 2],
            workers: vec![6],
            ticks: DEFAULT_TICKS,
            json: false,
        };
        let mut pending: Option<String> = None;
        for arg in args {
            let Some(flag) = pending.take() else {
                if let Some(flag) = arg.strip_prefix("--") {
                    if flag == "json" {
                        parsed.json = true;
                    } else {
                        pending = Some(flag.to_owned());
                    }
                    continue;
                }
                return Err(invalid(format!("unexpected argument `{arg}`")));
            };
            if flag == "json" {
                parsed.json = true;
                pending = Some(flag);
                continue;
            }
            match flag.as_str() {
                "bodies" => parsed.bodies = numbers(&arg)?,
                "crossers" => parsed.crossers = numbers(&arg)?,
                "slack" => parsed.slack = percents(&arg)?,
                "floor" => parsed.floor = numbers(&arg)?,
                "workers" => parsed.workers = counts(&arg)?,
                "ticks" => parsed.ticks = single(&arg)?,
                "arrival" => parsed.arrival = arrivals(&arg)?,
                "cluster-radius" => parsed.radius = single_float(&arg)?,
                other => return Err(invalid(format!("unknown flag `--{other}`"))),
            }
        }
        Ok(parsed)
    }
}

/// A comma-separated list of arrival patterns.
fn arrivals(text: &str) -> io::Result<Vec<Arrival>> {
    let mut values = Vec::new();
    for piece in text.split(',') {
        let piece = piece.trim();
        let arrival = Arrival::parse(piece)
            .ok_or_else(|| invalid(format!("`{piece}` is neither `random` nor `cluster`")))?;
        values.push(arrival);
    }
    if values.is_empty() {
        return Err(invalid(
            "an empty list was given where an arrival pattern was expected".to_owned(),
        ));
    }
    Ok(values)
}

/// One non-negative float, for the flags that describe the fixture rather than sweep a value.
fn single_float(text: &str) -> io::Result<f32> {
    let value: f32 = text
        .trim()
        .parse()
        .map_err(|_| invalid(format!("`{text}` is not a number")))?;
    if !value.is_finite() || value < 0.0 {
        return Err(invalid(format!("`{text}` is not a distance")));
    }
    Ok(value)
}

fn numbers(text: &str) -> io::Result<Vec<u32>> {
    let mut values = Vec::new();
    for piece in text.split(',') {
        let value: u32 = piece
            .trim()
            .parse()
            .map_err(|_| invalid(format!("`{piece}` is not a count")))?;
        values.push(value);
    }
    if values.is_empty() {
        return Err(invalid(
            "an empty list was given where a count was expected".to_owned(),
        ));
    }
    Ok(values)
}

fn percents(text: &str) -> io::Result<Vec<u8>> {
    numbers(text)?
        .into_iter()
        .map(|value| {
            u8::try_from(value).map_err(|_| invalid(format!("`{value}` is not a percentage")))
        })
        .collect()
}

/// A comma-separated list of counts, for the flags that sweep rather than fix a value.
fn counts(text: &str) -> io::Result<Vec<usize>> {
    let mut values = Vec::new();
    for piece in text.split(',') {
        values.push(single(piece)?);
    }
    if values.is_empty() {
        return Err(invalid(
            "an empty list was given where a count was expected".to_owned(),
        ));
    }
    Ok(values)
}

fn single(text: &str) -> io::Result<usize> {
    text.trim()
        .parse()
        .map_err(|_| invalid(format!("`{text}` is not a number")))
}

fn invalid(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn other(error: impl std::fmt::Display) -> io::Error {
    invalid(error.to_string())
}
