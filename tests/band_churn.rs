//! Churn regression: the band must converge to its target and then stop moving people.
//!
//! The defect this pins (ADR 0025) held the band at 256 live agents forever: every frame it
//! manifested 256 and folded the 256 from the frame before. Population was conserved and every
//! gate passed, which is why it survived five gate reviews — the population was fine, the
//! *work* was cancelling.
//!
//! Three causes, all of them now guarded here:
//!
//! 1. the manifest window was square, so its corners reached `radius * sqrt(2)` = 1.41 radius
//!    while folding began at 1.15 radius: everyone manifested from the corners was born
//!    outside the band and folded next tick;
//! 2. `seed_field` filled the field from the origin and stopped when it ran out, so the focus
//!    at the centre stood in empty country and the manifest reached outward past its radius;
//! 3. nothing checked the spawn point against the fold bound at all.
use bevy_app::prelude::*;
use bevy_ecs::prelude::*;
use bevy_flock::testkit::OrDie;
use bevy_transform::prelude::Transform;

use bevy_flock::adapters::primary::band::{Band, BandAgent, BandConfig, BandPlugin, seed_field};
use bevy_flock::adapters::primary::cognition::PlayerFocus;

const SEEDED: u32 = 40_000;

struct Harness {
    app: App,
}

impl Harness {
    fn new(config: BandConfig) -> Self {
        let mut app = App::new();
        app.add_plugins(BandPlugin::new(config));
        let _focus = app
            .world_mut()
            .spawn((PlayerFocus, Transform::from_xyz(256.0, 256.0, 0.0)))
            .id();
        let mut band = app.world_mut().resource_mut::<Band>();
        seed_field(&mut band, SEEDED);
        Self { app }
    }

    fn tick(&mut self) {
        self.app.update();
    }

    fn band(&self) -> &Band {
        self.app.world().resource::<Band>()
    }

    fn live(&mut self) -> usize {
        let mut query = self
            .app
            .world_mut()
            .query_filtered::<&Transform, With<BandAgent>>();
        let world = self.app.world_mut();
        let mut count = 0;
        let query = query.query(world);
        for _ in query.iter() {
            count += 1;
        }
        count
    }

    /// Distance from the focus for every live agent, in metres.
    fn distances(&mut self) -> Vec<f32> {
        let focus = self
            .app
            .world_mut()
            .query_filtered::<&Transform, With<PlayerFocus>>()
            .iter(self.app.world())
            .next()
            .map(|transform| (transform.translation.x, transform.translation.y))
            .or_die();
        let mut query = self.app.world_mut().query::<&Transform>();
        let world = self.app.world();
        query
            .iter(world)
            .filter(|transform| transform.translation.z == 0.0)
            .map(|transform| {
                let dx = transform.translation.x - focus.0;
                let dy = transform.translation.y - focus.1;
                dx.hypot(dy)
            })
            .filter(|distance| !distance.is_nan())
            .collect()
    }
}

#[test]
fn the_band_converges_to_its_target_and_then_stops_churning() {
    let mut harness = Harness::new(BandConfig::default());
    let target = BandConfig::default().target as usize;

    // 5,000 at a ration of 256 is twenty frames of manifesting; forty is generous.
    for _ in 0..40 {
        harness.tick();
    }
    let live = harness.live();
    assert_eq!(
        live,
        target,
        "the band did not reach its target: live={live} target={target} manifested={} folded={}",
        harness.band().manifested,
        harness.band().folded
    );

    // From here on, nothing should move. The defect this test exists for looked exactly like
    // a healthy band: +256 manifested and +256 folded every single frame.
    let manifested = harness.band().manifested;
    let folded = harness.band().folded;
    for _ in 0..60 {
        harness.tick();
    }
    assert_eq!(
        harness.band().manifested,
        manifested,
        "the band is still manifesting after convergence: {} extra over 60 frames",
        harness.band().manifested - manifested
    );
    assert_eq!(
        harness.band().folded,
        folded,
        "the band is folding after convergence: {} over 60 frames",
        harness.band().folded - folded
    );
    assert_eq!(harness.live(), target, "the live count drifted");

    // Conservation, in case a fix ever stops churning by losing people instead.
    let density = harness.band().field.population();
    assert_eq!(
        density + target as u64,
        u64::from(SEEDED),
        "population is not conserved: density={density} live={target}"
    );
}

#[test]
fn nobody_is_manifested_outside_the_band() {
    let mut harness = Harness::new(BandConfig::default());
    let radius = harness.band().radius;
    let fold = harness.band().fold_radius;

    for tick in 0..40 {
        harness.tick();
        for distance in harness.distances() {
            assert!(
                distance <= radius + 1.0,
                "tick {tick}: an agent appeared {distance:.1} m from the focus, outside the \
                 {radius:.1} m manifest radius (fold bound {fold:.1} m)"
            );
        }
    }
}

#[test]
fn a_zero_slack_band_does_not_fold_what_it_manifests() {
    // The degenerate setting: radius == fold. Every spawned agent is on the boundary of the
    // region that folds, so without the spawn guard this oscillates at the ration forever.
    let mut harness = Harness::new(BandConfig {
        fold_slack: 1.0,
        ..BandConfig::default()
    });
    for _ in 0..40 {
        harness.tick();
    }
    let folded = harness.band().folded;
    assert_eq!(
        folded, 0,
        "a zero-slack band folded {folded} agents, which can only be churn"
    );
    assert_eq!(harness.live(), BandConfig::default().target as usize);
}

#[test]
fn the_field_holds_the_seeded_population_where_it_was_put() {
    // The seeding utility is part of the story: a field whose people are all in one corner
    // makes the manifest behave like a vacuum cleaner. Uniform means uniform.
    let harness = Harness::new(BandConfig::default());
    let field = &harness.band().field;
    let (mut empty, mut occupied) = (0, 0);
    for y in 0..field.height() {
        for x in 0..field.width() {
            let cell = bevy_flock::domain::continuum::CellId::new(x, y);
            match field.cell(cell).map(|state| state.population()) {
                Some(population) if population > 0 => occupied += 1,
                _ => empty += 1,
            }
        }
    }
    let cells = empty + occupied;
    assert!(
        occupied * 100 / cells >= 55,
        "seeding left {empty} of {cells} cells empty; 40,000 people in a 256x256 field should \
         occupy most of it"
    );
    // The property that actually matters: the disc the band manifests from has to hold at
    // least the target, or the band could never fill and the test above would be measuring an
    // empty world rather than a converging band.
    let radius = harness.band().radius;
    let focus = [256.0, 256.0];
    let mut inside = 0_u64;
    for y in 0..field.height() {
        for x in 0..field.width() {
            let cell = bevy_flock::domain::continuum::CellId::new(x, y);
            let at = field.centre_of(cell);
            let dx = at[0] - focus[0];
            let dy = at[1] - focus[1];
            if dx * dx + dy * dy <= radius * radius {
                inside += u64::from(field.cell(cell).map_or(0, |state| state.population()));
            }
        }
    }
    let target = u64::from(BandConfig::default().target);
    assert!(
        inside >= target,
        "the {radius:.1} m manifest radius holds {inside} people, fewer than the {target} target"
    );
}
