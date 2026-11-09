//! The binding writes weights into a player that already has them. It must not allocate to do it.
//!
//! A crowd of ten thousand bodies at sixty ticks a second is six hundred thousand binding passes a
//! second. One `Vec` per pass is a garbage collector's worth of churn in a system that is supposed to
//! cost nothing, which is why this test exists rather than a claim in a doc comment.
//!
//! The counting allocator is the sanctioned `unsafe` in this crate: there is no other way to observe
//! an allocation from safe code. It is confined to this one test binary, and the audit records the
//! exception with its reason.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use bevy_animation::AnimationPlayer;
use bevy_animation::graph::AnimationNodeIndex;
use bevy_asset::Assets;
use bevy_ecs::prelude::{Schedule, World};
use bevy_flock::prelude::Locomotion;
use bevy_flock_anim::{ClipBindings, LocomotionProfile, apply_locomotion_blend};

/// Counts every allocation the process makes.
struct Counting;

// A thread-local rather than a global: `cargo test` runs the tests in this binary on several
// threads, and a global counter would charge one test for another's allocations. The const
// initialiser matters — a lazily initialised thread-local would allocate on its own first access,
// which is exactly the thing being measured.
thread_local! {
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

/// How many allocations this thread has made.
fn allocations() -> usize {
    ALLOCATIONS.with(Cell::get)
}

#[allow(
    unsafe_code,
    reason = "a global allocator is the only way to observe allocations from safe code; confined to this test binary and recorded in scripts/lint_audit.py"
)]
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.with(|count| count.set(count.get() + 1));
        // SAFETY: the layout came from the allocator API and is forwarded unchanged.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: the pointer came from `System.alloc` through this allocator and is returned to it
        // with the same layout.
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCATIONS.with(|count| count.set(count.get() + 1));
        // SAFETY: the pointer and layout are the ones the caller received, forwarded unchanged.
        unsafe { System.realloc(pointer, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// The profile the crate's documentation uses: a 1.4 m/s walk, a 5 m/s run, a 1.5 m stride.
fn profile() -> LocomotionProfile {
    LocomotionProfile::try_new(1.4, 5.0, 1.5, 0.5, 2.0)
        .unwrap_or_else(|_| unreachable!("the fixture profile is valid"))
}

#[test]
fn the_binding_does_not_allocate_once_it_is_warm() {
    let mut world = World::new();
    let mut assets = Assets::<LocomotionProfile>::default();
    let handle = assets.add(profile());
    world.insert_resource(assets);

    let nodes = [
        AnimationNodeIndex::new(0),
        AnimationNodeIndex::new(1),
        AnimationNodeIndex::new(2),
        AnimationNodeIndex::new(3),
    ];
    let bindings =
        ClipBindings::new(handle, nodes[0], nodes[1], nodes[2]).with_turn_in_place(nodes[3]);
    world.spawn((Locomotion::default(), bindings, AnimationPlayer::default()));

    let mut schedule = Schedule::default();
    schedule.add_systems(apply_locomotion_blend);
    if schedule.initialize(&mut world).is_err() {
        unreachable!("a single-system schedule always builds");
    }

    // Warm-up: the first run inserts four animations into the player's map, which is the last time
    // this path is allowed to allocate.
    for _ in 0..8 {
        schedule.run(&mut world);
    }

    let before = allocations();
    for _ in 0..1000 {
        schedule.run(&mut world);
    }
    let observed = allocations() - before;
    assert_eq!(
        observed, 0,
        "{observed} allocation(s) over a thousand binding passes"
    );
}

#[test]
fn the_binding_does_not_allocate_with_a_thousand_bodies() {
    let mut world = World::new();
    let mut assets = Assets::<LocomotionProfile>::default();
    let handle = assets.add(profile());
    world.insert_resource(assets);

    let bindings = ClipBindings::new(
        handle,
        AnimationNodeIndex::new(0),
        AnimationNodeIndex::new(1),
        AnimationNodeIndex::new(2),
    );
    for _ in 0..1000 {
        world.spawn((
            Locomotion::default(),
            bindings.clone(),
            AnimationPlayer::default(),
        ));
    }

    let mut schedule = Schedule::default();
    schedule.add_systems(apply_locomotion_blend);
    if schedule.initialize(&mut world).is_err() {
        unreachable!("a single-system schedule always builds");
    }
    for _ in 0..8 {
        schedule.run(&mut world);
    }

    let before = allocations();
    for _ in 0..10 {
        schedule.run(&mut world);
    }
    let observed = allocations() - before;
    assert_eq!(
        observed, 0,
        "{observed} allocation(s) over ten passes of a thousand bodies"
    );
}
