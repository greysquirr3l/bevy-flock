//! A behaviour tree evaluates without allocating.
//!
//! This is the claim the whole 100,000-NPC story rests on, so it is measured
//! rather than assumed: a counting global allocator watches the loop, and the
//! assertion is exactly zero allocations after warm-up.
//!
//! The allocator counts per thread, because the Rust test harness runs tests on
//! its own threads and would otherwise contribute noise that has nothing to do
//! with the tree.
// A counting allocator is the only way to prove "allocates nothing" from outside
// the crate, and `GlobalAlloc` is unsafe by definition. The unsafe code is four
// one-line forwards to the system allocator; the crate itself still denies unsafe.
#![allow(unsafe_code)]
use bevy_flock::testkit::OrDie;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Arc;

use bevy_flock::behavior::tree::{BehaviorTree, action, selector, sequence};
use bevy_flock::prelude::*;

thread_local! {
    /// Whether counting is on. `const` initialisers matter here: a lazily
    /// initialised thread local would allocate on first use, from inside the
    /// allocator.
    static ENABLED: Cell<bool> = const { Cell::new(false) };
    /// Allocations seen by this thread while counting.
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

struct CountingAllocator;

// SAFETY: every method forwards to the system allocator with the same arguments
// and adds a non-allocating counter increment. The thread locals are `const`
// initialised and never allocate.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count();
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        count();
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        count();
        unsafe { System.alloc_zeroed(layout) }
    }
}

fn count() {
    if ENABLED.with(Cell::get) {
        ALLOCATIONS.with(|count| count.set(count.get() + 1));
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn counted<T>(f: impl FnOnce() -> T) -> (T, usize) {
    ALLOCATIONS.with(|count| count.set(0));
    ENABLED.with(|flag| flag.set(true));
    let value = f();
    ENABLED.with(|flag| flag.set(false));
    (value, ALLOCATIONS.with(Cell::get))
}

/// A wide tree of roughly a thousand nodes: 32 sequences of 31 actions.
///
/// Leaf ids stay under 12 so every branch is made of actions; a branch containing
/// a predicate would fail instead of finishing, and the fixture would measure a
/// selector that never succeeds.
fn thousand_node_tree() -> BehaviorTree {
    let branches = (0..32_u32)
        .map(|branch| {
            let steps = (0..31_u32)
                .map(|step| action((branch + step) % 12))
                .collect();
            sequence(steps)
        })
        .collect();
    BehaviorTree::try_from_spec(selector(branches)).or_die()
}

#[test]
fn ticking_a_thousand_node_tree_ten_thousand_times_allocates_nothing() {
    let mut registry = FlockRegistry::new();
    let _leaves = registry.register_standard_leaves().or_die();
    let tree = Arc::new(thousand_node_tree());
    let mut brain = NpcBrain::new(Arc::clone(&tree), VirtualCost::new(500));
    let mut board = Blackboard::new();
    let mut dispatch = registry.dispatcher();
    let mut tick = Tick::ZERO;

    // Warm up: the cursor grows to the tree's depth here, not in the measured
    // loop, and the blackboard fills its slots.
    for _ in 0..64 {
        let _ = brain.tick(&mut dispatch, &mut board, tick, NpcId::unique(0));
        tick = tick.next();
    }

    let (statuses, allocations) = counted(|| {
        let mut seen_success = 0_usize;
        for _ in 0..10_000 {
            if brain
                .tick(&mut dispatch, &mut board, tick, NpcId::unique(0))
                .is_success()
            {
                seen_success += 1;
            }
            tick = tick.next();
        }
        seen_success
    });

    assert_eq!(
        allocations, 0,
        "evaluation allocated {allocations} times over 10,000 ticks"
    );
    assert!(
        statuses > 0,
        "the tree never succeeded, so nothing was measured"
    );
}

#[test]
fn building_a_tree_allocates_and_that_is_fine() {
    // The control: if the counter were broken, this test would fail rather than
    // silently reporting zero for everything.
    let (tree, allocations) = counted(thousand_node_tree);
    assert!(
        allocations > 0,
        "the counting allocator saw no allocation while compiling"
    );
    assert!(tree.len() > 900);
}

#[test]
fn a_fresh_cursor_does_not_grow_past_its_initial_capacity_for_shallow_trees() {
    let tree = thousand_node_tree();
    let cursor = TreeCursor::new();
    assert_eq!(cursor.depth_used(), 0);
    assert!(tree.depth() <= 8, "the fixture tree is shallow");
    assert_eq!(
        TreeCursor::INITIAL_CAPACITY,
        8,
        "the documented starting capacity is what makes the shallow case free"
    );
}
