//! What the section container costs (ADR 0013, ADR 0014).
//!
//! Version 2 does strictly more work than version 1 on the same world: it writes a
//! table, hashes the registry, checksums each section *and* the file, and carries a
//! registry. This benchmark asks whether any of that matters at a population where
//! a save is actually large — 50,000 active NPCs, two relationships each, on the
//! order of two megabytes.
//!
//! Version 1 has no writer in the library any more (ADR 0013: the new build never
//! emits it), so the baseline here is a reconstruction of the documented version 1
//! layout — header, records, trailer — written straight from ADR 0012. It is
//! verified by feeding its own output back through `MemorySnapshot::from_bytes`,
//! which only accepts it if it really is a readable version 1 file.
//!
//! Three measurements per version, because they fail in different places:
//!
//! - `encode` — CPU plus allocation on the way out, which happens on a save.
//! - `decode` — parsing plus every checksum, which happens on a load.
//! - `round_trip` — both, with a real snapshot in between, which is what a game
//!   actually experiences when it autosaves and the player reloads.
use bevy_flock::testkit::OrDie;
use std::hint::black_box;
use std::time::Duration;

use bevy_flock::domain::ids::NpcId;
use bevy_flock::domain::memory::{Encounter, EncounterKind, MemoryArchive, MemoryStore, SubjectId};
use bevy_flock::domain::num;
use bevy_flock::domain::saves::{
    LEGACY_VERSION, MAGIC, MemorySnapshot, Origin, SaveRecord, SavedSlot, SubjectEntry, SubjectKey,
    SubjectKind, SubjectRegistry,
};
use bevy_flock::domain::tick::Tick;
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};

/// Active NPCs in the measured world.
const NPCS: u32 = 50_000;

/// Subjects in the registry: the player and four recurring characters.
const SUBJECTS: u16 = 5;

fn registry() -> SubjectRegistry {
    (0..SUBJECTS)
        .map(|index| {
            SubjectEntry::labelled(
                SubjectKey::new(0x1000 + u64::from(index)),
                if index == 0 {
                    SubjectKind::Player
                } else {
                    SubjectKind::Npc
                },
                format!("subject-{index}"),
            )
        })
        .collect()
}

/// A world of 50,000 NPCs, each remembering two subjects.
fn world() -> MemoryStore {
    let mut store = MemoryStore::new(NPCS, SUBJECTS);
    for npc in 0..NPCS {
        let index = store.enrol(NpcId::unique(npc)).or_die();
        store.remember(
            index,
            SubjectId::new(0),
            &Encounter::new(EncounterKind::Attacked, Tick::new(100)),
        );
        store.remember(
            index,
            SubjectId::new(1 + (npc % 3) as u16),
            &Encounter::new(EncounterKind::Spoke, Tick::new(120)),
        );
    }
    store
}

fn snapshot(store: &MemoryStore) -> MemorySnapshot {
    MemorySnapshot::capture(
        store,
        &MemoryArchive::new(8),
        Tick::new(200),
        3_600,
        &registry(),
    )
}

/// The version 1 layout, reconstructed from ADR 0012.
///
/// Kept here rather than in the library because version 1 is a format this build
/// reads and never writes; a benchmark is allowed to ask what it would have cost.
fn v1_bytes(snapshot: &MemorySnapshot) -> Vec<u8> {
    // Sized from the v1 layout directly: `encoded_len` describes the v2 container, and
    // using it here would make the baseline pay for an encode it never writes.
    let mut out = Vec::with_capacity(32 + snapshot.records.len() * 43);
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&LEGACY_VERSION.to_le_bytes());
    out.extend_from_slice(&1_u16.to_le_bytes());
    out.extend_from_slice(&snapshot.half_life_ticks.to_le_bytes());
    out.extend_from_slice(&SUBJECTS.to_le_bytes());
    out.extend_from_slice(&(num::usize_to_u32(snapshot.records.len())).to_le_bytes());
    out.extend_from_slice(&0_u16.to_le_bytes());
    let header_crc = crc32(&out);
    out.extend_from_slice(&header_crc.to_le_bytes());

    for record in &snapshot.records {
        out.extend_from_slice(&record.npc.to_bits().to_le_bytes());
        out.push(0);
        out.extend_from_slice(&(num::usize_to_u16(record.slots.len())).to_le_bytes());
        for (subject, slot) in &record.slots {
            out.extend_from_slice(&subject.get().to_le_bytes());
            out.extend_from_slice(&slot.familiarity.to_le_bytes());
            out.extend_from_slice(&slot.valence.to_le_bytes());
            out.extend_from_slice(&slot.encounters.to_le_bytes());
            out.extend_from_slice(&slot.flags.to_le_bytes());
            out.extend_from_slice(&slot.first_age.to_le_bytes());
            out.extend_from_slice(&slot.last_age.to_le_bytes());
        }
    }
    let file_crc = crc32(&out);
    out.extend_from_slice(&file_crc.to_le_bytes());
    out.extend_from_slice(&0_u32.to_le_bytes());
    out
}

