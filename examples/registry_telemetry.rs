//! Draws what a memory store is wasting.
//!
//! Simulates a world over a sequence of sessions — spawning eagerly, meeting rarely,
//! and retiring characters in two content patches — then prints the numbers and
//! writes an SVG with two graphs: accumulation over time, and which subjects the
//! references actually point at.
//!
//! ```text
//! cargo run --example registry_telemetry --features memory -- telemetry.svg
//! ```
//!
//! The measurement lives in `bevy_flock::domain::telemetry`, where it is tested; this
//! file is the simulation that feeds it and the drawing that explains it. No plotting
//! dependency, no floating-point in the measurements, and byte-identical output for
//! the same arguments, because a diagnostic that changes between runs cannot be used
//! to compare two builds.
#![expect(
    clippy::expect_used,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::needless_raw_string_hashes,
    clippy::too_many_lines,
    clippy::write_with_newline,
    reason = "a diagnostic that aborts loudly on a broken fixture is the useful kind, and the SVG \
              strings all carry the same hash count so a colour literal cannot terminate one"
)]
use std::fmt::Write as _;
use std::fs;

use bevy_flock::domain::ids::NpcId;
use bevy_flock::domain::memory::{Encounter, EncounterKind, MAX_SUBJECTS, MemoryStore, SubjectId};
use bevy_flock::domain::saves::{SubjectEntry, SubjectKey, SubjectKind, SubjectRegistry};
use bevy_flock::domain::telemetry::{Sample, measure, references_to, retire};
use bevy_flock::domain::tick::Tick;

/// Sessions of play to simulate.
const SESSIONS: usize = 12;

/// NPCs spawned per session.
const SPAWN_PER_SESSION: u32 = 4_000;

/// Subjects at the start: the player and fifteen recurring characters.
///
/// `MAX_SUBJECTS` is a hard ceiling in the store, not a suggestion: a slot array is
/// flat, and sixteen is where the flat array is still the right data structure. A
/// fixture that asked for more would silently lose every encounter with a subject
/// past the cap — which is exactly how this tool was debugged.
const SUBJECTS: u16 = MAX_SUBJECTS;

/// Subjects removed across the two content patches.
const RETIRED_TOTAL: u16 = 7;

/// Subjects retired at the start of these sessions, to create fragmentation.
const RETIREMENTS: [(usize, u16); 2] = [(4, 3), (8, 4)];

/// Store capacity, which is what the waste is measured against.
const CAPACITY: u32 = 60_000;

/// A deterministic stand-in for a game's spawn and encounter pattern.
struct Lcg(u64);

impl Lcg {
    const fn new(seed: u64) -> Self {
        Self(seed)
    }

    /// `SplitMix64`'s finalizer over a Weyl sequence.
    ///
    /// The obvious multiply-and-shift gives a stream whose low bits correlate, which
    /// showed up here as a fifth of arrivals meeting somebody instead of two fifths
    /// and as subjects clustering on half the identifier space — a fixture bug that
    /// made the graph flatter than the situation it was meant to show.
    fn next(&mut self) -> u32 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        (z ^ (z >> 31)) as u32
    }
}

fn initial_registry() -> SubjectRegistry {
    (0..SUBJECTS)
        .map(|index| {
            SubjectEntry::labelled(
                SubjectKey::new(0x1000 + u64::from(index)),
                if index == 0 {
                    SubjectKind::Player
                } else {
                    SubjectKind::Npc
                },
                format!("subject-{index:02}"),
            )
        })
        .collect()
}

fn main() {
    let output = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "telemetry.svg".to_owned());

    let mut store = MemoryStore::new(CAPACITY, SUBJECTS);
    let mut registry = initial_registry();
    let mut rng = Lcg::new(0x5EED_1234);
    let mut next_npc = 0_u32;
    let mut samples = Vec::with_capacity(SESSIONS);

    for session in 0..SESSIONS {
        for (when, count) in RETIREMENTS {
            if when == session {
                registry = retire(&registry, count);
            }
        }

        // Spawning enrols eagerly; meeting somebody is a minority of new arrivals,
        // which is what makes ghosts accumulate rather than merely appear.
        let mut arrival = Vec::with_capacity(SPAWN_PER_SESSION as usize);
        for _ in 0..SPAWN_PER_SESSION {
            let npc = NpcId::unique(next_npc);
            next_npc += 1;
            if let Some(index) = store.enrol(npc) {
                arrival.push(index);
            }
        }
        for index in arrival {
            if rng.next() % 5 < 2 {
                let subject = SubjectId::new((rng.next() % u32::from(SUBJECTS)) as u16);
                store.remember(
                    index,
                    subject,
                    &Encounter::new(EncounterKind::Spoke, Tick::new(session as u64 * 600)),
                );
            }
        }

        samples.push(measure(&store, &registry, session));
    }

    let per_subject: Vec<u32> = (0..SUBJECTS)
        .map(|raw| references_to(&store, raw))
        .collect();

    print_table(&samples);
    let svg = render(&samples, &per_subject);
    if let Err(error) = fs::write(&output, &svg) {
        eprintln!("could not write {output}: {error}");
        std::process::exit(1);
    }
    println!("\nwrote {output} ({} bytes)", svg.len());
}

