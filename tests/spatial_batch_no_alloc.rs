//! What a batch query costs the allocator, measured rather than asserted by hand.
//!
//! The batch API exists so a crowd can ask one question per agent per tick without a `Vec` per agent.
//! The serial path is the guarantee a frame can rely on: after warm-up, a batch touches the allocator
//! zero times. The parallel path cannot make that claim, and this test says so precisely instead of
//! implying otherwise — `std::thread::scope` boxes one work packet per worker, so a six-worker batch
//! costs a bounded handful of allocator calls. A frame that must not allocate runs the serial path, or
//! reuses a pool (the open task in plan.phase3.toml).
//!
//! The allocator counts per thread, because the harness runs each test on its own thread.
// A counting allocator is the only way to prove "allocates nothing" from outside the crate, and
// `GlobalAlloc` is unsafe by definition. The unsafe code forwards to the system allocator; the crate
// itself still denies unsafe.
#![expect(
    unsafe_code,
    reason = "a counting allocator is the only way to assert zero allocations per frame"
)]

use bevy_flock::domain::num;
use bevy_flock::domain::spatial::{QueryArena, SpatialGrid, SpatialGridConfig};
use bevy_flock::testkit::OrDie;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use bevy_math::Vec3;

thread_local! {
    /// Whether counting is on. `const` initialisers matter: a lazily initialised thread local would
    /// allocate on first use, from inside the allocator.
    static ENABLED: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

struct CountingAllocator;

// SAFETY: every method forwards to the system allocator with the same arguments and adds a
// non-allocating counter increment. The thread locals are `const` initialised.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        bump();
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        bump();
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        bump();
        unsafe { System.alloc_zeroed(layout) }
    }
}

fn bump() {
    let _ = ENABLED.try_with(|enabled| {
        if enabled.get() {
            let _ = ALLOCATIONS.try_with(|count| count.set(count.get() + 1));
        }
    });
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn counting<R>(body: impl FnOnce() -> R) -> (R, usize) {
    ENABLED.with(|enabled| enabled.set(true));
    ALLOCATIONS.with(|count| count.set(0));
    let result = body();
    ENABLED.with(|enabled| enabled.set(false));
    (result, ALLOCATIONS.with(Cell::get))
}

/// Ten thousand bodies and five hundred queries, the shape a crowd produces in one tick.
fn fixture() -> (SpatialGrid, Vec<Vec3>) {
    let config = SpatialGridConfig::try_new(Vec3::splat(-200.0), Vec3::splat(200.0), 5.0).or_die();
    let mut grid = SpatialGrid::new(config).or_die();
    grid.extend_from((0..10_000_u32).map(|index| {
        let step = num::u32_to_f32(index);
        let ring = (step * 0.7).sin() * 180.0;
        (
            bevy_flock::domain::ids::NpcId::unique(index),
            Vec3::new(
                ring,
                (step * 0.13).cos() * 20.0,
                (step * 0.31).sin() * 180.0,
            ),
            Vec3::new(step * 0.01, 0.0, -step * 0.01),
        )
    }));
    grid.build();
    let centres = (0..500_usize)
        .map(|index| {
            let step = num::u32_to_f32(num::usize_to_u32(index));
            Vec3::new(
                (step * 1.7).sin() * 150.0,
                (step * 0.9).cos() * 12.0,
                (step * 2.3).cos() * 150.0,
            )
        })
        .collect();
    (grid, centres)
}

#[test]
fn a_steady_serial_batch_does_not_touch_the_allocator() {
    let (grid, centres) = fixture();
    // One thread, or the "serial" test would quietly take the parallel path: the default thread count
    // is the machine's, which is the point of the previous test.
    let grid = grid.with_threads(1);
    let mut arena = QueryArena::new(32);
    // Warm-up: the arena grows to `queries * cap` here and never again.
    grid.radius_batch(&centres, 14.0, &mut arena);
    grid.radius_batch(&centres, 14.0, &mut arena);
    let ((), allocations) = counting(|| grid.radius_batch(&centres, 14.0, &mut arena));
    assert_eq!(
        allocations, 0,
        "a steady serial batch allocated {allocations} times"
    );
    assert!(
        arena.counts().iter().any(|count| *count > 0),
        "the batch must actually have found neighbours"
    );
}

#[test]
fn a_parallel_batch_pays_a_bounded_amount_for_its_workers() {
    // Not zero, and pretending otherwise would be worse than the cost: `std::thread::scope` boxes one
    // packet per worker. Six workers measured 22 allocator calls on this machine. The bound here is
    // deliberately loose enough to survive a stdlib change and tight enough to catch a per-query
    // allocation, which is the regression that would matter: 500 queries would blow past it at once.
    let (grid, centres) = fixture();
    let workers = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    let grid = grid.with_threads(workers);
    let mut arena = QueryArena::new(32);
    grid.radius_batch(&centres, 14.0, &mut arena);
    let ((), allocations) = counting(|| grid.radius_batch(&centres, 14.0, &mut arena));
    let budget = 8_usize.saturating_add(4 * workers);
    assert!(
        allocations <= budget,
        "a {workers}-worker batch allocated {allocations} times, over the {budget} the scope costs"
    );
}
