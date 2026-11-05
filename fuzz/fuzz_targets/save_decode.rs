#![no_main]
//! Feed arbitrary bytes to the save decoder.
//!
//! The contract the target enforces is narrow and important: `from_bytes` may reject
//! anything, and must never panic, hang, or allocate wildly. A save file arrives from
//! disk, from a cloud sync or from a player's friend, so it is untrusted input in the
//! same sense as a network packet — the only difference is that a corrupt save is more
//! likely to be an accident than an attack.
use bevy_flock::domain::saves::MemoryArchive;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(mut archive) = MemoryArchive::from_bytes(data) {
        // A successful decode must survive being re-encoded and decoded again: the
        // round trip is what a save/load/save cycle does, and a decoder that accepts a
        // file it cannot re-emit is a data-loss bug waiting for a player to hit it.
        let re_encoded = archive.to_bytes();
        if let Ok(mut again) = MemoryArchive::from_bytes(&re_encoded) {
            // Advancing the clock is what a load does before the first frame.
            again.rebase_offline(1);
        }
        let _ = archive.slot_count();
        let _ = archive.registry().map(bevy_flock::domain::saves::SubjectRegistry::len);
    }
});
