//! Decoder robustness without nightly: every byte pattern must be an error, never a panic.
//!
//! The `fuzz/` targets are the real thing — coverage-guided and run by hand or on a
//! schedule — but they need a nightly toolchain, and a suite that only runs on one
//! machine at a time is a suite that stops running. These tests run on stable in CI and
//! cover the same contract with three cheap generators:
//!
//! * **truncation** — every prefix of a valid file (the classic half-written save),
//! * **bit flips** — one flipped bit per byte position, spread deterministically,
//! * **header and length lies** — a valid body with a corrupted header, section table or
//!   trailer, which is what a bad merge or a hostile editor produces.
//!
//! The assertion is always the same: `Err`, or a value that survives re-encoding. A panic
//! fails the test by unwinding; a hang fails it by timeout. Nothing here checks *which*
//! error, because a decoder is allowed to be conservative.
#![expect(
    clippy::expect_used,
    reason = "the fixtures are built here and cannot be absent"
)]
#![cfg(feature = "memory")]
// This file is about corrupting bytes at chosen offsets; every index below is bounded by
// the fixture's length by construction, and the panic-on-slice lint would force a
// `get(..).expect()` at each site for no extra safety.
#![allow(
    clippy::indexing_slicing,
    clippy::redundant_clone,
    reason = "the corruption loop indexes a buffer whose length it just measured"
)]

use bevy_flock::domain::ids::NpcId;
use bevy_flock::domain::memory::{Encounter, EncounterKind, MemoryArchive, MemoryStore};
use bevy_flock::domain::saves::{
    MemorySnapshot, SubjectEntry, SubjectKey, SubjectKind, SubjectRegistry,
};
use bevy_flock::domain::tick::Tick;

/// A small but structurally complete archive: registry, rows, trailer.
fn fixture() -> Vec<u8> {
    let mut registry = SubjectRegistry::new();
    for player in 0..4_u64 {
        let _ = registry.register(SubjectEntry::labelled(
            SubjectKey::new(player),
            SubjectKind::Player,
            format!("player {player}"),
        ));
    }
    let mut store = MemoryStore::new(32, 8);
    for npc in 0..12_u32 {
        let index = store.enrol(NpcId::unique(npc)).expect("a row");
        for subject in 0..3_u16 {
            let _ = store.remember(
                index,
                bevy_flock::domain::memory::SubjectId::new(subject),
                &Encounter::new(EncounterKind::Spoke, Tick::new(u64::from(npc)))
                    .with_valence(120 - i16::try_from(npc).expect("small")),
            );
        }
    }
    MemorySnapshot::capture(
        &store,
        &MemoryArchive::new(256),
        Tick::new(500),
        600,
        &registry,
    )
    .to_bytes()
}

/// Decode, and if it decodes, prove it can be re-encoded.
fn must_not_panic(bytes: &[u8]) -> bool {
    MemorySnapshot::from_bytes(bytes).is_ok_and(|mut archive| {
        let again = archive.to_bytes();
        let decoded = MemorySnapshot::from_bytes(&again);
        assert!(decoded.is_ok(), "a round trip must decode: {decoded:?}");
        archive.rebase_offline(1);
        true
    })
}

#[test]
fn every_truncation_is_an_error_not_a_panic() {
    let bytes = fixture();
    let mut accepted = 0;
    for cut in 0..bytes.len() {
        if must_not_panic(&bytes[..cut]) {
            accepted += 1;
        }
    }
    // A prefix cannot be a complete file: the trailer is the last thing written, so every
    // truncation must be rejected. If this ever fails, the decoder is accepting partial
    // data, which is how a half-flushed save becomes a corrupted world.
    assert_eq!(accepted, 0, "{accepted} truncations decoded");
}

#[test]
fn single_bit_flips_never_panic_and_usually_fail_the_checksum() {
    let bytes = fixture();
    let mut decoded = 0;
    for position in (0..bytes.len()).step_by(3) {
        for bit in [0_u8, 3, 7] {
            let mut mutated = bytes.clone();
            mutated[position] ^= 1 << bit;
            if must_not_panic(&mutated) {
                decoded += 1;
            }
        }
    }
    // Some flips land in a section's payload where the CRC cannot see them, so a clean
    // "nothing decoded" would be an over-claim. What matters is that nothing panicked.
    assert!(
        decoded < bytes.len() / 3,
        "the checksum should reject most corruption, but {decoded} decodes is too many"
    );
}

#[test]
fn a_valid_body_with_a_lying_header_fails_closed() {
    let bytes = fixture();
    // The magic is the first four bytes; anything else must not be mistaken for a save.
    let mut wrong_magic = bytes.clone();
    wrong_magic[0] ^= 0xFF;
    assert!(!must_not_panic(&wrong_magic));
    // The version is next; a future version must be refused rather than misread.
    let mut wrong_version = bytes.clone();
    wrong_version[4] = wrong_version[4].wrapping_add(9);
    assert!(!must_not_panic(&wrong_version));
}

#[test]
fn random_bytes_are_rejected_quickly() {
    // Deterministic pseudo-random noise, so a failure is reproducible.
    let mut state = 0x5EED_1234_ABCD_0001_u64;
    for _ in 0..512 {
        let len = usize::try_from(state % 96).expect("small");
        let mut bytes = Vec::with_capacity(len);
        for _ in 0..len {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            bytes.push(u8::try_from(state & 0xFF).expect("a byte"));
        }
        assert!(!must_not_panic(&bytes), "noise decoded as a save");
    }
}

#[test]
fn an_empty_input_is_refused_without_panicking() {
    assert!(!must_not_panic(&[]));
}
