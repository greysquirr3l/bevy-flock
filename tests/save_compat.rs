//! Save compatibility, tested against committed bytes (ADR 0014).
//!
//! Round-trip tests prove that a reader agrees with the writer in the same commit,
//! which is the one pair that cannot disagree about a format that has shipped. These
//! tests load files that no code path in this run produced: a version 1 file and a
//! version 2 file, both blessed and committed, plus every mutation of the smaller
//! one.
//!
//! A fixture that stops loading is a P0 and the fix is the reader. Re-blessing is
//! deliberate:
//!
//! ```text
//! BLESS_SAVES=1 cargo test --test save_compat
//! ```
//!
//! after which the change appears as a binary diff next to the ADR that authorised
//! it.
#![expect(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::cast_possible_truncation,
    reason = "a compatibility test that cannot index a fixture or assert loudly is not one"
)]
use std::path::PathBuf;

use bevy_flock::domain::ids::NpcId;
use bevy_flock::domain::memory::{Encounter, EncounterKind, MemoryArchive, MemoryStore, SubjectId};
use bevy_flock::domain::saves::{
    LEGACY_VERSION, LegacySubjectPolicy, MAGIC, MemorySnapshot, Origin, SaveRecord, SavedSlot,
    SubjectEntry, SubjectKey, SubjectKind, SubjectRegistry,
};
use bevy_flock::domain::tick::Tick;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn blessing() -> bool {
    std::env::var("BLESS_SAVES").is_ok()
}

fn registry() -> SubjectRegistry {
    [
        SubjectEntry::labelled(SubjectKey::new(0xA1), SubjectKind::Player, "the player"),
        SubjectEntry::labelled(SubjectKey::new(0xB2), SubjectKind::Npc, "the captain"),
    ]
    .into_iter()
    .collect()
}

/// A world small enough to commit and varied enough to exercise the paths: two
/// subjects, a hot row with two slots, a hot row with one, and an archived row.
fn canonical_world() -> (MemoryStore, MemoryArchive) {
    let mut store = MemoryStore::new(8, 2);
    let mut archive = MemoryArchive::new(2);

    let first = store.enrol(NpcId::unique(7)).expect("room");
    store.remember(
        first,
        SubjectId::new(0),
        &Encounter::new(EncounterKind::Attacked, Tick::new(100)),
    );
    store.remember(
        first,
        SubjectId::new(1),
        &Encounter::new(EncounterKind::Helped, Tick::new(120)),
    );

    let second = store.enrol(NpcId::unique(9)).expect("room");
    store.remember(
        second,
        SubjectId::new(0),
        &Encounter::new(EncounterKind::Spoke, Tick::new(90)),
    );

    let parked = store.enrol(NpcId::unique(3)).expect("room");
    store.remember(
        parked,
        SubjectId::new(1),
        &Encounter::new(EncounterKind::Robbed, Tick::new(50)),
    );
    archive.insert(store.release(parked).expect("enrolled"));

    (store, archive)
}

fn canonical_snapshot() -> MemorySnapshot {
    let (store, archive) = canonical_world();
    MemorySnapshot::capture(&store, &archive, Tick::new(200), 3_600, &registry())
}

/// The version 1 layout, reconstructed from ADR 0012 for the legacy fixture.
fn v1_bytes(snapshot: &MemorySnapshot) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&LEGACY_VERSION.to_le_bytes());
    out.extend_from_slice(&1_u16.to_le_bytes());
    out.extend_from_slice(&snapshot.half_life_ticks.to_le_bytes());
    out.extend_from_slice(&2_u16.to_le_bytes());
    out.extend_from_slice(&(snapshot.records.len() as u32).to_le_bytes());
    out.extend_from_slice(&0_u16.to_le_bytes());
    let crc = crc32(&out);
    out.extend_from_slice(&crc.to_le_bytes());
    for record in &snapshot.records {
        out.extend_from_slice(&record.npc.to_bits().to_le_bytes());
        out.push(match record.origin {
            Origin::Store => 0,
            Origin::Archive => 1,
        });
        out.extend_from_slice(&(record.slots.len() as u16).to_le_bytes());
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
    let crc = crc32(&out);
    out.extend_from_slice(&crc.to_le_bytes());
    out.extend_from_slice(&0_u32.to_le_bytes());
    out
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFF_u32;
    for byte in bytes {
        let index = ((crc ^ u32::from(*byte)) & 0xFF) as usize;
        crc = (crc >> 8) ^ TABLE[index];
    }
    !crc
}

const TABLE: [u32; 256] = {
    let mut table = [0_u32; 256];
    let mut index = 0;
    while index < 256 {
        let mut value = index as u32;
        let mut bit = 0;
        while bit < 8 {
            value = if value & 1 == 1 {
                (value >> 1) ^ 0xEDB8_8320
            } else {
                value >> 1
            };
            bit += 1;
        }
        table[index] = value;
        index += 1;
    }
    table
};

