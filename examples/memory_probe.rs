//! What the steering kernel costs the memory system, not the clock.
//!
//! Two questions the wall-clock benchmarks cannot answer, and the SIMD layout change makes
//! both of them fair to ask again:
//!
//! 1. **Does steering allocate?** A counting global allocator watches the steady-state loop,
//!    so "the kernel does not allocate after construction" is measured rather than asserted.
//! 2. **Is it cache-friendly?** The layout is structure-of-arrays with a 4-byte order vector;
//!    the counter is `valgrind --tool=cachegrind` (or `perf stat`), run against this binary.
//!
//! Usage: `cargo run --release --example memory_probe -- [agents] [frames] [--simd]`
//! The feature, not the flag, decides the kernel; the flag is accepted for the record and
//! printed so a captured run says which path it measured.
// A counting allocator is the only way to prove a steady-state loop allocates nothing, and
// `GlobalAlloc` is unsafe by definition. The unsafe code is four one-line forwards; the crate
// itself still denies unsafe.
#![allow(unsafe_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use bevy_flock::application::packed::{Pack, SteerParams};

/// Counts allocations and bytes, so a steady-state loop can be held to zero.
struct Counting;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static BYTES: AtomicUsize = AtomicUsize::new(0);

// SAFETY: every method forwards to `System`, with the same layout and the same result; the
// counters are the only addition and they do not touch the returned memory.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(new_size.saturating_sub(layout.size()), Ordering::Relaxed);
        unsafe { System.realloc(pointer, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

fn main() {
    let mut arguments = std::env::args()
        .skip(1)
        .filter(|argument| !argument.starts_with("--"));
    let agents: usize = arguments
        .next()
        .and_then(|value| value.parse().ok())
        .unwrap_or(50_000);
    let frames: usize = arguments
        .next()
        .and_then(|value| value.parse().ok())
        .unwrap_or(60);

    let params = SteerParams {
        radius: 8.0,
        separation: 2.0,
        extent: 512.0,
        ..SteerParams::default()
    };

    let mut pack = Pack::with_capacity(agents, params);
    let mut state = 0x5EED_u64;
    let mut next = move || {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        ((state >> 33) as f32 / (u32::MAX >> 1) as f32) * params.extent
    };
    for _ in 0..agents {
        let x = next();
        let y = next();
        pack.push([x, y], [0.5, 0.25]);
    }
    pack.steer(&params);
    pack.integrate(params.max_speed);
    pack.advance(1.0 / 60.0, params.extent);

    // Everything the pack needs is allocated by now; from here the loop is the claim.
    let allocations_before = ALLOCATIONS.load(Ordering::Relaxed);
    let bytes_before = BYTES.load(Ordering::Relaxed);

    let started = std::time::Instant::now();
    let mut pairs = 0_u64;
    for _ in 0..frames {
        pairs += pack.steer(&params).pairs;
        pack.integrate(params.max_speed);
        pack.advance(1.0 / 60.0, params.extent);
    }
    let elapsed = started.elapsed();

    let allocations = ALLOCATIONS.load(Ordering::Relaxed) - allocations_before;
    let bytes = BYTES.load(Ordering::Relaxed) - bytes_before;

    println!(
        "kernel={} agents={agents} frames={frames}",
        if cfg!(feature = "simd") {
            "simd"
        } else {
            "scalar"
        }
    );
    println!(
        "heap: {allocations} allocations, {bytes} bytes over {frames} frames ({:.2} ms/frame)",
        elapsed.as_secs_f64() * 1000.0 / frames as f64
    );
    println!("pairs: {pairs}");
    // positions + velocities + desired, 8 bytes each, plus the two index arrays at 4 bytes.
    let footprint = agents * (3 * 8 + 2 * 4);
    println!(
        "footprint: {} KiB of agent arrays ({} bytes/agent)",
        footprint / 1024,
        footprint / agents.max(1)
    );
}
