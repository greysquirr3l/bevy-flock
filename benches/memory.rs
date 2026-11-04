//! What remembering costs.
//!
//! Four operations, because they have four different costs and only one of them
//! belongs in a frame:
//!
//! - `remember` — written by a perception system, once per encounter, which is
//!   rare.
//! - `recall` — read by a behaviour tree, once per think, which is common.
//! - `recall_decayed` — the same read with the decay applied, which is the one a
//!   game actually calls.
//! - `sweep` — the maintenance pass that frees rows nobody needs, which belongs on
//!   a slow cadence and never in the frame budget.
//!
//! Measured over 16,384 enrolled NPCs with four subjects: the shape of a busy
//! tavern district rather than a whole city.
#![expect(
    clippy::expect_used,
    reason = "benchmark setup asserts its own fixtures"
)]
use std::hint::black_box;

use bevy_flock::domain::memory::{
    DecayPolicy, Encounter, EncounterKind, MemoryIndex, MemoryStore, SubjectId,
};
use bevy_flock::prelude::*;
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};

const ROWS: u32 = 16_384;
const SUBJECTS: u16 = 4;

struct Fixture {
    store: MemoryStore,
    rows: Vec<MemoryIndex>,
}

fn fixture() -> Fixture {
    let mut store = MemoryStore::new(ROWS, SUBJECTS);
    let rows = (0..ROWS)
        .map(|npc| {
            store
                .enrol(NpcId::unique(npc))
                .expect("capacity matches the fixture")
        })
        .collect();
    Fixture { store, rows }
}

fn bench_remember(c: &mut Criterion) {
    let mut group = c.benchmark_group("memory_remember");
    group.throughput(Throughput::Elements(u64::from(ROWS)));
    let mut fixture = fixture();
    let subject = SubjectId::new(0);

    group.bench_function("16k_npcs_one_encounter_each", |b| {
        let mut tick = Tick::ZERO;
        b.iter(|| {
            tick = tick.next();
            let encounter = Encounter::new(EncounterKind::Spoke, tick);
            for row in &fixture.rows {
                black_box(fixture.store.remember(black_box(*row), subject, &encounter));
            }
        });
    });
    group.finish();
}

fn bench_recall(c: &mut Criterion) {
    let mut fixture = fixture();
    let subject = SubjectId::new(0);
    let encounter = Encounter::new(EncounterKind::Attacked, Tick::ZERO);
    for row in &fixture.rows {
        fixture.store.remember(*row, subject, &encounter);
    }

    let mut group = c.benchmark_group("memory_recall");
    group.throughput(Throughput::Elements(u64::from(ROWS)));

    group.bench_function("16k_npcs_raw", |b| {
        b.iter(|| {
            let mut found = 0_u32;
            for row in &fixture.rows {
                if fixture.store.recall(black_box(*row), subject).is_some() {
                    found += 1;
                }
            }
            black_box(found)
        });
    });

    let decay = DecayPolicy::MINUTE;
    group.bench_function("16k_npcs_decayed", |b| {
        let now = Tick::new(10_000);
        b.iter(|| {
            let mut hostile = 0_u32;
            for row in &fixture.rows {
                if let Some((valence, _)) = fixture.store.recall_decayed(*row, subject, now, decay)
                    && valence < 0
                {
                    hostile += 1;
                }
            }
            black_box(hostile)
        });
    });
    group.finish();
}

fn bench_sweep(c: &mut Criterion) {
    let mut group = c.benchmark_group("memory_sweep");
    group.throughput(Throughput::Elements(u64::from(ROWS)));
    let decay = DecayPolicy::MINUTE;
    let subject = SubjectId::new(0);

    // Nothing has faded: this is the scan cost, paid by every sweep.
    let mut kept = fixture();
    for row in &kept.rows {
        kept.store.remember(
            *row,
            subject,
            &Encounter::new(EncounterKind::Attacked, Tick::ZERO),
        );
    }
    group.bench_function("16k_none_freed", |b| {
        b.iter(|| {
            black_box(kept.store.sweep(black_box(Tick::new(10)), decay, 1));
        });
    });

    // Everything has faded: the scan plus the reclaim. Setup is excluded so the
    // number is the sweep and not the re-enrolment that feeds it.
    group.bench_function("16k_all_reclaimed", |b| {
        b.iter_batched(
            || {
                let mut fixture = fixture();
                for row in &fixture.rows {
                    fixture.store.remember(
                        *row,
                        subject,
                        &Encounter::new(EncounterKind::Spoke, Tick::ZERO),
                    );
                }
                fixture.store
            },
            |mut store| black_box(store.sweep(Tick::new(100_000), decay, 1)),
            criterion::BatchSize::SmallInput,
        );
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_remember,
    bench_recall,
    bench_sweep,
    bench_stride
);
criterion_main!(benches);

/// What ADR 0016's ceiling costs when a game actually uses it.
///
/// The stride is the sweep's outer loop: a store with sixty-four subjects scans four
/// times as many cells as one with sixteen, and touches four times as many cache
/// lines per row. This measures that, in the case where it is entirely paid for — a
/// store holding no relationships at all, which is the worst case for the scan.
fn bench_stride(c: &mut Criterion) {
    let mut group = c.benchmark_group("memory_stride");
    group.throughput(Throughput::Elements(4_096));
    let decay = DecayPolicy::MINUTE;
    for subjects in [16_u16, 64] {
        let mut store = MemoryStore::new(4_096, subjects);
        for npc in 0..4_096_u32 {
            store.enrol(NpcId::unique(npc)).expect("sized");
        }
        group.bench_function(BenchmarkId::new("sweep_empty", subjects), |b| {
            b.iter(|| {
                black_box(store.sweep(black_box(Tick::new(10)), decay, 1));
            });
        });

        // One relationship per row, held at the first subject: the shape a crowded
        // street actually has, and the one where the stride is really walked.
        let mut populated = MemoryStore::new(4_096, subjects);
        for npc in 0..4_096_u32 {
            let index = populated.enrol(NpcId::unique(npc)).expect("sized");
            populated.remember(
                index,
                SubjectId::new(0),
                &Encounter::new(EncounterKind::Spoke, Tick::ZERO),
            );
        }
        group.bench_function(BenchmarkId::new("sweep_one_slot_each", subjects), |b| {
            b.iter(|| {
                black_box(populated.sweep(black_box(Tick::new(10)), decay, 1));
            });
        });
    }
    group.finish();
}
