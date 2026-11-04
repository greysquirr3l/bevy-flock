//! The scheduler through the plugin, not around it.
//!
//! The unit tests in `adapters::primary::cognition` build their own minimal app, which is
//! how a system gets tested. This one starts from [`FlockPlugin`], so it fails if the
//! systems are not registered, not ordered, or not in the schedule the plugin installs —
//! the three mistakes a unit test cannot see.
#![expect(
    clippy::expect_used,
    reason = "the fixture asserts the tree it just built"
)]

use std::sync::Arc;

use bevy_app::prelude::*;
use bevy_ecs::prelude::*;
use bevy_transform::prelude::Transform;

use bevy_flock::adapters::primary::cognition::{
    NpcBlackboard, NpcThinkWeight, PlayerFocus, ThinkSchedule,
};
use bevy_flock::adapters::primary::components::{Npc, NpcBrain};
use bevy_flock::adapters::primary::plugin::{CrowdPreset, FlockConfig, FlockPlugin};
use bevy_flock::application::SimClock;
use bevy_flock::behavior::tree::{BehaviorTree, NodeSpec};
use bevy_flock::domain::cost::VirtualCost;
use bevy_flock::domain::ids::NpcId;
use bevy_flock::domain::tick::Tick;
use bevy_flock::domain::weight::ThinkWeight;

fn app() -> App {
    let mut app = App::new();
    app.add_plugins(FlockPlugin::new(FlockConfig::preset(
        CrowdPreset::QuietVillage,
    )));
    app
}

fn tree() -> Arc<BehaviorTree> {
    Arc::new(BehaviorTree::try_from_spec(NodeSpec::Action(0)).expect("a leaf is a tree"))
}

fn spawn_brain(app: &mut App, id: u32, x: f32, cost: VirtualCost) {
    app.world_mut().spawn((
        Npc(NpcId::unique(id)),
        Transform::from_xyz(x, 0.0, 0.0),
        NpcBrain::new(tree(), cost),
        NpcBlackboard::default(),
        NpcThinkWeight::default(),
    ));
}

fn advance(app: &mut App, frames: u64) {
    for frame in 0..frames {
        let tick = app.world().resource::<SimClock>().tick.get() + 1;
        let _ = frame;
        app.world_mut().resource_mut::<SimClock>().tick = Tick::new(tick);
        app.update();
    }
}

#[test]
fn the_plugin_registers_the_scheduler_and_counts_what_it_planned() {
    let mut app = app();
    // Ten, not fifty: the preset's configured population is the schedule's capacity, and a
    // test that outruns it would be measuring the capacity guard instead of the wiring.
    for id in 0..10 {
        spawn_brain(&mut app, id, 400.0, VirtualCost::from_micros(2));
    }
    advance(&mut app, 1);

    let stats = app.world().resource::<ThinkSchedule>().stats();
    assert_eq!(stats.enrolled, 10, "every brain is a participant");
    assert!(stats.chosen > 0, "the default budget buys some thinking");
    assert_eq!(
        stats.chosen + stats.deferred,
        stats.enrolled,
        "every participant is either chosen or deferred"
    );
}

#[test]
fn a_crowd_near_the_player_thinks_more_often_than_one_far_away() {
    // The question this answers is the one that matters in a game: does walking toward the
    // player buy an NPC more thinking? With the plugin installed and nothing else set up,
    // it has to.
    let mut app = app();
    app.world_mut()
        .spawn((PlayerFocus, Transform::from_xyz(0.0, 0.0, 0.0)));
    spawn_brain(&mut app, 0, 2.0, VirtualCost::from_micros(4));
    spawn_brain(&mut app, 1, 600.0, VirtualCost::from_micros(4));

    // A budget that fits one think per tick, so cadence is the only variable.
    app.world_mut()
        .resource_mut::<ThinkSchedule>()
        .set_budget(VirtualCost::from_micros(4));

    let mut near = 0;
    let mut far = 0;
    for _ in 0..60 {
        advance(&mut app, 1);
        let schedule = app.world().resource::<ThinkSchedule>();
        near += u32::from(schedule.is_chosen(0));
        far += u32::from(schedule.is_chosen(1));
    }
    assert!(near > 0, "the NPC beside the player never thought");
    assert!(
        near >= 3 * far.max(1),
        "proximity should buy cadence: near {near}, far {far}"
    );
}

#[test]
fn the_weight_component_is_written_and_survives_hysteresis() {
    let mut app = app();
    app.world_mut()
        .spawn((PlayerFocus, Transform::from_xyz(0.0, 0.0, 0.0)));
    spawn_brain(&mut app, 0, 10.0, VirtualCost::from_micros(1));
    advance(&mut app, 1);

    // Filtered: the preset spawns its own population with the same identifiers, and only
    // the entities this test gave a brain are its business.
    let mut query = app
        .world_mut()
        .query_filtered::<(&Npc, &NpcThinkWeight), With<NpcBrain>>();
    let weight = query
        .iter(app.world())
        .find(|(npc, _)| npc.0.index() == 0)
        .map(|(_, weight)| weight.0)
        .expect("the NPC exists");
    assert_eq!(
        weight,
        ThinkWeight::NEARBY,
        "10 units from the player is the nearby band"
    );

    // Move it to the boundary and jitter: the weight must hold.
    let entity = {
        let mut query = app
            .world_mut()
            .query_filtered::<(&Npc, Entity), With<NpcThinkWeight>>();
        query
            .iter(app.world())
            .find(|(npc, _)| npc.0.index() == 0)
            .map(|(_, entity)| entity)
            .expect("the NPC exists")
    };
    for step in 0..8 {
        let x = 30.0 + if step % 2 == 0 { 0.2 } else { -0.3 };
        if let Some(mut transform) = app.world_mut().get_mut::<Transform>(entity) {
            transform.translation.x = x;
        }
        advance(&mut app, 1);
    }
    let weight = app
        .world()
        .get::<NpcThinkWeight>(entity)
        .map(|weight| weight.0)
        .expect("the NPC exists");
    assert_eq!(
        weight,
        ThinkWeight::NEARBY,
        "jitter across the band edge must not demote"
    );
}
