//! The cross-engine fixture: one frame of the whole crowd, reported in the shared JSON schema.
//!
//! This is the `bevy-flock` side of `docs/dev/tuning/benchmarks/cross-engine.md`. It runs the documented workload — a spawn burst
//! followed by steady-state ticks — and reports the same six statistics every other engine's harness must report, so the
//! numbers can be put in one table instead of two blog posts.
//!
//! ```text
//! cargo run --release --example cross_bench -- --agents 10000,100000 --ticks 1000
//! cargo run --release --example cross_bench -- --agents 1000000 --ticks 200 --json
//! cargo run --release --example cross_bench -- --workload matched --agents 10000,100000
//! cargo run --release --example cross_bench -- --workload all --agents 100000 --workers 1,2,4,6 --json
//! ```
//!
//! The frame measured is `App::update` over the plugin's own schedule, which is the closest thing this crate has to the
//! number a game loop sees — not a kernel microbenchmark. It excludes everything the crate does not ship: rendering,
//! animation, audio, networking. The Godot and Unreal harnesses in the same document measure their engine's equivalent
//! frame, and the document says in one place what that asymmetry is worth.
//!
//! Every row names the CPU it ran on, on Linux, macOS and Windows alike, because `compare_benchmarks.py` refuses to
//! put two machines in one table and a harness that cannot name its own machine defeats that check.
//!
//! # Two workloads, because one number cannot be attributed
//!
//! `W1-fixed-tick` is the product-level row: the whole plugin, every agent steered, no band and no culling. When it
//! comes out behind another engine, that row cannot say whether the difference is the schedule, the preset, or the
//! arithmetic, because all three are inside it.
//!
//! `matched` adds `W2-matched-bare` and `W2-matched-ecs`, which are the Unreal harness's loop written twice: same
//! 1,000 m cube, same 10 m cells, same eight-neighbour bound, same seek and separation, same 1/60 s step. The bare row
//! calls that loop directly; the ECS row calls the identical functions from one system in `Update` on a plain
//! `App::new()`. The differences between the three medians are the answer:
//!
//! - `ecs - bare` is what the ECS and its schedule cost for the same work.
//! - `plugin - ecs` is what this crate does beyond the matched loop: the bandless ambient preset's steering, needs,
//!   cognition scheduling, and the wider twenty-seven-cell search the real grid performs.
//!
//! The two matched rows are asserted bit-identical by a test, so the ECS row cannot be paying for a different
//! trajectory than the bare one.
use bevy_app::prelude::*;
use bevy_app::{TaskPoolOptions, TaskPoolPlugin, TaskPoolThreadAssignmentPolicy};
use bevy_ecs::prelude::Resource;
use bevy_ecs::system::ResMut;
use bevy_flock::adapters::primary::plugin::{CrowdPreset, FlockConfig, FlockPlugin};
use bevy_flock::domain::ids::SimSeed;
use bevy_flock::domain::num;
use bevy_flock::domain::population::{PopulationMode, PopulationSize};
use bevy_math::Vec3;
use bevy_math::ops;
use std::time::Instant;

/// The frame budget every harness reports against, in milliseconds: 60 Hz.
const BUDGET_MS: f64 = 16.6;

/// Which workload to run. `W1` is the product row; the two matched rows exist to attribute it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Workload {
    FixedTick,
    MatchedBare,
    MatchedEcs,
}

impl Workload {
    /// The `workload` string in the JSON row, shared with the Godot and Unreal harnesses.
    const fn label(self) -> &'static str {
        match self {
            Self::FixedTick => "W1-fixed-tick",
            Self::MatchedBare => "W2-matched-bare",
            Self::MatchedEcs => "W2-matched-ecs",
        }
    }

    const fn note(self) -> &'static str {
        match self {
            Self::FixedTick => {
                "App::update over the plugin schedule; no rendering, animation, physics or audio"
            }
            Self::MatchedBare => {
                "matched fixture: 1000 m cube, 10 m cells, <=8 same-cell neighbours, 1/60 s step, called directly"
            }
            Self::MatchedEcs => {
                "matched fixture: same loop as W2-matched-bare, one system in Update on a plain App::new()"
            }
        }
    }

    /// `w1`, `matched`, `matched-bare`, `matched-ecs` or `all`, comma-separated. Unknown names are reported rather than
    /// silently dropped, because a benchmark that quietly runs something else is worse than one that fails.
    fn parse(list: &str) -> Vec<Self> {
        let mut chosen: Vec<Self> = Vec::new();
        for name in list.split(',').map(str::trim).filter(|n| !n.is_empty()) {
            let group: &[Self] = match name {
                "w1" | "fixed-tick" => &[Self::FixedTick],
                "matched-bare" => &[Self::MatchedBare],
                "matched-ecs" => &[Self::MatchedEcs],
                "matched" => &[Self::MatchedBare, Self::MatchedEcs, Self::FixedTick],
                "all" => &[Self::FixedTick, Self::MatchedBare, Self::MatchedEcs],
                other => {
                    eprintln!("ignoring unknown workload {other}");
                    continue;
                }
            };
            for workload in group {
                if !chosen.contains(workload) {
                    chosen.push(*workload);
                }
            }
        }
        if chosen.is_empty() {
            chosen.push(Self::FixedTick);
        }
        chosen
    }
}

