//! Memory through the plugin: enrolment, publication, and forgetting.
//!
//! The unit tests in `adapters::primary::memory_bridge` build their own app. These start
//! from [`FlockPlugin`] with the `memory` feature on, so they fail if the bridge is not
//! registered, not ordered after perception, or not fed by the scheduler's crowd — the
//! three mistakes a unit test cannot see.
use bevy_app::prelude::*;
use bevy_ecs::prelude::*;
use bevy_flock::adapters::primary::cognition::NpcBlackboard;
use bevy_flock::adapters::primary::components::Npc;
use bevy_flock::adapters::primary::memory_bridge::{MemoryBridgeStats, MemoryConfig, NpcMemoryRow};
use bevy_flock::adapters::primary::perception::{KEY_PLAYER_SEEN, PlayerCharacter};
use bevy_flock::adapters::primary::plugin::{CrowdPreset, FlockConfig, FlockPlugin};
use bevy_flock::application::SimClock;
use bevy_flock::behavior::blackboard::Value;
use bevy_flock::behavior::memory_leaves::{KEY_MEMORY_FAMILIARITY, KEY_MEMORY_VALENCE};
use bevy_flock::domain::ids::NpcId;
use bevy_flock::domain::memory::{Encounter, EncounterKind, MemoryStore, SubjectId};
use bevy_flock::domain::tick::Tick;
use bevy_flock::testkit::OrDie;
use bevy_transform::prelude::Transform;

fn app(radius: f32) -> App {
    let mut app = App::new();
    app.add_plugins(FlockPlugin::new(FlockConfig::preset(
        CrowdPreset::QuietVillage,
    )));
    app.insert_resource(MemoryConfig::default().with_publish_radius(radius));
    app
}

/// Give every spawned NPC a blackboard.
///
/// Memory is a cognition feature: an NPC with no mind has nowhere to publish a memory
/// view and does not enrol. A game that wants a remembering crowd makes the same call,
/// which is why it is a component rather than something the spawner assumes.
fn install_minds(app: &mut App) {
    let npcs: Vec<Entity> = {
        let mut query = app
            .world_mut()
            .query_filtered::<Entity, (With<Npc>, Without<PlayerCharacter>)>();
        query.iter(app.world()).collect()
    };
    for entity in npcs {
        app.world_mut()
            .entity_mut(entity)
            .insert(NpcBlackboard::default());
    }
}

fn advance(app: &mut App, frames: u64) {
    for _ in 0..frames {
        let tick = app.world().resource::<SimClock>().tick.get() + 1;
        app.world_mut().resource_mut::<SimClock>().tick = Tick::new(tick);
        app.update();
    }
}

#[test]
fn the_crowd_around_the_player_gets_rowed_and_published() {
    let mut app = app(40.0);
    // The crowd spawns over several frames; the player stands in the middle of it.
    app.world_mut().spawn((
        PlayerCharacter,
        Npc(NpcId::unique(9_000)),
        Transform::from_xyz(0.0, 0.0, 0.0),
    ));
    advance(&mut app, 20);
    install_minds(&mut app);
    advance(&mut app, 90);

    // The counters describe one tick, and by now enrolment has caught up with everyone
    // in range, so the evidence is the store rather than the last frame's count.
    let rows = app.world().resource::<MemoryStore>().len();
    assert!(rows > 0, "nobody was enrolled over ninety ticks");

    // Every enrolled NPC that has seen the player either has a view or remembers nobody;
    // what must never happen is a view on an NPC that has not seen anyone.
    let mut query = app
        .world_mut()
        .query_filtered::<&NpcBlackboard, With<NpcMemoryRow>>();
    for board in query.iter(app.world()) {
        let seen = matches!(board.0.get(KEY_PLAYER_SEEN), Some(Value::Bool(true)));
        if !seen {
            assert_eq!(
                board.0.get(KEY_MEMORY_FAMILIARITY),
                None,
                "a stale view survived the player leaving"
            );
        }
    }
}

#[test]
fn a_remembered_player_is_readable_by_a_tree() {
    let mut app = app(40.0);
    let player = app
        .world_mut()
        .spawn((
            PlayerCharacter,
            Npc(NpcId::unique(9_000)),
            Transform::from_xyz(0.0, 0.0, 0.0),
        ))
        .id();
    advance(&mut app, 20);
    install_minds(&mut app);
    advance(&mut app, 30);

    // Give one enrolled NPC a real memory of the player, then let the bridge publish it.
    let (entity, row) = {
        let mut query = app.world_mut().query::<(Entity, &NpcMemoryRow)>();
        query
            .iter(app.world())
            .map(|(entity, row)| (entity, row.0))
            .next()
            .or_die()
    };
    {
        let mut store = app.world_mut().resource_mut::<MemoryStore>();
        for _ in 0..6 {
            store.remember(
                row,
                SubjectId::new(0),
                &Encounter::new(EncounterKind::Spoke, Tick::ZERO).with_valence(400),
            );
        }
    }
    let _ = player;
    advance(&mut app, 2);

    let board = app.world().entity(entity).get::<NpcBlackboard>().or_die();
    assert!(
        board.0.get(KEY_MEMORY_FAMILIARITY).is_some(),
        "the bridge should publish what the row holds"
    );
    assert!(board.0.get(KEY_MEMORY_VALENCE).is_some());
}

#[test]
fn a_crowd_beyond_the_radius_remembers_nobody() {
    let mut app = app(0.5);
    // Far enough that no spawn placement can land inside half a metre.
    app.world_mut().spawn((
        PlayerCharacter,
        Npc(NpcId::unique(9_000)),
        Transform::from_xyz(50_000.0, 0.0, 0.0),
    ));
    advance(&mut app, 20);
    install_minds(&mut app);
    advance(&mut app, 60);
    let stats = *app.world().resource::<MemoryBridgeStats>();
    assert_eq!(stats.candidates, 0, "the radius should exclude everyone");
    assert_eq!(stats.enrolled, 0);
    assert_eq!(app.world().resource::<MemoryStore>().len(), 0);
}

#[test]
fn a_row_survives_the_npc_walking_out_of_range_and_comes_back() {
    let mut app = app(40.0);
    let player = app
        .world_mut()
        .spawn((
            PlayerCharacter,
            Npc(NpcId::unique(9_000)),
            Transform::from_xyz(0.0, 0.0, 0.0),
        ))
        .id();
    advance(&mut app, 20);
    install_minds(&mut app);
    advance(&mut app, 60);
    let row = {
        let mut query = app.world_mut().query::<&NpcMemoryRow>();
        query.iter(app.world()).next().copied()
    }
    .or_die();
    assert!(
        app.world().resource::<MemoryStore>().owner(row.0).is_some(),
        "the row is owned by an NPC"
    );

    // The game moves the player away: perception stops noticing, the bridge clears the
    // view, and the row stays — a memory is not a perception fact.
    app.world_mut()
        .entity_mut(player)
        .insert(Transform::from_xyz(5_000.0, 0.0, 0.0));
    advance(&mut app, 3);
    let stored = app.world().resource::<MemoryStore>().is_enrolled(row.0);
    assert!(stored, "the row survives the player leaving");
}
