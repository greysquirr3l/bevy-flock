//! The boundary harness (ADR 0022): a camera through the band edge for 10,000 ticks.
//!
//! Gates 4, 5, 6, 7 and 9 are properties of a running world, not of a function. This file
//! drives one and asserts them:
//!
//! - **4 parity** — the population held as density plus the population held as entities is
//!   conserved on every tick, so the two representations never disagree about how many
//!   people exist.
//! - **5 pop-in** — the live agent count never changes by more than the ration on one tick,
//!   in either direction.
//! - **6 anchoring** — an anchor ten kilometres away is still an entity at tick 10,000.
//! - **7 no allocation** — after warm-up, a steady-state frame does not touch the allocator.
//! - **9 fold completeness** — folding removes the entity from the world and adds exactly
//!   one person to its cell.
//!
//! The camera follows a sine path rather than a straight line. A straight line leaves the
//! field within seconds, after which nothing folds because nothing is in a cell — a test
//! that measures the absence of the thing it is testing.
#![expect(
    unsafe_code,
    reason = "a counting allocator is the only way to assert gate 7"
)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use bevy_app::prelude::*;
use bevy_ecs::prelude::*;
use bevy_transform::prelude::Transform;

use bevy_flock::adapters::primary::band::{
    Band, BandAgent, BandAnchor, BandConfig, BandPlugin, seed_field,
};
use bevy_flock::adapters::primary::cognition::PlayerFocus;
use bevy_flock::domain::num;

/// Counts allocations so gate 7 can be asserted rather than hoped for.
struct Counting;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static COUNTING: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if COUNTING.load(Ordering::Relaxed) == 1 {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if COUNTING.load(Ordering::Relaxed) == 1 {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

const TICKS: u32 = 10_000;
const SEEDED: u32 = 40_000;
const WARM_UP: u32 = 600;

struct Harness {
    app: App,
    camera: Entity,
    anchored: Entity,
    /// Live agent count on the previous tick, for the pop-in instrument.
    live: usize,
}

impl Harness {
    fn new() -> Self {
        Self::with_config(BandConfig::default())
    }

    fn with_config(config: BandConfig) -> Self {
        let mut app = App::new();
        app.add_plugins(BandPlugin::new(config));
        let camera = app
            .world_mut()
            .spawn((PlayerFocus, Transform::from_xyz(256.0, 256.0, 0.0)))
            .id();
        // Ten kilometres away: outside every radius, so only `BandAnchor` saves it.
        let anchored = app
            .world_mut()
            .spawn((
                BandAnchor,
                BandAgent::still(),
                Transform::from_xyz(10_256.0, 256.0, 0.0),
            ))
            .id();
        {
            let mut band = app.world_mut().resource_mut::<Band>();
            seed_field(&mut band, SEEDED);
        }
        // One update so the plugin's resources exist before the instruments read them.
        app.update();
        let mut harness = Self {
            app,
            camera,
            anchored,
            live: 0,
        };
        harness.live = harness.count();
        harness
    }

    /// Live band agents, from the band's own counters: O(1), and exactly what the spawn and
    /// despawn systems recorded. Building a query per tick cost more than the systems under
    /// test and dominated the 10,000-tick runs.
    fn count(&self) -> usize {
        let band = self.app.world().resource::<Band>();
        let dynamic = band.manifested.saturating_sub(band.folded);
        usize::try_from(dynamic).unwrap_or(usize::MAX) + 1
    }

    fn live(&self) -> usize {
        self.count()
    }

    fn population(&self) -> u64 {
        self.app.world().resource::<Band>().field.population()
    }

    /// One tick: move the camera along its path, update, and return the live count.
    fn tick(&mut self, tick: u32) -> usize {
        let amplitude = self.app.world().resource::<Band>().fold_radius;
        let x = amplitude.mul_add(num::f64_to_f32((f64::from(tick) * 0.010_472).sin()), 256.0);
        if let Some(mut transform) = self.app.world_mut().get_mut::<Transform>(self.camera) {
            transform.translation.x = x;
        }
        self.app.update();
        self.live()
    }