fn write_fixture(name: &str, bytes: &[u8]) {
    let path = fixture(name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("fixture directory");
    }
    std::fs::write(&path, bytes).expect("write fixture");
}

fn read_fixture(name: &str) -> Vec<u8> {
    let path = fixture(name);
    std::fs::read(&path).unwrap_or_else(|error| {
        panic!(
            "missing fixture {}: {error}. Bless it with BLESS_SAVES=1 if this is a new case.",
            path.display()
        )
    })
}

#[test]
fn the_version_two_writer_is_frozen() {
    let bytes = canonical_snapshot().to_bytes();
    if blessing() {
        write_fixture("v2/canonical.bflk", &bytes);
    }
    assert_eq!(
        read_fixture("v2/canonical.bflk"),
        bytes,
        "the writer changed. If that was intentional, bless it and note it in an ADR"
    );
}

#[test]
fn a_committed_version_one_file_still_loads() {
    let expected = canonical_snapshot();
    let bytes = v1_bytes(&expected);
    if blessing() {
        write_fixture("v1/basic.bflk", &bytes);
    }

    let decoded = MemorySnapshot::from_bytes(&read_fixture("v1/basic.bflk")).expect("v1 must load");
    assert!(
        decoded.registry().is_none(),
        "version 1 carries no registry"
    );
    assert_eq!(
        decoded.records, expected.records,
        "the relationships survive"
    );

    // Refused by default, because a version 1 file cannot be verified…
    let (mut store, mut archive) = (MemoryStore::new(8, 2), MemoryArchive::new(2));
    let refused = decoded.clone().restore(
        &mut store,
        &mut archive,
        Tick::new(200),
        &registry(),
        LegacySubjectPolicy::Refuse,
    );
    assert!(
        refused.is_err(),
        "unverified subjects are not trusted silently"
    );
    assert_eq!(store.len(), 0);

    // …and loaded when the game says so, with the doubt recorded.
    let report = decoded
        .restore(
            &mut store,
            &mut archive,
            Tick::new(200),
            &registry(),
            LegacySubjectPolicy::AssumeDense,
        )
        .expect("trusted on request");
    assert!(report.unverified_mapping);
    assert_eq!(report.store_records, 2);
    assert_eq!(report.archive_records, 1);
}

#[test]
fn the_committed_version_two_file_round_trips_into_a_store() {
    let bytes = read_fixture("v2/canonical.bflk");
    let decoded = MemorySnapshot::from_bytes(&bytes).expect("v2 must load");
    let mut store = MemoryStore::new(8, 2);
    let mut archive = MemoryArchive::new(2);
    let report = decoded
        .restore(
            &mut store,
            &mut archive,
            Tick::new(200),
            &registry(),
            LegacySubjectPolicy::Refuse,
        )
        .expect("loads");
    assert_eq!(report.slots, 4);
    assert!(!report.translated);

    let (index, _) = store
        .enrolled()
        .find(|(_, npc)| *npc == NpcId::unique(7))
        .expect("the grudge holder");
    assert_eq!(
        store
            .recall(index, SubjectId::new(0))
            .expect("grudge")
            .valence(),
        -65
    );
}

#[test]
fn every_flipped_bit_is_a_typed_error_or_a_legitimate_load() {
    let bytes = read_fixture("v2/canonical.bflk");
    let mut loads = 0;
    let mut refusals = 0;
    for index in 0..bytes.len() {
        for bit in 0..8 {
            let mut mutated = bytes.clone();
            mutated[index] ^= 1 << bit;
            match MemorySnapshot::from_bytes(&mutated) {
                Ok(_) => loads += 1,
                Err(_) => refusals += 1,
            }
        }
    }
    assert_eq!(
        loads + refusals,
        bytes.len() * 8,
        "every mutation was decided"
    );
    assert!(
        refusals > loads,
        "most mutations must be refused, not trusted"
    );
}

#[test]
fn every_prefix_is_refused_without_panicking() {
    let bytes = read_fixture("v2/canonical.bflk");
    for cut in 0..bytes.len() {
        let prefix = &bytes[..cut];
        assert!(
            MemorySnapshot::from_bytes(prefix).is_err(),
            "a {cut}-byte prefix is not a save"
        );
    }
}

#[test]
fn a_future_version_is_refused_by_number() {
    let mut bytes = read_fixture("v2/canonical.bflk");
    bytes[4..6].copy_from_slice(&3_u16.to_le_bytes());
    let crc = crc32(&bytes[..20]);
    bytes[20..24].copy_from_slice(&crc.to_le_bytes());
    match MemorySnapshot::from_bytes(&bytes) {
        Err(bevy_flock::domain::saves::SaveError::UnsupportedVersion { found }) => {
            assert_eq!(found, 3);
        }
        other => panic!("expected a version refusal, got {other:?}"),
    }
}

