//! Spawn placement: random, authored, and the default spiral.
//!
//! A crowd's shape is a design decision, so these tests hold the three answers a game can give:
//! scatter them uniformly in a disc, put them exactly where a level designer said, or use the
//! spiral. Each one is checked through the plugin — add it, ask for a crowd, update — plus the
//! properties that make a spawner trustworthy: determinism from the seed, positions inside the
//! simulated volume, and no silent stacking under the default.

use bevy_app::App;
use bevy_flock::error::ConfigError;
use bevy_flock::prelude::*;
use bevy_flock::testkit::OrDie;

fn app_with(population: usize, placement: Placement) -> App {
    let mut app = App::new();
    let config = FlockConfig {
        population: PopulationSize::clamped(population),
        spawn_budget: 1_000_000,
        ..FlockConfig::preset(CrowdPreset::QuietVillage)
    };
    let config = config.with_placement(placement).or_die();
    app.add_plugins(FlockPlugin::new(config));
    app.update();
    app
}

fn positions(app: &mut App) -> Vec<Vec3> {
    app.world_mut()
        .query::<(&Npc, &Transform)>()
        .iter(app.world())
        .map(|(_, transform)| transform.translation)
        .collect()
}

#[test]
fn a_uniform_disc_scatters_the_crowd_inside_the_disc() {
    let centre = Vec3::new(20.0, 0.0, -30.0);
    let radius = 60.0;
    let mut app = app_with(500, Placement::uniform_disc(centre, radius));
    let placed = positions(&mut app);
    assert_eq!(placed.len(), 500);

    for point in &placed {
        let offset = Vec3::new(point.x - centre.x, 0.0, point.z - centre.z);
        assert!(
            offset.length() <= radius + 1e-3,
            "{} is outside the disc",
            offset.length()
        );
        assert!(point.y.abs() < 1e-6, "the crowd stays on its plane");
    }

    // A random placement that produced one point 500 times would also pass the bounds check.
    let mut unique = placed.clone();
    unique.sort_by(|a, b| a.x.total_cmp(&b.x).then(a.z.total_cmp(&b.z)));
    unique.dedup();
    assert_eq!(unique.len(), placed.len(), "no two NPCs should coincide");

    // And it should fill the disc, not hug the middle: half the radius is a quarter of the area,
    // so a sample this size lands well away from both 0% and 50%.
    let inner = placed
        .iter()
        .filter(|point| {
            let offset = Vec3::new(point.x - centre.x, 0.0, point.z - centre.z);
            offset.length() <= radius / 2.0
        })
        .count();
    let fraction = inner as f64 / placed.len() as f64;
    assert!(
        (0.15..0.36).contains(&fraction),
        "inner half held {fraction:.2} of the crowd"
    );
}

#[test]
fn a_uniform_rectangle_respects_its_corners() {
    let min = Vec3::new(-40.0, 0.0, 10.0);
    let max = Vec3::new(40.0, 0.0, 90.0);
    let mut app = app_with(400, Placement::uniform_rect(min, max));
    for point in positions(&mut app) {
        assert!((min.x..=max.x).contains(&point.x), "x {} escaped", point.x);
        assert!((min.z..=max.z).contains(&point.z), "z {} escaped", point.z);
    }
}

#[test]
fn authored_positions_are_used_as_written() {
    let authored = vec![
        Vec3::new(1.0, 0.0, 2.0),
        Vec3::new(-3.0, 0.0, 4.0),
        Vec3::new(5.0, 0.0, -6.0),
    ];
    let mut app = app_with(3, Placement::fixed(authored.clone()));
    let mut placed = positions(&mut app);
    placed.sort_by(|a, b| a.x.total_cmp(&b.x));
    let mut expected = authored;
    expected.sort_by(|a, b| a.x.total_cmp(&b.x));
    // The app has run one frame, so integration has nudged each NPC by a millimetre or so;
    // what matters is that they started at the authored points rather than in a spiral.
    for (actual, wanted) in placed.iter().zip(&expected) {
        assert!(
            (*actual - *wanted).length() < 0.02,
            "NPC at {actual} should have started at {wanted}"
        );
    }
}