fn print_table(samples: &[Sample]) {
    println!(
        "{:>4}  {:>8}  {:>8}  {:>8}  {:>7}  {:>9}  {:>8}  {:>9}",
        "sess", "enrolled", "ghosts", "ghost‰", "slots", "orphan‰", "subjects", "orphan‰"
    );
    for sample in samples {
        println!(
            "{:>4}  {:>8}  {:>8}  {:>8}  {:>7}  {:>9}  {:>8}  {:>9}",
            sample.session,
            sample.enrolled,
            sample.ghosts,
            sample.ghost_permille(CAPACITY),
            sample.slots,
            sample.orphan_slot_permille(),
            sample.registry_len,
            sample.orphan_subject_permille()
        );
    }
    let last = samples.last().expect("at least one session");
    let sessions = last.session + 1;
    let ghost_pct = f64::from(last.ghost_permille(CAPACITY)) / 10.0;
    let orphan_pct = f64::from(last.orphan_slot_permille()) / 10.0;
    println!(
        "\nafter {sessions} sessions: {ghosts} of {enrolled} rows are ghosts ({ghost_pct:.1}% of \
         capacity); {orphans} of {slots} slots ({orphan_pct:.1}%) point at retired subjects; \
         {live} of {registered} subjects are referenced",
        ghosts = last.ghosts,
        enrolled = last.enrolled,
        orphans = last.orphan_slots,
        slots = last.slots,
        live = last.live_subjects,
        registered = last.registry_len
    );
}