#[test]
fn an_unknown_section_is_skipped() {
    // The committed file is version 2 with two sections; a third writer may add one,
    // and this build must load the file rather than refuse it.
    let snapshot = MemorySnapshot::from_bytes(&read_fixture("v2/canonical.bflk")).expect("loads");
    let base = read_fixture("v2/canonical.bflk");
    let extended = append_section(&base, 200, b"future payload");
    {
        let count = u16::from_le_bytes([extended[18], extended[19]]) as usize;
        let body_start = 24 + count * 16;
        let stored = u32::from_le_bytes([
            extended[extended.len() - 8],
            extended[extended.len() - 7],
            extended[extended.len() - 6],
            extended[extended.len() - 5],
        ]);
        eprintln!(
            "sections {count} body_start {body_start} len {} stored {stored:08x} computed {:08x} base_count {}",
            extended.len(),
            crc32(&extended[..body_start]),
            u16::from_le_bytes([base[18], base[19]])
        );
    }
    let decoded = MemorySnapshot::from_bytes(&extended).expect("forward compatible");
    assert_eq!(decoded.records, snapshot.records);
}

/// Append a section to a version 2 file, fixing the header, table and trailer.
fn append_section(bytes: &[u8], id: u16, payload: &[u8]) -> Vec<u8> {
    let section_count = u16::from_le_bytes([bytes[18], bytes[19]]) as usize;
    let table_bytes = 16;
    let old_table_end = 24 + section_count * table_bytes;
    let body_end = bytes.len() - 8;
    let body = &bytes[old_table_end..body_end];

    let mut out = Vec::with_capacity(bytes.len() + table_bytes + payload.len());
    out.extend_from_slice(&bytes[..18]);
    out.extend_from_slice(&((section_count + 1) as u16).to_le_bytes());
    // The section count lives inside the header's checksum, so it must be recomputed
    // rather than copied: the first version of this helper did copy it, and the file
    // it produced was rejected — which is the check working.
    let header_crc = crc32(&out);
    out.extend_from_slice(&header_crc.to_le_bytes());

    // Existing entries keep their order; each offset moves by one table entry.
    for entry in 0..section_count {
        let base = 24 + entry * table_bytes;
        let mut copy = bytes[base..base + table_bytes].to_vec();
        let offset = u32::from_le_bytes([copy[4], copy[5], copy[6], copy[7]]) + table_bytes as u32;
        copy[4..8].copy_from_slice(&offset.to_le_bytes());
        out.extend_from_slice(&copy);
    }
    // After the existing payloads, not at the start of the body.
    let new_offset = (old_table_end + table_bytes + body.len()) as u32;
    out.extend_from_slice(&id.to_le_bytes());
    out.extend_from_slice(&0_u16.to_le_bytes());
    out.extend_from_slice(&new_offset.to_le_bytes());
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(&crc32(payload).to_le_bytes());

    out.extend_from_slice(body);
    out.extend_from_slice(payload);
    // The trailer checksums the header and the table only: payloads carry their own
    // section checksums, which is the amendment the benchmark forced.
    let table_end = 24 + (section_count + 1) * table_bytes;
    let table_crc = crc32(&out[..table_end]);
    out.extend_from_slice(&table_crc.to_le_bytes());
    out.extend_from_slice(&0_u32.to_le_bytes());
    out
}

#[test]
fn the_manifest_names_what_the_corpus_covers() {
    // ADR 0014 promises a manifest; this asserts the promise is visible rather than
    // assumed, and fails if a fixture is added without being described.
    let manifest = fixture("manifest.toml");
    let text = std::fs::read_to_string(&manifest).unwrap_or_else(|error| {
        panic!("missing manifest {}: {error}", manifest.display());
    });
    for name in ["v1/basic.bflk", "v2/canonical.bflk"] {
        assert!(
            text.contains(name),
            "{name} is not described in the manifest"
        );
        assert!(fixture(name).exists(), "{name} is described but absent");
    }
}

#[allow(dead_code)]
fn unused_slot_fixture() -> SavedSlot {
    // Keeps the type in scope for future fixtures that need a hand-built slot.
    SavedSlot {
        familiarity: 1,
        valence: -1,
        encounters: 1,
        flags: 0,
        first_age: 0,
        last_age: 0,
    }
}

#[allow(dead_code)]
fn unused_record_fixture() -> SaveRecord {
    SaveRecord {
        npc: NpcId::unique(1),
        origin: Origin::Store,
        slots: smallvec::SmallVec::new(),
    }
}