#[test]
fn a_short_authored_list_cycles_and_still_fills_the_population() {
    // Fewer places than people is a configuration smell, not a crash: the extras stand on the
    // first points and separation pulls them apart on the opening frames.
    let mut app = app_with(7, Placement::fixed(vec![Vec3::new(0.0, 0.0, 0.0)]));
    let placed = positions(&mut app);
    assert_eq!(placed.len(), 7);
    assert!(placed.iter().all(|point| point.length() < 1e-6));
}

#[test]
fn the_default_placement_is_the_spiral() {
    let mut app = app_with(
        100,
        FlockConfig::preset(CrowdPreset::QuietVillage).placement,
    );
    let placed = positions(&mut app);
    // The spiral is draw-free and index-ordered, so the free function must agree with it.
    for (index, point) in placed.iter().enumerate() {
        let expected = spawn_position(index as u32);
        assert!(
            (*point - expected).length() < 0.02,
            "NPC {index} at {point} should have started at {expected}"
        );
    }
}

#[test]
fn the_same_seed_places_the_same_crowd() {
    let placement = Placement::uniform_disc(Vec3::new(5.0, 0.0, 5.0), 50.0);
    let mut first = app_with(200, placement.clone());
    let mut second = app_with(200, placement);
    let mut a = positions(&mut first);
    let mut b = positions(&mut second);
    a.sort_by(|x, y| x.x.total_cmp(&y.x).then(x.z.total_cmp(&y.z)));
    b.sort_by(|x, y| x.x.total_cmp(&y.x).then(x.z.total_cmp(&y.z)));
    assert_eq!(
        a, b,
        "a replay must place the same people in the same spots"
    );
}

#[test]
fn a_placement_outside_the_volume_is_refused_before_it_spawns() {
    let config = FlockConfig::preset(CrowdPreset::QuietVillage);
    let outside = Placement::uniform_disc(Vec3::new(0.0, 0.0, 0.0), 5_000.0);
    let rejected = config.with_placement(outside);
    assert!(
        matches!(rejected, Err(ConfigError::PlacementOutsideGrid { .. })),
        "a disc larger than the world must not reach the spawner"
    );
}

#[test]
fn a_random_placement_still_produces_the_same_people() {
    // Placement draws come after temperament and archetype, and the spiral draws nothing, so a
    // game that switches placement keeps the population it had. This holds the "draws nothing"
    // half of that promise: the identities of a random crowd must match the spiral's.
    let mut spiral = app_with(64, Placement::default());
    let mut random = app_with(64, Placement::uniform_disc(Vec3::ZERO, 40.0));
    let identities = |app: &mut App| {
        let mut found: Vec<(u32, Archetype)> = app
            .world_mut()
            .query::<(&Npc, &NpcIdentity)>()
            .iter(app.world())
            .map(|(npc, identity)| (npc.0.index(), identity.archetype))
            .collect();
        found.sort_by_key(|(index, _)| *index);
        found
    };
    assert_eq!(identities(&mut spiral), identities(&mut random));
}

#[test]
fn a_volume_crowd_spawns_at_height_and_stays_there() {
    // The end-to-end version of the kernel's vertical test: this crate was planar, and the way a
    // planar assumption comes back is a crowd that spawns in a box and is then flattened by the
    // first steering pass. Nine frames later the heights must still be spread.
    let min = Vec3::new(-30.0, 0.0, -30.0);
    let max = Vec3::new(30.0, 25.0, 30.0);
    let mut app = app_with(300, Placement::uniform_volume(min, max));
    for _ in 0..8 {
        app.update();
    }
    let mut heights: Vec<f32> = app
        .world_mut()
        .query::<(&Npc, &Transform)>()
        .iter(app.world())
        .map(|(_, transform)| transform.translation.y)
        .collect();
    assert_eq!(heights.len(), 300);
    heights.sort_by(f32::total_cmp);
    let low = heights.first().copied().unwrap_or(0.0);
    let high = heights.last().copied().unwrap_or(0.0);
    assert!(
        high - low > 5.0,
        "heights collapsed to {low}..{high}: something is still flattening the crowd"
    );
    assert!(
        low >= -0.5 && high <= 25.5,
        "heights left the box: {low}..{high}"
    );
}