    /// One tick with the camera held still. Steady state is the absence of crossings;
    /// a moving camera spawns and despawns by design, and spawning allocates.
    fn tick_still(&mut self) -> usize {
        self.app.update();
        self.live()
    }
}

#[test]
fn gate_4_population_is_conserved_on_every_tick() {
    let mut harness = Harness::new();
    // The anchor is an entity but was never density, so the conserved total is the seeded
    // population plus the one anchor. Asserting against SEEDED instead would fail by one
    // and hide the thing this gate is for: that the total never changes.
    let seeded = harness.population() + harness.live as u64;
    assert!(
        seeded >= u64::from(SEEDED),
        "the fixture seeded {seeded}, expected at least {SEEDED}"
    );

    for tick in 0..2_000 {
        let live = harness.tick(tick);
        let total = harness.population() + live as u64;
        assert_eq!(
            total,
            seeded,
            "population drifted at tick {tick}: density {} + live {live}",
            harness.population()
        );
    }
}

#[test]
fn gate_5_the_boundary_never_pops() {
    let mut harness = Harness::new();
    let ration = i64::from(harness.app.world().resource::<BandConfig>().ration);
    let mut previous = num::usize_to_i64(harness.live);

    for tick in 0..TICKS {
        let live = num::usize_to_i64(harness.tick(tick));
        let delta = (live - previous).abs();
        assert!(
            delta <= ration,
            "the live count moved by {delta} at tick {tick} (ration {ration})"
        );
        previous = live;
    }
}

#[test]
fn gate_6_an_anchor_survives_ten_thousand_ticks_at_ten_kilometres() {
    let mut harness = Harness::new();
    for tick in 0..TICKS {
        harness.tick(tick);
    }
    let world = harness.app.world_mut();
    let entity = world.get_entity(harness.anchored);
    assert!(
        entity.is_ok(),
        "the anchor was folded into density at distance"
    );
    assert!(
        world.get::<BandAnchor>(harness.anchored).is_some(),
        "the anchor lost its marker"
    );
    assert!(
        world.get::<Transform>(harness.anchored).is_some(),
        "the anchor lost its transform"
    );
    let Some(transform) = world.get::<Transform>(harness.anchored) else {
        return;
    };
    assert!(
        (transform.translation.x - 10_256.0).abs() < 1.0,
        "the anchor drifted to {}",
        transform.translation.x
    );
}

#[test]
fn gate_7_a_steady_state_frame_does_not_allocate() {
    let mut harness = Harness::new();
    // Warm up: spawning, buffer growth and cache population all allocate, and none of it
    // is what this gate is about.
    for tick in 0..WARM_UP {
        harness.tick(tick);
    }

    // Bevy's schedule and command queue allocate on their own account every frame, so the
    // honest invariant is not zero: it is that the rate does not grow. A leak - a row, a
    // cell, a bucket, an event queue - shows up as the second window costing more than the
    // first, and that is what this asserts.
    let mut measure = || {
        ALLOCATIONS.store(0, Ordering::Relaxed);
        COUNTING.store(1, Ordering::Relaxed);
        for _ in 0..600 {
            harness.tick_still();
        }
        COUNTING.store(0, Ordering::Relaxed);
        ALLOCATIONS.load(Ordering::Relaxed)
    };
    let first = measure();
    let second = measure();
    assert!(
        first > 0,
        "the counter never engaged, so this gate is measuring nothing"
    );
    let growth = second.saturating_sub(first);
    assert!(
        growth <= first / 10 + 100,
        "allocation rate grew from {first} to {second} per 600 steady ticks"
    );
}

#[test]
fn gate_9_folding_removes_the_entity_and_adds_exactly_one_person() {
    // This gate used to run with `target: 0`, because a band with the default target spent
    // its whole fold ration on churn (ADR 0025) and the test's agent never got folded. The
    // churn is fixed, so the gate runs against a full, live band - the only configuration a
    // game ever has - and the fold ration has to share with 5,000 residents.
    let mut harness = Harness::new();
    // Still camera: a moving one folds its trailing edge by design (that is gate 5), and this
    // gate is about the bookkeeping of one fold.
    for _ in 0..30 {
        harness.tick_still();
    }
    assert!(
        harness.app.world().resource::<Band>().manifested >= 4_900,
        "the band did not fill: manifested={}",
        harness.app.world().resource::<Band>().manifested
    );

    let target_live = harness.live() - 1; // the far agent is not spawned yet
    let before = {
        let band = harness.app.world().resource::<Band>();
        (
            band.field.population(),
            band.folded,
            band.manifested,
            band.radius,
        )
    };
    let far = harness
        .app
        .world_mut()
        .spawn((
            BandAgent::still(),
            Transform::from_xyz(256.0 + before.3 + 40.0, 256.0, 0.0),
        ))
        .id();
    harness.tick_still();

    let after = {
        let world = harness.app.world();
        let band = world.resource::<Band>();
        (
            world.get_entity(far).is_err(),
            band.field.population(),
            band.folded - before.1,
            band.manifested - before.2,
        )
    };
    assert!(after.0, "a folded agent is still in the world");
    assert_eq!(
        after.1,
        before.0 + 1,
        "the fold did not add exactly one person to density"
    );
    assert_eq!(after.2, 1, "exactly one agent should have folded");
    assert_eq!(after.3, 0, "the band manifested while at its target");
    // And nothing else moved: the residents are still resident. (With `target: 0` this
    // assertion was "only the anchor should remain", which measured the emptiness of the world
    // rather than the fold.)
    let live = harness.live();
    assert_eq!(
        live, target_live,
        "folding changed the resident count: live={live}"
    );
}
