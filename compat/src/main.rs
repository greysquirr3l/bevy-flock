//! The forward direction of compatibility: a released build reading current files.
//!
//! Everything else in `tests/save_compat.rs` runs inside one build, which means the
//! reader and the writer are the same code and cannot fail in the direction that
//! breaks players — an *older* copy of the game opening a file written by a newer
//! one. That needs an older copy of the game, so this crate is not a member of the
//! main workspace and does not depend on the local path: it depends on a **git tag**.
//!
//! ```text
//! cargo run --manifest-path compat/Cargo.toml
//! ```
//!
//! The rule is weaker than for the current build, deliberately (ADR 0014): an old
//! build may not understand a newer file, so refusing is a pass. Panicking is not, and
//! neither is accepting a file it does not understand by guessing.
//!
//! When this fails, the answer is a decision, not a patch: either the format changed
//! in a way that needs a new version number, or the pin is obsolete and the tag below
//! should move — with a note in the ADR saying which.

use std::path::PathBuf;
use std::panic::catch_unwind;

use bevy_flock::domain::memory::{MemoryArchive, MemoryStore};
use bevy_flock::domain::saves::{
    LegacySubjectPolicy, MemorySnapshot, SaveError, SubjectEntry, SubjectKey, SubjectKind,
    SubjectRegistry,
};
use bevy_flock::domain::tick::Tick;

/// What a case is allowed to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    /// Must decode and restore.
    Loads,
    /// Must decode, but a restore without trust must be refused.
    LoadsUntrusted,
    /// Must be refused by version number, and must not panic.
    RefusesVersion,
    /// Must be decided either way, and must not panic.
    Decides,
}

struct Case {
    name: &'static str,
    bytes: Vec<u8>,
    expect: Expect,
}

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("compat sits inside the repository")
        .join("tests/fixtures")
}

/// CRC-32 (IEEE), for the one case that forges a header.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFF_u32;
    for byte in bytes {
        let index = ((crc ^ u32::from(*byte)) & 0xFF) as usize;
        crc = (crc >> 8) ^ POLY[index];
    }
    !crc
}

const POLY: [u32; 256] = {
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

fn main() {
    let root = fixtures();
    let mut cases = Vec::new();

    let v1 = std::fs::read(root.join("v1/basic.bflk")).expect("the committed v1 fixture");
    let v2 = std::fs::read(root.join("v2/canonical.bflk")).expect("the committed v2 fixture");

    cases.push(Case {
        name: "v1/basic.bflk",
        bytes: v1.clone(),
        expect: Expect::LoadsUntrusted,
    });
    cases.push(Case {
        name: "v2/canonical.bflk",
        bytes: v2.clone(),
        expect: Expect::Loads,
    });

    // A file from the future: same container, a version this build has never seen.
    let mut future = v2.clone();
    future[4..6].copy_from_slice(&99_u16.to_le_bytes());
    let header_crc = crc32(&future[..20]);
    future[20..24].copy_from_slice(&header_crc.to_le_bytes());
    cases.push(Case {
        name: "v2 with version 99",
        bytes: future,
        expect: Expect::RefusesVersion,
    });

    // A corruption in the payload: decided either way, but decided.
    let mut corrupted = v2.clone();
    let middle = corrupted.len() / 2;
    corrupted[middle] ^= 0x40;
    cases.push(Case {
        name: "v2 with one flipped bit",
        bytes: corrupted,
        expect: Expect::Decides,
    });

    let mut failures = 0;
    println!("pinned build: {}", describe_pin());
    println!(
        "{:<28} {:<18} {:<10} {}",
        "case", "expected", "outcome", "detail"
    );

    for case in &cases {
        let outcome = catch_unwind(|| decode(case));
        let (verdict, detail) = match outcome {
            Err(_) => ("PANIC", "the old build panicked on a file".to_owned()),
            Ok(report) => report,
        };
        let passed = verdict == "ok";
        if !passed {
            failures += 1;
        }
        println!(
            "{:<28} {:<18} {:<10} {}",
            case.name,
            format!("{:?}", case.expect),
            if passed { "pass" } else { "FAIL" },
            detail
        );
    }

    if failures > 0 {
        eprintln!(
            "\n{failures} case(s) failed. An old build must load what it understands and \
             refuse the rest by name; a panic or a silent misread is a format bug, not a pin bug."
        );
        std::process::exit(1);
    }
    println!("\nall cases pass: the released build neither panics nor guesses.");
}

/// The same two subjects the fixture was written with, keyed identically: a build
/// that matched by position instead of key would still "work" here, which is why the
/// reordering case lives in the current build's tests rather than this one.
fn registry() -> SubjectRegistry {
    [
        SubjectEntry::labelled(SubjectKey::new(0xA1), SubjectKind::Player, "the player"),
        SubjectEntry::labelled(SubjectKey::new(0xB2), SubjectKind::Npc, "the captain"),
    ]
    .into_iter()
    .collect()
}

fn describe_pin() -> &'static str {
    // Kept as a literal rather than read from Cargo.toml: this line is the thing a
    // reader checks against the tag in compat/Cargo.toml.
    "bevy-flock v0.1.0-rc.1 (git tag), reading fixtures written by the current tree"
}

fn decode(case: &Case) -> (&'static str, String) {
    match MemorySnapshot::from_bytes(&case.bytes) {
        Err(SaveError::UnsupportedVersion { found }) => {
            if found == 99 {
                ("ok", format!("refused by number: version {found}"))
            } else {
                ("FAIL", format!("refused as version {found}, expected 99"))
            }
        }
        Err(error) => match case.expect {
            Expect::RefusesVersion => ("FAIL", format!("refused, but for the wrong reason: {error:?}")),
            _ => ("ok", format!("refused with a typed error: {error:?}")),
        },
        Ok(snapshot) => {
            if case.expect == Expect::RefusesVersion {
                return ("FAIL", "accepted a file from the future".to_owned());
            }
            let mut store = MemoryStore::new(8, 2);
            let mut archive = MemoryArchive::new(2);
            let registry = registry();
            match snapshot.restore(
                &mut store,
                &mut archive,
                Tick::new(200),
                &registry,
                LegacySubjectPolicy::Refuse,
            ) {
                Ok(report) => (
                    "ok",
                    format!(
                        "decoded and restored: {} store records, {} archived",
                        report.store_records, report.archive_records
                    ),
                ),
                Err(error) if case.expect == Expect::LoadsUntrusted => (
                    "ok",
                    format!("decoded, and untrusted restore refused: {error:?}"),
                ),
                Err(error) => ("FAIL", format!("decoded but would not restore: {error:?}")),
            }
        }
    }
}