/// Draw the two graphs.
///
/// Hand-rolled SVG rather than a plotting crate: a diagnostic that pulls a dependency
/// tree into a library's dev-dependencies is a diagnostic nobody runs.
fn render(samples: &[Sample], per_subject: &[u32]) -> String {
    const WIDTH: u32 = 960;
    const HEIGHT: u32 = 580;
    const LEFT: f64 = 70.0;
    const PLOT_W: f64 = 380.0;
    const TOP_H: f64 = 250.0;
    const BOTTOM_Y: f64 = 400.0;
    const BOTTOM_H: f64 = 140.0;
    const INK: &str = "#1c1c1e";
    const MUTED: &str = "#6e6e73";
    const GRID: &str = "#dcdce1";
    const GHOST: &str = "#c0392b";
    const LIVE: &str = "#2d7d46";
    const RETIRED: &str = "#8e8e93";

    let mut svg = String::new();
    let _ = write!(
        svg,
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="{WIDTH}" height="{HEIGHT}" viewBox="0 0 {WIDTH} {HEIGHT}" font-family="ui-monospace, SFMono-Regular, Menlo, monospace">
<rect width="{WIDTH}" height="{HEIGHT}" fill="#fbfbfd"/>
<text x="{LEFT}" y="34" font-size="17" fill="{INK}" font-weight="600">Subject registry fragmentation and ghost accumulation</text>
<text x="{LEFT}" y="54" font-size="11" fill="{MUTED}">{SESSIONS} sessions · {SPAWN_PER_SESSION} spawned each · capacity {CAPACITY} · {SUBJECTS} subjects, {RETIRED_TOTAL} retired at sessions 4 and 8</text>
"##
    );

    let last = samples.last().expect("at least one session");
    let max_rows = samples
        .iter()
        .map(|sample| sample.enrolled)
        .max()
        .unwrap_or(1)
        .max(1);
    let scale_y = |rows: u32| TOP_H - (f64::from(rows) / f64::from(max_rows)) * (TOP_H - 110.0);
    let scale_x =
        |session: usize| LEFT + 24.0 + (session as f64 / (SESSIONS as f64 - 1.0)) * (PLOT_W - 60.0);

    let _ = write!(
        svg,
        r##"<text x="{LEFT}" y="84" font-size="12" fill="{INK}" font-weight="600">Rows per session · stacked: populated, then ghosts</text>
"##
    );
    for step in 0..=4 {
        let rows = max_rows * step / 4;
        let y = scale_y(rows);
        let _ = write!(
            svg,
            r##"<line x1="{LEFT}" y1="{y:.1}" x2="{:.1}" y2="{y:.1}" stroke="{GRID}"/>
<text x="{:.1}" y="{:.1}" font-size="10" fill="{MUTED}" text-anchor="end">{rows}</text>
"##,
            LEFT + PLOT_W,
            LEFT - 6.0,
            y + 3.0
        );
    }

    let mut populated_path = String::new();
    let mut ghost_path = String::new();
    for sample in samples {
        let x = scale_x(sample.session);
        let _ = write!(
            populated_path,
            "{}{:.1},{:.1} ",
            if populated_path.is_empty() { "M" } else { "L" },
            x,
            scale_y(sample.populated)
        );
        let _ = write!(
            ghost_path,
            "{}{:.1},{:.1} ",
            if ghost_path.is_empty() { "M" } else { "L" },
            x,
            scale_y(sample.enrolled)
        );
    }
    let _ = write!(
        svg,
        r##"<path d="{populated_path}L{:.1},{:.1} L{:.1},{:.1} Z" fill="{LIVE}" fill-opacity="0.18" stroke="{LIVE}" stroke-width="2"/>
<path d="{ghost_path}L{:.1},{:.1} L{:.1},{:.1} Z" fill="{GHOST}" fill-opacity="0.14" stroke="{GHOST}" stroke-width="2"/>
<text x="{:.1}" y="{:.1}" font-size="10" fill="{LIVE}">populated {}</text>
<text x="{:.1}" y="{:.1}" font-size="10" fill="{GHOST}">ghosts {}</text>
"##,
        scale_x(last.session),
        TOP_H,
        scale_x(0),
        TOP_H,
        scale_x(last.session),
        TOP_H,
        scale_x(0),
        TOP_H,
        scale_x(last.session) + 6.0,
        scale_y(last.populated) + 14.0,
        last.populated,
        scale_x(last.session) + 6.0,
        scale_y(last.enrolled) - 6.0,
        last.enrolled
    );
    for sample in samples {
        let _ = write!(
            svg,
            r##"<text x="{:.1}" y="{:.1}" font-size="9" fill="{MUTED}" text-anchor="middle">{}</text>
"##,
            scale_x(sample.session),
            TOP_H + 14.0,
            sample.session
        );
    }
    let _ = write!(
        svg,
        r##"<text x="{:.1}" y="{:.1}" font-size="10" fill="{MUTED}" text-anchor="middle">session</text>
"##,
        LEFT + PLOT_W / 2.0,
        TOP_H + 30.0
    );

    let columns = per_subject.len();
    let column_w = 24.0;
    let start_x = WIDTH as f64 - 40.0 - column_w * columns as f64;
    let peak = per_subject.iter().copied().max().unwrap_or(1).max(1);
    let bar_scale = |count: u32| (f64::from(count) / f64::from(peak)) * (BOTTOM_H - 12.0);

    let _ = write!(
        svg,
        r##"<text x="{start_x:.1}" y="84" font-size="12" fill="{INK}" font-weight="600">References per subject</text>
<text x="{start_x:.1}" y="102" font-size="10" fill="{MUTED}">peak {peak} · {:.1}% of slots orphaned</text>
<line x1="{start_x:.1}" y1="{:.1}" x2="{:.1}" y2="{:.1}" stroke="{GRID}"/>
"##,
        last.orphan_slot_permille() as f64 / 10.0,
        BOTTOM_Y + BOTTOM_H,
        start_x + column_w * columns as f64,
        BOTTOM_Y + BOTTOM_H
    );
    for (raw, count) in per_subject.iter().enumerate() {
        let x = start_x + column_w * raw as f64;
        let height = bar_scale(*count);
        let retired = u16::try_from(raw).unwrap_or(u16::MAX) >= last.registry_len;
        let fill = if retired {
            RETIRED
        } else if *count == 0 {
            GHOST
        } else {
            LIVE
        };
        let _ = write!(
            svg,
            r##"<rect x="{x:.1}" y="{:.1}" width="{:.1}" height="{height:.1}" fill="{fill}" fill-opacity="0.72"/>
"##,
            BOTTOM_Y + BOTTOM_H - height,
            column_w - 4.0
        );
        if raw % 4 == 0 {
            let _ = write!(
                svg,
                r##"<text x="{:.1}" y="{:.1}" font-size="9" fill="{MUTED}" text-anchor="middle">{raw}</text>
"##,
                x + (column_w - 4.0) / 2.0,
                BOTTOM_Y + BOTTOM_H + 14.0
            );
        }
    }
    let legend_y = BOTTOM_Y - 16.0;
    let live_x = start_x + 62.0;
    let unreferenced_x = start_x + 250.0;
    let axis_x = start_x + column_w * columns as f64 / 2.0;
    let axis_y = BOTTOM_Y + BOTTOM_H + 30.0;
    let _ = write!(
        svg,
        r##"<text x="{start_x:.1}" y="{legend_y:.1}" font-size="10" fill="{LIVE}">■ live</text>
<text x="{live_x:.1}" y="{legend_y:.1}" font-size="10" fill="{GHOST}">■ registered, never referenced</text>
<text x="{unreferenced_x:.1}" y="{legend_y:.1}" font-size="10" fill="{RETIRED}">■ retired, still referenced</text>
<text x="{axis_x:.1}" y="{axis_y:.1}" font-size="10" fill="{MUTED}" text-anchor="middle">subject id</text>
"##
    );

    let _ = write!(
        svg,
        r##"<text x="{LEFT}" y="556" font-size="10" fill="{MUTED}">Ghosts are rows holding no relationship: enrolled on spawn, never met anyone. They cost capacity, not time.</text>
<text x="{LEFT}" y="572" font-size="10" fill="{MUTED}">Orphan slots point at retired subjects. A version 2 save translates them; a running store cannot, so it pays until they decay.</text>
</svg>
"##
    );
    svg
}
