//! The crowd kernel steers a steady population without allocating.
//!
//! The cell-major kernel exists to make a 10,000-agent frame cheap, and the claim that matters
//! most for a game loop is that the cost does not include the allocator: every buffer is grown
//! once and reused. This measures it from outside the crate with a counting global allocator and
//! asserts exactly zero allocations after warm-up.
//!
//! The allocator counts per thread, because the test harness runs tests on its own threads.
// A counting allocator is the only way to prove "allocates nothing" from outside the crate, and
// `GlobalAlloc` is unsafe by definition. The unsafe code forwards to the system allocator; the
// crate itself still denies unsafe.
#![allow(unsafe_code)]
#![expect(clippy::expect_used, reason = "test setup asserts its own fixtures")]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use bevy_flock::application::{AvoidanceParams, Crowd, CrowdParams};
use bevy_flock::domain::spatial::{SpatialGrid, SpatialGridConfig};
use bevy_flock::domain::steering::SteeringParams;

thread_local! {
    /// Whether counting is on. `const` initialisers matter: a lazily initialised thread local
    /// would allocate on first use, from inside the allocator.
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

#[test]
fn a_steady_frame_does_not_touch_the_allocator() {
    let grid = SpatialGrid::new(SpatialGridConfig::default()).expect("grid");
    let params = CrowdParams::from_steering(&SteeringParams::human_scale());
    let mut crowd = Crowd::default();
    crowd.configure_from_grid(&grid);

    // 1,000 agents on the spawn spiral, the shape the crowd plugin produces.
    let frame = |crowd: &mut Crowd| {
        crowd.begin();
        for index in 0..1_000_u32 {
            let ring = 1.6 * libm::sqrtf(index as f32);
            let angle = index as f32 * 2.399_963_2;
            let step_angle = index as f32 * 0.7;
            // A crowd that spans three dimensions, so the buffers are sized for the real case.
            crowd.push(
                [
                    libm::cosf(angle) * ring,
                    libm::sinf(step_angle) * 3.0,
                    libm::sinf(angle) * ring,
                ],
                [0.1, 0.0, -0.1],
                if index % 4 == 0 {
                    Some([20.0, 2.0, 20.0])
                } else {
                    None
                },
                true,
            );
        }
        crowd.steer(&params)
    };

    // Warm-up: the buffers, the occupancy table and the order vector are grown here.
    frame(&mut crowd);
    frame(&mut crowd);

    let (report, allocations) = counting(|| frame(&mut crowd));
    assert_eq!(report.steered, 1_000);
    assert_eq!(
        allocations, 0,
        "a steady crowd frame allocated {allocations} times"
    );
}
#[test]
fn a_steady_avoidance_pass_does_not_touch_the_allocator() {
    let grid = SpatialGrid::new(SpatialGridConfig::default()).expect("grid");
    let params = CrowdParams::from_steering(&SteeringParams::human_scale());
    let avoidance = AvoidanceParams::new();
    let mut crowd = Crowd::default();
    crowd.configure_from_grid(&grid);
    let frame = |crowd: &mut Crowd| {
        crowd.begin();
        for index in 0..1_000_u32 {
            let ring = 1.6 * libm::sqrtf(index as f32);
            let angle = index as f32 * 2.399_963_2;
            crowd.push(
                [
                    libm::cosf(angle) * ring,
                    libm::sinf(index as f32 * 0.7) * 3.0,
                    libm::sinf(angle) * ring,
                ],
                [0.1, 0.0, -0.1],
                Some([20.0, 2.0, 20.0]),
                true,
            );
        }
        let report = crowd.steer(&params);
        let avoided = crowd.avoid(&avoidance);
        (report, avoided)
    };
    frame(&mut crowd);
    frame(&mut crowd);
    let ((steered, avoided), allocations) = counting(|| frame(&mut crowd));
    assert_eq!(steered.steered, 1_000);
    assert!(
        avoided.pairs > 0,
        "the pass must actually have examined pairs"
    );
    assert_eq!(
        allocations, 0,
        "a steady avoidance pass allocated {allocations} times"
    );
}