/// CRC-32 (IEEE), the same table the format uses.
/// The same CRC-32 the writer uses, so the benchmark measures the format rather than a stub.
fn crc32(bytes: &[u8]) -> u32 {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    let table =
        TABLE.get_or_init(|| core::array::from_fn(|index| crc_entry(num::usize_to_u32(index))));
    let mut crc = 0xFFFF_FFFF_u32;
    for byte in bytes {
        let index = usize::from(u8::try_from((crc ^ u32::from(*byte)) & 0xFF).unwrap_or(0));
        crc = (crc >> 8) ^ table.get(index).copied().unwrap_or_default();
    }
    !crc
}
const fn crc_entry(index: u32) -> u32 {
    let mut value = index;
    let mut bit = 0;
    while bit < 8 {
        value = if value & 1 == 1 {
            (value >> 1) ^ 0xEDB8_8320
        } else {
            value >> 1
        };
        bit += 1;
    }
    value
}

#[expect(
    clippy::significant_drop_tightening,
    reason = "Criterion writes the report when the group drops; the group is the measurement"
)]
fn save_format(c: &mut Criterion) {
    let store = world();
    let snapshot = snapshot(&store);

    let v1 = v1_bytes(&snapshot);
    let v2 = snapshot.to_bytes();
    assert!(
        v1.len() > 2_000_000,
        "the fixture is save-sized: {}",
        v1.len()
    );
    assert!(
        MemorySnapshot::from_bytes(&v1).is_ok(),
        "the reconstructed v1 file must be one the library can read"
    );
    println!(
        "50,000 NPCs x 2 relationships: v1 {} bytes, v2 {} bytes ({} more, {:+.2}%)",
        v1.len(),
        v2.len(),
        num::usize_to_i64(v2.len()) - num::usize_to_i64(v1.len()),
        (num::usize_to_f64(v2.len()) / num::usize_to_f64(v1.len()) - 1.0) * 100.0
    );
    let mut group = c.benchmark_group("save_encode");
    group.sample_size(20);
    group.bench_with_input(
        BenchmarkId::new("v1", NPCS),
        &snapshot,
        |bencher, snapshot| {
            bencher.iter(|| black_box(v1_bytes(black_box(snapshot))));
        },
    );
    group.bench_with_input(
        BenchmarkId::new("v2", NPCS),
        &snapshot,
        |bencher, snapshot| {
            bencher.iter(|| black_box(snapshot.to_bytes()));
        },
    );
    group.finish();
    let mut group = c.benchmark_group("save_decode");
    group.sample_size(20);
    group.bench_with_input(BenchmarkId::new("v1", NPCS), &v1, |bencher, bytes| {
        bencher.iter(|| black_box(MemorySnapshot::from_bytes(black_box(bytes)).or_die()));
    });
    group.bench_with_input(BenchmarkId::new("v2", NPCS), &v2, |bencher, bytes| {
        bencher.iter(|| black_box(MemorySnapshot::from_bytes(black_box(bytes)).or_die()));
    });
    group.finish();
    let mut group = c.benchmark_group("save_round_trip");
    group.sample_size(20);
    group.bench_function(BenchmarkId::new("v2", NPCS), |bencher| {
        bencher.iter(|| {
            let bytes = snapshot.to_bytes();
            black_box(MemorySnapshot::from_bytes(&bytes).or_die())
        });
    });
    group.finish();

    // A single record, to show what the container costs when the file is small: the
    // fixed overhead is the section table and two extra checksums.
    let small = MemorySnapshot {
        half_life_ticks: 3_600,
        registry: Some(registry()),
        records: vec![SaveRecord {
            npc: NpcId::unique(1),
            origin: Origin::Store,
            slots: smallvec::SmallVec::from_slice(&[(
                SubjectId::new(0),
                SavedSlot {
                    familiarity: 1,
                    valence: -65,
                    encounters: 1,
                    flags: 1,
                    first_age: 10,
                    last_age: 10,
                },
            )]),
        }],
    };
    println!(
        "one NPC: v1 {} bytes, v2 {} bytes (the container's fixed cost)",
        v1_bytes(&small).len(),
        small.to_bytes().len()
    );

    // The v1 size is a pure function of the layout, so pin it; the v2 size depends on
    // the registry's labels and is pinned by the unit test in `saves.rs` instead.
    assert_eq!(v1.len(), 32 + NPCS as usize * 43);
    assert!(v2.len() > v1.len(), "the container is never free");
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .warm_up_time(Duration::from_millis(500))
        .measurement_time(Duration::from_secs(4));
    targets = save_format
}
criterion_main!(benches);