struct Args {
    agents: Vec<usize>,
    ticks: usize,
    warmup: usize,
    json: bool,
    workloads: Vec<Workload>,
    /// Requested grid-build worker counts. Empty means "let the plugin choose", which is the canonical row.
    workers: Vec<Option<usize>>,
    /// Requested compute-pool thread counts. Empty means "no `TaskPoolPlugin`", which is the canonical row.
    pools: Vec<usize>,
    /// The pool shape this process was re-executed with, if any.
    pool: Option<usize>,
}

fn args() -> Args {
    let mut parsed = Args {
        agents: vec![10_000],
        ticks: 1_000,
        warmup: 100,
        json: false,
        workloads: vec![Workload::FixedTick],
        workers: Vec::new(),
        pools: Vec::new(),
        pool: None,
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
            "--workload" => {
                if let Some(list) = rest.next() {
                    parsed.workloads = Workload::parse(&list);
                }
            }
            "--workers" => {
                if let Some(list) = rest.next() {
                    let counts: Vec<usize> = list
                        .split(',')
                        .filter_map(|v| v.trim().parse().ok())
                        .collect();
                    if !counts.is_empty() {
                        parsed.workers = counts.into_iter().map(Some).collect();
                    }
                }
            }
            "--pool" => {
                if let Some(list) = rest.next() {
                    let counts: Vec<usize> = list
                        .split(',')
                        .filter_map(|v| v.trim().parse().ok())
                        .filter(|n| *n > 0)
                        .collect();
                    match counts.as_slice() {
                        [] => {}
                        [single] => parsed.pool = Some(*single),
                        many => parsed.pools = many.to_vec(),
                    }
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

/// The six statistics every row reports, computed once so every workload is summarised by the same code.
struct Row {
    median: f64,
    p95: f64,
    p99: f64,
    max: f64,
    over_pct: f64,
}

impl Row {
    fn from_samples(samples: &mut [f64]) -> Self {
        samples.sort_by(f64::total_cmp);
        let over = samples.iter().filter(|sample| **sample > BUDGET_MS).count();
        // Percentages as integer ratios so no count is widened to f64 to divide by another count.
        let over_pct = if samples.is_empty() {
            0.0
        } else {
            num::usize_to_f64(over * 1000 / samples.len()) / 10.0
        };
        Self {
            median: percentile(samples, 50, 100),
            p95: percentile(samples, 95, 100),
            p99: percentile(samples, 99, 100),
            max: samples.last().copied().unwrap_or(0.0),
            over_pct,
        }
    }

    fn ns_per_agent(&self, agents: usize) -> f64 {
        self.median * 1e6 / num::usize_to_f64(agents)
    }
}

/// What distinguishes one row from another: the label a table prints, the note that admits what the row includes, and the
/// two knobs that produced it. Grouped rather than threaded through `report` as four parameters, which is one more than a
/// reader can keep in order.
struct RowId {
    label: String,
    note: String,
    workers: Option<usize>,
    pool: Option<usize>,
}

impl RowId {
    /// The canonical row for a workload, with no knob forced.
    fn of(workload: Workload) -> Self {
        Self {
            label: workload.label().to_owned(),
            note: workload.note().to_owned(),
            workers: None,
            pool: None,
        }
    }
}

fn report(id: &RowId, agents: usize, ticks: usize, warmup: usize, row: &Row, json: bool) {
    // `null` rather than `0`: a row that does not choose a worker count, and a row where the question does not apply,
    // are both "not chosen", and the plugin's own `0` already means "decide for me".
    let workers_field = id
        .workers
        .map_or_else(|| "null".to_owned(), |count| count.to_string());
    let pool_field = id
        .pool
        .map_or_else(|| "null".to_owned(), |count| count.to_string());
    if json {
        println!(
            "{{\"engine\":\"bevy-flock\",\"engine_version\":\"{}\",\"workload\":\"{}\",\"agents\":{},\
             \"ticks\":{},\"warmup\":{},\"budget_ms\":{},\"median_ms\":{:.3},\"p95_ms\":{:.3},\"p99_ms\":{:.3},\
             \"max_ms\":{:.3},\"over_budget_pct\":{:.2},\"ns_per_agent\":{:.0},\"machine\":\"{}\",\
             \"workers\":{},\"pool_threads\":{},\"notes\":\"{}\"}}",
            env!("CARGO_PKG_VERSION"),
            id.label,
            agents,
            ticks,
            warmup,
            BUDGET_MS,
            row.median,
            row.p95,
            row.p99,
            row.max,
            row.over_pct,
            row.ns_per_agent(agents),
            machine(),
            workers_field,
            pool_field,
            id.note,
        );
    } else {
        println!(
            "{agents} agents [{}]: median {:.3} ms, p95 {:.3}, p99 {:.3}, max {:.3}, \
             {:.2}% over {BUDGET_MS} ms, {:.0} ns/agent",
            id.label,
            row.median,
            row.p95,
            row.p99,
            row.max,
            row.over_pct,
            row.ns_per_agent(agents),
        );
    }
}

fn machine() -> String {
    let cpu = cpu_name().unwrap_or_else(|| "unknown cpu".to_owned());
    let cores = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    format!("{cpu}, {cores} threads")
}

/// The CPU model, spelled the way the platform spells it.
///
/// This is reported in every row and `scripts/compare_benchmarks.py` refuses to compare rows that name different
/// machines, so a harness that cannot name its own CPU silently disables that check. Reading `/proc/cpuinfo`
/// unconditionally did exactly that on macOS, where the file does not exist: every row said `unknown cpu` and the
/// Godot row on the same laptop said `Apple M3 Pro`, which is not a comparison the script should have to guess at.
fn cpu_name() -> Option<String> {
    if cfg!(target_os = "linux") {
        // x86 says "model name", aarch64 says "Processor" or nothing at all.
        std::fs::read_to_string("/proc/cpuinfo")
            .ok()
            .as_deref()
            .and_then(parse_cpu_info)
    } else if cfg!(target_os = "macos") {
        command_stdout("sysctl", &["-n", "machdep.cpu.brand_string"])
    } else if cfg!(target_os = "windows") {
        std::env::var("PROCESSOR_IDENTIFIER")
            .ok()
            .as_deref()
            .and_then(clean)
    } else {
        None
    }
}

fn parse_cpu_info(text: &str) -> Option<String> {
    ["model name", "Processor", "Hardware"]
        .iter()
        .find_map(|key| {
            text.lines()
                .find(|line| line.starts_with(key))
                .and_then(|line| line.split(':').nth(1))
                .and_then(clean)
        })
}

fn command_stdout(program: &str, arguments: &[&str]) -> Option<String> {
    let output = std::process::Command::new(program)
        .args(arguments)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    std::str::from_utf8(&output.stdout).ok().and_then(clean)
}

/// Trim, and treat an empty answer as no answer rather than as a machine called " ".
fn clean(name: &str) -> Option<String> {
    let trimmed = name.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// The plugin with a compute pool of exactly `threads`, which a default host only gets by accident.
///
/// `percent: 0.0` with equal bounds asks for exactly that many threads: the percentage contributes nothing and the
/// clamp does the work. `io` and `async_compute` are pinned to one thread each so the row measures the compute pool
/// instead of three pools' worth of idle threads.
fn task_pool_plugin(threads: usize) -> TaskPoolPlugin {
    let policy = || TaskPoolThreadAssignmentPolicy {
        min_threads: threads,
        max_threads: threads,
        percent: 0.0,
        on_thread_spawn: None,
        on_thread_destroy: None,
    };
    let single = TaskPoolThreadAssignmentPolicy {
        min_threads: 1,
        max_threads: 1,
        percent: 0.0,
        on_thread_spawn: None,
        on_thread_destroy: None,
    };
    TaskPoolPlugin {
        task_pool_options: TaskPoolOptions {
            min_total_threads: 1,
            max_total_threads: usize::MAX,
            io: single.clone(),
            async_compute: single,
            compute: policy(),
        },
    }
}

fn fixed_tick_config(agents: usize, workers: Option<usize>) -> FlockConfig {
    let mut config = FlockConfig {
        population: PopulationSize::clamped(agents),
        spawn_budget: 1_000_000,
        mode: PopulationMode::Unique,
        seed: SimSeed::new(0xC0FF_EE00),
        ..FlockConfig::preset(CrowdPreset::CityAmbient)
    };
    if let Some(workers) = workers {
        config.boids = config.boids.with_workers(workers);
    }
    config
}

fn run_fixed_tick(
    agents: usize,
    ticks: usize,
    warmup: usize,
    json: bool,
    workers: Option<usize>,
    pool: Option<usize>,
) -> Row {
    let config = fixed_tick_config(agents, workers);
    let mut app = App::new();
    if let Some(threads) = pool {
        app.add_plugins(task_pool_plugin(threads));
    }
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
    let row = Row::from_samples(&mut samples);
    let mut id = RowId::of(Workload::FixedTick);
    if let Some(workers) = workers {
        id.label = format!("{}-{workers}w", Workload::FixedTick.label());
        id.note.push_str("; grid build pinned to ");
        id.note.push_str(&workers.to_string());
        id.note
            .push_str(if workers == 1 { " worker" } else { " workers" });
    }
    if let Some(threads) = pool {
        id.label = workers.map_or_else(
            || format!("{}-pool{threads}", Workload::FixedTick.label()),
            |workers| format!("{}-{workers}w-pool{threads}", Workload::FixedTick.label()),
        );
        id.note.push_str("; compute pool pinned to ");
        id.note.push_str(&threads.to_string());
        id.note
            .push_str(if threads == 1 { " thread" } else { " threads" });
    }
    id.workers = workers;
    id.pool = pool;
    report(&id, agents, ticks, warmup, &row, json);
    row
}

// ----------------------------------------------------------------------------------------------------------------
// W2: the matched fixture, which is Unreal's subsystem loop written twice (bare, and inside an ECS system)
// ----------------------------------------------------------------------------------------------------------------

/// Half the cube's side, in metres. The fixture spans 1,000 m on every axis, as `benchmarks/unreal` does.
const MATCH_HALF_EXTENT: f32 = 500.0;
/// Cell size before the grid divides the cube: 1,000 m over 10 m is the 100x100x100 grid the Unreal harness builds.
const MATCH_CELL_SIZE: f32 = 10.0;
/// Every body moves at one speed, as in the Unreal fixture.
const MATCH_SPEED: f32 = 4.0;
/// How much of a body's velocity is replaced by the seek each tick.
const MATCH_SEEK_RATE: f32 = 0.05;
/// The separation impulse, per neighbour.
const MATCH_SEPARATION_PUSH: f32 = 0.05;
/// One fixed step, so the simulation does not depend on how fast the harness happens to run.
const MATCH_STEP_SECONDS: f32 = 1.0 / 60.0;
/// Neighbours read per body: an unbounded search would make this a density measurement rather than a crowd one.
const MATCH_MAX_NEIGHBOURS: usize = 8;
/// The seed the Unreal and Godot harnesses use. Same distribution, not the same sample: no two engines draw alike.
const MATCH_SEED: u64 = 0x00C0_FFEE;
/// Below this squared distance, a separation direction is noise rather than a direction.
const MATCH_TINY_SQUARED: f32 = 1.0e-8;

/// A counting-sort grid with the same slots `SpatialGrid` uses: `cell_start[cell]` is where that cell's bodies begin and
/// `cell_start[cell + 1]` is where they end, so the last slot is the total.
///
/// The three buffers are allocated once and reused, because a steady-state rebuild that allocates would show up in
/// every row and flatter whichever engine allocates least.
struct MatchedGrid {
    axis: usize,
    inverse_cell: f32,
    cell_start: Vec<u32>,
    cell_cursor: Vec<u32>,
    sorted: Vec<u32>,
}

impl MatchedGrid {
    fn new(axis: usize, cell: f32, agents: usize) -> Self {
        let cells = axis.saturating_mul(axis).saturating_mul(axis);
        Self {
            axis,
            inverse_cell: 1.0 / cell,
            cell_start: vec![0; cells + 1],
            cell_cursor: vec![0; cells],
            sorted: vec![0; agents],
        }
    }

    /// The grid the fixture calls for: the cube divided by the cell size, with the divisor recomputed from the axis so
    /// the last cell is never a sliver. `1,000 / 10` is exact, so the recomputed divisor is the same 10 m.
    fn for_agents(agents: usize) -> Self {
        let span = MATCH_HALF_EXTENT * 2.0;
        let divisor = (span / MATCH_CELL_SIZE).max(1.0);
        let axis = num::f32_to_usize(divisor).max(1);
        Self::new(axis, span / divisor, agents)
    }

    fn cell_of(&self, position: Vec3) -> usize {
        let last = self.axis.saturating_sub(1);
        let axis = |value: f32| {
            num::f32_to_usize((value + MATCH_HALF_EXTENT) * self.inverse_cell).min(last)
        };
        let (x, y, z) = (axis(position.x), axis(position.y), axis(position.z));
        (x * self.axis + y) * self.axis + z
    }

    /// Count, prefix, then scatter: the three passes `SpatialGrid::build` runs, in the same order and with the same
    /// slot arithmetic, because a comparison against it has to be the same shape of work.
    ///
    /// The prefix and the cursor copy walk slices rather than indexing, so this row is not slower than the engine it is
    /// compared with for a reason that has nothing to do with the engine: a bounds check and a reload per cell across
    /// a million cells is a millisecond of nothing.
    fn rebuild(&mut self, positions: &[Vec3]) {
        self.cell_start.fill(0);
        for position in positions {
            let cell = self.cell_of(*position);
            if let Some(count) = self.cell_start.get_mut(cell + 1) {
                *count += 1;
            }
        }

        // The running total is carried in a register rather than re-read from the previous slot.
        let mut running = 0u32;
        for count in self.cell_start.iter_mut().skip(1) {
            running += *count;
            *count = running;
        }

        // The cursor starts where each cell starts, which is the prefix array minus the slot holding the total.
        let cells = self.cell_start.len().saturating_sub(1);
        if let (Some(cursor), Some(starts)) = (
            self.cell_cursor.get_mut(..cells),
            self.cell_start.get(..cells),
        ) {
            cursor.copy_from_slice(starts);
        }
        for (index, position) in positions.iter().enumerate() {
            let cell = self.cell_of(*position);
            let Some(target) = self.cell_cursor.get(cell).copied() else {
                continue;
            };
            if let Some(slot) = self.sorted.get_mut(num::u32_to_usize(target)) {
                *slot = num::usize_to_u32(index);
            }
            if let Some(cursor) = self.cell_cursor.get_mut(cell) {
                *cursor += 1;
            }
        }
    }

    /// The slots a body may read: `begin .. min(end, begin + MATCH_MAX_NEIGHBOURS)`.
    fn window(&self, cell: usize) -> (usize, usize) {
        let begin = num::u32_to_usize(self.cell_start.get(cell).copied().unwrap_or(0));
        let end = num::u32_to_usize(self.cell_start.get(cell + 1).copied().unwrap_or(0));
        (begin, end.min(begin.saturating_add(MATCH_MAX_NEIGHBOURS)))
    }
}

/// One tick of Unreal's `Step()`: seek the origin, read at most eight same-cell neighbours, separate from them,
/// integrate, clamp to the cube.
fn matched_step(grid: &MatchedGrid, positions: &mut [Vec3], velocities: &mut [Vec3]) {
    for index in 0..positions.len() {
        let Some(current) = positions.get(index).copied() else {
            continue;
        };
        let desired = (-current).normalize_or_zero() * MATCH_SPEED;
        let previous = velocities.get(index).copied().unwrap_or(Vec3::ZERO);
        let mut velocity = previous.lerp(desired, MATCH_SEEK_RATE);

        let (begin, end) = grid.window(grid.cell_of(current));
        for slot in begin..end {
            let Some(other) = grid.sorted.get(slot).copied() else {
                continue;
            };
            let other = num::u32_to_usize(other);
            if other == index {
                continue;
            }
            let Some(other_position) = positions.get(other).copied() else {
                continue;
            };
            let away = current - other_position;
            let distance_squared = away.length_squared();
            if distance_squared > MATCH_TINY_SQUARED
                && distance_squared < MATCH_CELL_SIZE * MATCH_CELL_SIZE
            {
                velocity += away.normalize_or_zero() * MATCH_SEPARATION_PUSH;
            }
        }

        let moved = current + velocity * MATCH_STEP_SECONDS;
        let limit = Vec3::splat(MATCH_HALF_EXTENT);
        if let Some(velocity_slot) = velocities.get_mut(index) {
            *velocity_slot = velocity;
        }
        if let Some(position_slot) = positions.get_mut(index) {
            *position_slot = moved.clamp(-limit, limit);
        }
    }
}

fn matched_tick(grid: &mut MatchedGrid, positions: &mut [Vec3], velocities: &mut [Vec3]) {
    grid.rebuild(positions);
    matched_step(grid, positions, velocities);
}

/// A 64-bit xorshift. The fixture needs reproducibility, not statistical quality, and a random-number dependency for one
/// drawing would be a dependency in every consumer's build.
struct Rng(u64);

impl Rng {
    const fn new(seed: u64) -> Self {
        // A zero state is a fixed point of xorshift, so the seed is never allowed to be one.
        Self(seed | 1)
    }

    /// A value in `[0, 1)`, built from 23 random bits so the conversion to `f32` is exact rather than a cast.
    fn fraction(&mut self) -> f32 {
        let mut state = self.0;
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        self.0 = state;
        let mantissa = num::u64_to_low_u32(state >> 41) & 0x007F_FFFF;
        f32::from_bits(0x3F80_0000 | mantissa) - 1.0
    }
}

/// The spawn the other harnesses use: uniform over the cube, heading uniform in the plane, one speed.
fn matched_population(agents: usize) -> (Vec<Vec3>, Vec<Vec3>) {
    let mut rng = Rng::new(MATCH_SEED);
    let mut positions = Vec::with_capacity(agents);
    let mut velocities = Vec::with_capacity(agents);
    for _ in 0..agents {
        let span = MATCH_HALF_EXTENT * 2.0;
        positions.push(Vec3::new(
            -MATCH_HALF_EXTENT + span * rng.fraction(),
            -MATCH_HALF_EXTENT + span * rng.fraction(),
            -MATCH_HALF_EXTENT + span * rng.fraction(),
        ));
        let angle = std::f32::consts::TAU * rng.fraction();
        velocities.push(Vec3::new(
            ops::cos(angle) * MATCH_SPEED,
            ops::sin(angle) * MATCH_SPEED,
            0.0,
        ));
    }
    (positions, velocities)
}

fn run_matched_bare(agents: usize, ticks: usize, warmup: usize, json: bool) -> Row {
    let (mut positions, mut velocities) = matched_population(agents);
    let mut grid = MatchedGrid::for_agents(agents);
    for _ in 0..warmup {
        matched_tick(&mut grid, &mut positions, &mut velocities);
    }

    let mut samples: Vec<f64> = Vec::with_capacity(ticks);
    for _ in 0..ticks {
        let started = Instant::now();
        matched_tick(&mut grid, &mut positions, &mut velocities);
        samples.push(started.elapsed().as_secs_f64() * 1e3);
    }
    let row = Row::from_samples(&mut samples);
    report(
        &RowId::of(Workload::MatchedBare),
        agents,
        ticks,
        warmup,
        &row,
        json,
    );
    row
}

/// The same population and the same two functions, driven by one system in `Update` on a plain `App::new()`: no plugins,
/// no fixed-timestep schedule, no change detection. The floor for an ECS-driven loop.
#[derive(Resource)]
struct MatchedCrowd {
    positions: Vec<Vec3>,
    velocities: Vec<Vec3>,
    grid: MatchedGrid,
}

fn matched_ecs_tick(mut crowd: ResMut<MatchedCrowd>) {
    let MatchedCrowd {
        positions,
        velocities,
        grid,
    } = &mut *crowd;
    matched_tick(grid, positions, velocities);
}

fn run_matched_ecs(agents: usize, ticks: usize, warmup: usize, json: bool) -> Row {
    let (positions, velocities) = matched_population(agents);
    let mut app = App::new();
    app.insert_resource(MatchedCrowd {
        positions,
        velocities,
        grid: MatchedGrid::for_agents(agents),
    });
    app.add_systems(Update, matched_ecs_tick);
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
    let row = Row::from_samples(&mut samples);
    report(
        &RowId::of(Workload::MatchedEcs),
        agents,
        ticks,
        warmup,
        &row,
        json,
    );
    row
}

/// What the three rows mean once they exist. Printed only for a human: the JSON rows are the record.
fn attribute(agents: usize, bare: &Row, ecs: &Row, plugin: Option<&Row>) {
    let overhead = ecs.median - bare.median;
    println!(
        "  attribution at {agents} agents: the ECS and its schedule cost {overhead:+.3} ms \
         per tick over the same loop ({:.1}% of the matched tick)",
        share(overhead, ecs.median),
    );
    if let Some(plugin) = plugin {
        let extra = plugin.median - ecs.median;
        println!(
            "  attribution at {agents} agents: the plugin adds {extra:+.3} ms per tick over the matched loop \
             ({:.1}% of the plugin row, {:.2}x the matched tick)",
            share(extra, plugin.median),
            ratio(plugin.median, ecs.median),
        );
    }
}

fn share(part: f64, whole: f64) -> f64 {
    if whole > 0.0 {
        part / whole * 100.0
    } else {
        0.0
    }
}

fn ratio(numerator: f64, denominator: f64) -> f64 {
    if denominator > 0.0 {
        numerator / denominator
    } else {
        0.0
    }
}

/// Re-runs this binary once per requested pool shape.
///
/// `ComputeTaskPool` is a process-wide `OnceLock`, so the first pool created in a process wins and every later
/// `TaskPoolPlugin` is ignored. A sweep inside one process would report the first shape N times, which is the kind of
/// plausible-looking wrong answer this harness exists to avoid. One process per shape costs a spawn and makes the
/// number real.
fn respawn_for_pools(pools: &[usize]) -> Result<(), String> {
    let exe =
        std::env::current_exe().map_err(|error| format!("cannot find this binary: {error}"))?;
    for pool in pools {
        let mut command = std::process::Command::new(&exe);
        let mut skip_next = false;
        for arg in std::env::args().skip(1) {
            if skip_next {
                skip_next = false;
                continue;
            }
            if arg == "--pool" {
                skip_next = true;
                continue;
            }
            command.arg(arg);
        }
        command.arg("--pool").arg(pool.to_string());
        let status = command
            .status()
            .map_err(|error| format!("cannot run the pool child: {error}"))?;
        if !status.success() {
            return Err(format!("the {pool}-thread child exited with {status}"));
        }
    }
    Ok(())
}

fn main() -> std::process::ExitCode {
    let parsed = args();
    if !parsed.pools.is_empty() {
        if let Err(message) = respawn_for_pools(&parsed.pools) {
            eprintln!("pool sweep failed: {message}");
            return std::process::ExitCode::FAILURE;
        }
        return std::process::ExitCode::SUCCESS;
    }
    // An empty list means one canonical row with the plugin's own choice of workers.
    let workers = if parsed.workers.is_empty() {
        vec![None]
    } else {
        parsed.workers.clone()
    };
    for agents in parsed.agents {
        let mut bare: Option<Row> = None;
        let mut ecs: Option<Row> = None;
        let mut plugin: Option<Row> = None;
        for workload in &parsed.workloads {
            match workload {
                Workload::FixedTick => {
                    // The last row of a sweep is the one the attribution compares against.
                    for count in &workers {
                        plugin = Some(run_fixed_tick(
                            agents,
                            parsed.ticks,
                            parsed.warmup,
                            parsed.json,
                            *count,
                            parsed.pool,
                        ));
                    }
                }
                Workload::MatchedBare => {
                    bare = Some(run_matched_bare(
                        agents,
                        parsed.ticks,
                        parsed.warmup,
                        parsed.json,
                    ));
                }
                Workload::MatchedEcs => {
                    ecs = Some(run_matched_ecs(
                        agents,
                        parsed.ticks,
                        parsed.warmup,
                        parsed.json,
                    ));
                }
            }
        }
        if let (Some(bare), Some(ecs)) = (bare.as_ref(), ecs.as_ref()) {
            attribute(agents, bare, ecs, plugin.as_ref());
        }
    }
    std::process::ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::{
        MATCH_HALF_EXTENT, MATCH_MAX_NEIGHBOURS, MatchedCrowd, MatchedGrid, clean,
        matched_population, matched_tick, num, parse_cpu_info,
    };
    use bevy_app::prelude::{App, Update};
    use bevy_math::Vec3;

    /// Both spellings `/proc/cpuinfo` uses, because the label has to survive an ARM workstation as well as a laptop.
    #[test]
    fn the_cpu_name_is_read_from_either_key() {
        let x86 =
            "processor\t: 0\nmodel name\t: AMD Ryzen 9 7950X 16-Core Processor\nflags\t\t: fpu\n";
        assert_eq!(
            parse_cpu_info(x86).as_deref(),
            Some("AMD Ryzen 9 7950X 16-Core Processor")
        );
        let arm = "processor\t: 0\nBogoMIPS\t: 108.00\nHardware\t: BCM2835\n";
        assert_eq!(parse_cpu_info(arm).as_deref(), Some("BCM2835"));
    }

    /// No model line, or a blank one, is reported as no name: `unknown cpu` is a label, `" "` is a bug.
    #[test]
    fn a_cpuinfo_without_a_model_is_not_a_machine_name() {
        assert_eq!(parse_cpu_info("processor\t: 0\n"), None);
        assert_eq!(parse_cpu_info("model name\t:\n"), None);
        assert_eq!(clean("  \t "), None);
        assert_eq!(clean("Apple M3 Pro\n").as_deref(), Some("Apple M3 Pro"));
    }

    /// The whole point of two matched rows: the ECS wrapper must not change the trajectory. If it did, the difference
    /// between the rows would be arithmetic rather than schedule.
    #[test]
    fn the_ecs_row_produces_the_same_trajectory_as_the_bare_row() {
        let agents = 2_000;
        let (mut bare_positions, mut bare_velocities) = matched_population(agents);
        let mut bare_grid = MatchedGrid::for_agents(agents);
        for _ in 0..3 {
            matched_tick(&mut bare_grid, &mut bare_positions, &mut bare_velocities);
        }

        let (positions, velocities) = matched_population(agents);
        let mut app = App::new();
        app.insert_resource(MatchedCrowd {
            positions,
            velocities,
            grid: MatchedGrid::for_agents(agents),
        });
        app.add_systems(Update, super::matched_ecs_tick);
        for _ in 0..3 {
            app.update();
        }
        let crowd = app.world().resource::<MatchedCrowd>();

        assert_eq!(crowd.positions.len(), bare_positions.len());
        for (ecs_position, bare_position) in crowd.positions.iter().zip(bare_positions.iter()) {
            assert_eq!(ecs_position.x.to_bits(), bare_position.x.to_bits());
            assert_eq!(ecs_position.y.to_bits(), bare_position.y.to_bits());
            assert_eq!(ecs_position.z.to_bits(), bare_position.z.to_bits());
        }
    }

    /// A grid that loses or duplicates a body would make every row wrong in a way no statistic would show.
    #[test]
    fn the_matched_grid_places_every_body_once() {
        let agents = 5_000;
        let (positions, _) = matched_population(agents);
        let mut grid = MatchedGrid::for_agents(agents);
        grid.rebuild(&positions);

        assert_eq!(
            grid.cell_start.last().copied(),
            Some(num::usize_to_u32(agents))
        );
        assert!(
            grid.cell_start.windows(2).all(|pair| pair[0] <= pair[1]),
            "cell_start must be monotone: it is a prefix scan, not a count"
        );

        let mut seen = vec![false; agents];
        for index in &grid.sorted {
            let index = num::u32_to_usize(*index);
            assert_ne!(seen.get(index).copied(), Some(true), "body placed twice");
            if let Some(slot) = seen.get_mut(index) {
                *slot = true;
            }
        }
        assert!(seen.iter().all(|slot| *slot), "a body was left out");
    }

    /// The bound is what separates a crowd benchmark from a density measurement.
    #[test]
    fn no_window_reads_more_than_the_neighbour_bound() {
        let agents = 5_000;
        let (positions, _) = matched_population(agents);
        let mut grid = MatchedGrid::for_agents(agents);
        grid.rebuild(&positions);

        let cells = grid
            .axis
            .saturating_mul(grid.axis)
            .saturating_mul(grid.axis);
        let mut widest = 0;
        let mut occupied = 0;
        for cell in 0..cells {
            let (begin, end) = grid.window(cell);
            let size = end.saturating_sub(begin);
            widest = widest.max(size);
            if size > 0 {
                occupied += 1;
            }
        }
        assert!(widest <= MATCH_MAX_NEIGHBOURS, "widest window was {widest}");
        assert!(occupied > 0, "the fixture has no neighbours to read");
    }

    /// The cube is the world: a body that escapes it would leave the grid and never come back.
    #[test]
    fn the_matched_step_keeps_bodies_inside_the_cube() {
        let (mut positions, mut velocities) = matched_population(64);
        for (position, velocity) in positions.iter_mut().zip(velocities.iter_mut()) {
            *position = Vec3::splat(MATCH_HALF_EXTENT);
            *velocity = Vec3::new(50.0, 50.0, 50.0);
        }
        let mut grid = MatchedGrid::for_agents(64);
        matched_tick(&mut grid, &mut positions, &mut velocities);
        assert!(
            positions
                .iter()
                .all(|position| position.abs().max_element() <= MATCH_HALF_EXTENT)
        );
    }
}
