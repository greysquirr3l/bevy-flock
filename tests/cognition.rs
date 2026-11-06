//! Tiered cognition through the plugin, not around it.
//!
//! The unit tests in `adapters::primary::cognition` build their own app, which is how a
//! system gets tested. These start from [`FlockPlugin`], so they fail if the systems are
//! not registered, not ordered, or not in the schedule the plugin installs — the three
//! mistakes a unit test cannot see.

use bevy_flock::testkit::OrDie;
use std::sync::Arc;

use bevy_app::prelude::*;
use bevy_ecs::prelude::*;
use bevy_transform::prelude::Transform;

use bevy_flock::adapters::primary::cognition::{
    NpcBlackboard, NpcEngaged, NpcThinkWeight, NpcTier, PlayerFocus, ThinkSchedule,
};
use bevy_flock::adapters::primary::components::{Npc, NpcBrain};
use bevy_flock::adapters::primary::plugin::{CrowdPreset, FlockConfig, FlockPlugin};
use bevy_flock::application::SimClock;
use bevy_flock::behavior::tree::{BehaviorTree, NodeSpec};
use bevy_flock::domain::cognition::CognitionTier;
use bevy_flock::domain::cost::VirtualCost;
use bevy_flock::domain::ids::NpcId;
use bevy_flock::domain::tick::Tick;

fn app() -> App {
    let mut app = App::new();
    app.add_plugins(FlockPlugin::new(FlockConfig::preset(
        CrowdPreset::QuietVillage,
    )));
    app
}

fn tree() -> Arc<BehaviorTree> {
    Arc::new(BehaviorTree::try_from_spec(NodeSpec::Action(0)).or_die())
}

fn spawn_brain(app: &mut App, id: u32, x: f32, cost: VirtualCost) -> Entity {
    app.world_mut()
        .spawn((
            Npc(NpcId::unique(id)),
            Transform::from_xyz(x, 0.0, 0.0),
            NpcBrain::new(tree(), cost),
            NpcBlackboard::default(),
            NpcThinkWeight::default(),
            NpcTier::default(),
        ))
        .id()
}

fn advance(app: &mut App, frames: u64) {
    for _ in 0..frames {
        let tick = app.world().resource::<SimClock>().tick.get() + 1;
        app.world_mut().resource_mut::<SimClock>().tick = Tick::new(tick);
        app.update();
    }
}

fn tier_of(app: &App, entity: Entity) -> CognitionTier {
    app.world()
        .get::<NpcTier>(entity)
        .map(|tier| tier.0)
        .or_die()
}

#[test]
fn the_plugin_registers_the_tiered_scheduler() {
    let mut app = app();
    for id in 0..10 {
        spawn_brain(&mut app, id, 400.0, VirtualCost::from_micros(2));
    }
    advance(&mut app, 1);

    let stats = app.world().resource::<ThinkSchedule>().stats();
    assert_eq!(stats.enrolled, 10, "every brain is a participant");
    assert!(stats.chosen > 0, "the default budget buys some thinking");
    assert!(stats.spent.get() <= ThinkSchedule::DEFAULT_BUDGET.get());
    let per_tier: u32 = stats.per_tier.iter().map(|tier| tier.chosen).sum();
    assert_eq!(per_tier, stats.chosen, "the per-tier counts add up");
}

#[test]
fn approach_raises_the_tier_and_distance_lowers_it() {
    // The question the whole framework exists to answer: does walking toward the player
    // change what an NPC thinks with?
    let mut app = app();
    app.world_mut()
        .spawn((PlayerFocus, Transform::from_xyz(0.0, 0.0, 0.0)));
    let near = spawn_brain(&mut app, 0, 2.0, VirtualCost::from_micros(4));
    let far = spawn_brain(&mut app, 1, 600.0, VirtualCost::from_micros(1));
    advance(&mut app, 2); // the ration promotes a tier per tick, so give it two

    assert_eq!(tier_of(&app, near), CognitionTier::Deliberative);
    assert_eq!(tier_of(&app, far), CognitionTier::Reflex);

    // Walking away demotes, and demotion is immediate rather than rationed.
    if let Some(mut transform) = app.world_mut().get_mut::<Transform>(near) {
        transform.translation.x = 600.0;
    }
    advance(&mut app, 1);
    assert_eq!(tier_of(&app, near), CognitionTier::Reflex);
}

#[test]
fn a_conversation_outranks_distance() {
    let mut app = app();
    app.world_mut()
        .spawn((PlayerFocus, Transform::from_xyz(0.0, 0.0, 0.0)));
    let far = spawn_brain(&mut app, 0, 600.0, VirtualCost::from_micros(4));
    advance(&mut app, 1);
    assert_eq!(tier_of(&app, far), CognitionTier::Reflex);

    app.world_mut().entity_mut(far).insert(NpcEngaged);
    // Social is three rungs up; the ration allows one promotion per tick per NPC.
    advance(&mut app, 4);
    assert_eq!(
        tier_of(&app, far),
        CognitionTier::Social,
        "an NPC being spoken to reasons socially however far away it stands"
    );
}

#[test]
fn the_social_reserve_survives_a_crowd() {
    // A hundred reflex NPCs and one conversation. The conversation's tier has its own
    // queue and its own 5%, so it runs on the very first tick.
    let mut app = app();
    // The preset's population is the schedule's capacity; this test wants a hundred and
    // one participants, so it says so rather than quietly measuring the capacity guard.
    app.insert_resource(ThinkSchedule::new(512, ThinkSchedule::DEFAULT_BUDGET));
    app.world_mut()
        .spawn((PlayerFocus, Transform::from_xyz(0.0, 0.0, 0.0)));
    for id in 0..100 {
        spawn_brain(&mut app, id, 800.0, VirtualCost::from_micros(1));
    }
    let talker = spawn_brain(&mut app, 200, 3.0, VirtualCost::from_micros(20));
    app.world_mut().entity_mut(talker).insert(NpcEngaged);
    advance(&mut app, 4);

    let schedule = app.world().resource::<ThinkSchedule>();
    assert_eq!(
        schedule.tier_of(200),
        Some(CognitionTier::Social),
        "the talker climbed to the social tier"
    );
    assert!(
        schedule.stats().tier(CognitionTier::Social).budget.get() > 0,
        "the social tier has its own reserved budget"
    );
    assert!(
        schedule.stats().tier(CognitionTier::Reflex).enrolled >= 90,
        "the crowd is still enrolled and still cheap"
    );
}
