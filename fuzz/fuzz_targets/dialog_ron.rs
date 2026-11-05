#![no_main]
//! Feed arbitrary bytes to the dialogue graph loader.
//!
//! Dialogue files are the most likely thing in a game to be written by a writer rather
//! than a programmer, and the most likely to be edited by hand at the last minute. A
//! panic here is a crash on load; an error with a position is a bug report a writer can
//! act on.
use bevy_flock::behavior::dialog::DialogGraph;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(text) = std::str::from_utf8(data) {
        if let Ok(graph) = DialogGraph::from_ron(text) {
            // Walking every node from the entry point is what a conversation does.
            let _ = graph.validate();
            let _ = graph.open();
        }
    }
});
