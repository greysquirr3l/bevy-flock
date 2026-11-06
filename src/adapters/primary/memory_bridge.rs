//! The ECS half of memory: who gets remembered, and what their tree can see.
//!
//! Three jobs, in this order every tick:
//!
//! 1. [`enrol_the_present`] gives a row to the NPCs nearest a player, up to a per-tick
//!    budget. Memory is a fixed resource, so it is granted by proximity rather than by
//!    existing — the NPCs the player can actually meet are the ones who remember them.
//! 2. [`publish_memory`] copies the strongest memory an enrolled NPC holds onto its
//!    blackboard, once per tick, so a tree can ask "do I know you?" for the cost of a
//!    slot read ([`crate::behavior::memory_leaves`]).
//! 3. [`forget_the_absent`] clears those keys again when nobody is in range: a stale
//!    "friendly" flag on an NPC staring at an empty street is worse than no flag.
//!
//! Behind the `memory` feature, because the store is: a crowd of a million does not pay
//! for any of this, and a game that enables it chooses a capacity and therefore a bill.
use crate::adapters::primary::cognition::NpcBlackboard;
use crate::adapters::primary::components::Npc;
use crate::adapters::primary::perception::{KEY_PLAYER_DISTANCE, KEY_PLAYER_SEEN};
use crate::application::SimClock;
use crate::behavior::blackboard::Value;
use crate::behavior::memory_leaves::{
    KEY_MEMORY_FAMILIARITY, KEY_MEMORY_FLAGS, KEY_MEMORY_VALENCE,
};
use crate::domain::memory::{DecayPolicy, MemoryIndex, MemoryStore, enrolment_order};
use bevy_app::prelude::{App, Update};
use bevy_ecs::prelude::*;
use smallvec::SmallVec;

/// How memory is granted and published.
#[derive(Resource, Debug, Clone, Copy, PartialEq)]
pub struct MemoryConfig {
    /// The radius, in metres, inside which an NPC is close enough to remember someone.
    ///
    /// Deliberately larger than perception range: an NPC who noticed the player at the
    /// edge of their sight should still hold the memory when the player walks away.
    pub publish_radius: f32,
    /// How many new rows may be created in one tick.
    ///
    /// The guard against a player teleporting into a packed square and enrolling
    /// thousands of rows in a single frame. Enrolment drains over subsequent ticks,
    /// nearest first, so nobody important is missed.
    pub enrol_budget: u32,
    /// How fast a memory fades. Decay is lazy: nothing is written until a read.
    pub decay: DecayPolicy,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            publish_radius: 24.0,
            enrol_budget: 64,
            decay: DecayPolicy::MINUTE,
        }
    }
}

impl MemoryConfig {
    /// Set the enrolment radius.
    #[must_use]
    pub fn with_publish_radius(mut self, metres: f32) -> Self {
        self.publish_radius = metres;
        self
    }

    /// Set the per-tick enrolment budget.
    #[must_use]
    pub const fn with_enrol_budget(mut self, rows: u32) -> Self {
        self.enrol_budget = rows;
        self
    }

    /// Set how fast memory fades.
    #[must_use]
    pub const fn with_decay(mut self, decay: DecayPolicy) -> Self {
        self.decay = decay;
        self
    }

    /// The radius as a non-negative value: a negative radius remembers nobody, which is
    /// a configuration mistake rather than a policy.
    #[must_use]
    fn radius(&self) -> f32 {
        if self.publish_radius.is_finite() {
            self.publish_radius.max(0.0)
        } else {
            Self::default().publish_radius
        }
    }
}

/// The store row an NPC owns.
///
/// A component rather than a field on [`Npc`] because it is optional: most of a large
/// crowd has no row, and a component that is usually absent costs nothing to query for.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct NpcMemoryRow(pub MemoryIndex);

/// What the bridge did last tick.
///
/// Counters rather than a log: a game can show "1,284 of 5,000 remember you" without
/// paying for per-NPC history, and a test can assert the budget without timing anything.
#[derive(Resource, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MemoryBridgeStats {
    /// Candidates that were close enough to be considered.
    pub candidates: u32,
    /// Rows created this tick.
    pub enrolled: u32,
    /// Blackboards refreshed with a memory view.
    pub published: u32,
    /// Blackboards whose view was cleared because nobody was in range.
    pub cleared: u32,
}

impl MemoryBridgeStats {
    /// Forget last tick's numbers. Called at the start of [`enrol_the_present`].
    pub const fn reset(&mut self) {
        self.candidates = 0;
        self.enrolled = 0;
        self.published = 0;
        self.cleared = 0;
    }

    /// Whether the budget ran out with candidates still waiting.
    ///
    /// The signal a game uses to raise `enrol_budget` or accept that the crowd is larger
    /// than the memory it can afford.
    #[must_use]
    pub const fn budget_bound(&self) -> bool {
        self.candidates > self.enrolled
    }
}

/// Give rows to the NPCs nearest a player, up to the per-tick budget.
///
/// Ordering comes from [`crate::domain::memory::enrolment_order`] — the same rule the
/// domain test pins — so the ECS path and the pure one cannot disagree about who gets
/// remembered.
pub fn enrol_the_present(
    mut commands: Commands,
    config: Res<MemoryConfig>,
    mut store: ResMut<MemoryStore>,
    mut stats: ResMut<MemoryBridgeStats>,
    candidates: Query<(Entity, &Npc, &NpcBlackboard), Without<NpcMemoryRow>>,
) {
    stats.reset();
    let radius = config.radius();
    let mut shortlist: SmallVec<[(Npc, f32, Entity); 64]> = SmallVec::new();
    for (entity, npc, board) in &candidates {
        let seen = matches!(board.0.get(KEY_PLAYER_SEEN), Some(Value::Bool(true)));
        if !seen {
            continue;
        }
        let Some(distance) = distance_of(board) else {
            continue;
        };
        if distance > radius {
            continue;
        }
        shortlist.push((*npc, distance, entity));
    }
    stats.candidates = u32::try_from(shortlist.len()).unwrap_or(u32::MAX);
    shortlist
        .sort_unstable_by(|left, right| enrolment_order((left.0.0, left.1), (right.0.0, right.1)));
    let budget = usize::try_from(config.enrol_budget).unwrap_or(usize::MAX);
    for (npc, _, entity) in shortlist.iter().take(budget) {
        let Some(index) = store.enrol(npc.0) else {
            // The store is full: stop rather than walk the rest of the shortlist doing
            // nothing. The statistics say the crowd outgrew its memory.
            break;
        };
        commands.entity(*entity).insert(NpcMemoryRow(index));
        stats.enrolled += 1;
    }
}

/// Copy each enrolled NPC's strongest memory onto its blackboard.
///
/// One store read per NPC per tick, not per leaf evaluation: a tree may ask four
/// questions of the same memory, and they should all cost a slot lookup.
pub fn publish_memory(
    store: Res<MemoryStore>,
    config: Res<MemoryConfig>,
    clock: Res<SimClock>,
    mut stats: ResMut<MemoryBridgeStats>,
    mut minds: Query<(&NpcMemoryRow, &mut NpcBlackboard)>,
) {
    for (row, mut board) in &mut minds {
        let seen = matches!(board.0.get(KEY_PLAYER_SEEN), Some(Value::Bool(true)));
        if !seen {
            continue;
        }
        match store.strongest_recall(row.0, clock.tick, config.decay) {
            Some((_subject, valence, familiarity, flags)) => {
                let _ = board
                    .0
                    .set(KEY_MEMORY_VALENCE, Value::Int(i64::from(valence)));
                let _ = board
                    .0
                    .set(KEY_MEMORY_FAMILIARITY, Value::Int(i64::from(familiarity)));
                let _ = board.0.set(KEY_MEMORY_FLAGS, Value::Int(i64::from(flags)));
                stats.published += 1;
            }
            None => {
                // Enrolled but remembers nobody: leave the blackboard without a view,
                // which is what a stranger looks like to a tree.
                clear_view(&mut board.0);
            }
        }
    }
}

/// Clear the memory view for NPCs whose player has walked away.
///
/// A view is a statement about the present. Leaving the last one in place would let a
/// shopkeeper greet a customer who left ten minutes ago.
pub fn forget_the_absent(
    mut stats: ResMut<MemoryBridgeStats>,
    mut minds: Query<&mut NpcBlackboard, With<NpcMemoryRow>>,
) {
    for mut board in &mut minds {
        let seen = matches!(board.0.get(KEY_PLAYER_SEEN), Some(Value::Bool(true)));
        if seen {
            continue;
        }
        if clear_view(&mut board.0) {
            stats.cleared += 1;
        }
    }
}

/// Remove the three memory keys; returns whether any were present.
fn clear_view(board: &mut crate::behavior::blackboard::Blackboard) -> bool {
    let mut removed = board.remove(KEY_MEMORY_VALENCE).is_some();
    removed |= board.remove(KEY_MEMORY_FAMILIARITY).is_some();
    removed |= board.remove(KEY_MEMORY_FLAGS).is_some();
    removed
}

/// The distance to the nearest noticed player, if the blackboard has one.
fn distance_of(board: &NpcBlackboard) -> Option<f32> {
    match board.0.get(KEY_PLAYER_DISTANCE) {
        Some(Value::Float(metres)) if metres.is_finite() && metres >= 0.0 => Some(metres),
        _ => None,
    }
}

/// Register the bridge's resources and systems.
///
/// Called by [`crate::adapters::primary::plugin::FlockPlugin`] when the `memory` feature
/// is on. Split from the plugin so a game that owns its own schedule can add exactly the
/// three systems it wants.
pub fn register(app: &mut App, config: MemoryConfig, capacity: u32, subjects: u16) {
    app.insert_resource(config)
        .insert_resource(MemoryStore::new(capacity, subjects))
        .init_resource::<MemoryBridgeStats>()
        .add_systems(
            Update,
            (enrol_the_present, publish_memory, forget_the_absent)
                .chain()
                .in_set(crate::adapters::primary::plugin::FlockSet::Perception),
        );
}

#[cfg(test)]
mod tests {
    use crate::testkit::OrDie;

    use super::*;
    use crate::behavior::blackboard::Blackboard;
    use crate::domain::ids::NpcId;
    use crate::domain::memory::{Encounter, EncounterKind, SubjectId};
    use crate::domain::tick::Tick;
    use bevy_app::prelude::{App, Update};
    use bevy_transform::prelude::Transform;

    fn app(config: MemoryConfig) -> App {
        let mut app = App::new();
        let mut store = MemoryStore::new(8, 4);
        // One NPC already remembers player 0 warmly; the rest are strangers.
        let index = store.enrol(NpcId::unique(1)).or_die();
        for _ in 0..30 {
            store.remember(
                index,
                SubjectId::new(0),
                &Encounter::new(EncounterKind::Spoke, Tick::ZERO).with_valence(500),
            );
        }
        app.insert_resource(config)
            .insert_resource(store)
            .insert_resource(SimClock::default())
            .init_resource::<MemoryBridgeStats>()
            .add_systems(
                Update,
                (enrol_the_present, publish_memory, forget_the_absent).chain(),
            );
        app
    }

    /// Spawn an NPC that already owns a store row, the way an enrolled NPC does.
    fn spawn_enrolled(app: &mut App, id: u32, row: MemoryIndex, distance: Option<f32>) -> Entity {
        let mut board = Blackboard::new();
        if let Some(metres) = distance {
            let _ = board.set(KEY_PLAYER_SEEN, Value::Bool(true));
            let _ = board.set(KEY_PLAYER_DISTANCE, Value::Float(metres));
        }
        app.world_mut()
            .spawn((
                Npc(NpcId::unique(id)),
                NpcBlackboard(board),
                NpcMemoryRow(row),
                Transform::default(),
            ))
            .id()
    }

    fn spawn_npc(app: &mut App, id: u32, distance: Option<f32>) -> Entity {
        let mut board = Blackboard::new();
        if let Some(metres) = distance {
            let _ = board.set(KEY_PLAYER_SEEN, Value::Bool(true));
            let _ = board.set(KEY_PLAYER_DISTANCE, Value::Float(metres));
        }
        app.world_mut()
            .spawn((
                Npc(NpcId::unique(id)),
                NpcBlackboard(board),
                Transform::default(),
            ))
            .id()
    }

    #[test]
    fn only_the_near_are_enrolled_and_the_budget_is_respected() {
        let mut app = app(MemoryConfig::default().with_enrol_budget(2));
        let near = spawn_npc(&mut app, 10, Some(1.0));
        let nearer = spawn_npc(&mut app, 11, Some(0.5));
        let far = spawn_npc(&mut app, 12, Some(500.0));
        app.update();
        let stats = *app.world().resource::<MemoryBridgeStats>();
        assert_eq!(stats.candidates, 2);
        assert_eq!(stats.enrolled, 2);
        assert!(app.world().entity(nearer).get::<NpcMemoryRow>().is_some());
        assert!(app.world().entity(near).get::<NpcMemoryRow>().is_some());
        assert!(app.world().entity(far).get::<NpcMemoryRow>().is_none());
    }

    #[test]
    fn the_nearest_are_served_first_when_the_budget_binds() {
        let mut app = app(MemoryConfig::default().with_enrol_budget(1));
        let near = spawn_npc(&mut app, 20, Some(2.0));
        let nearer = spawn_npc(&mut app, 21, Some(1.0));
        app.update();
        assert!(app.world().entity(nearer).get::<NpcMemoryRow>().is_some());
        assert!(app.world().entity(near).get::<NpcMemoryRow>().is_none());
        let stats = *app.world().resource::<MemoryBridgeStats>();
        assert!(stats.budget_bound());
    }

    #[test]
    fn an_enrolled_npc_publishes_what_it_remembers() {
        let mut app = app(MemoryConfig::default());
        // Identity 1 is the NPC the fixture already gave a memory row to; the component
        // is what tells the bridge the row exists.
        let entity = spawn_enrolled(&mut app, 1, MemoryIndex::new(0), Some(1.0));
        app.update();
        let board = app.world().entity(entity).get::<NpcBlackboard>().or_die();
        // Thirty conversations at eight familiarity each, with no decay elapsed.
        assert_eq!(
            board.0.get(KEY_MEMORY_FAMILIARITY),
            Some(Value::Int(240)),
            "30 x familiarity_per_encounter, no decay elapsed"
        );
        assert!(
            matches!(board.0.get(KEY_MEMORY_VALENCE), Some(Value::Int(valence)) if valence > 0),
            "the greeting should read as welcome: {:?}",
            board.0.get(KEY_MEMORY_VALENCE)
        );
        let stats = *app.world().resource::<MemoryBridgeStats>();
        assert!(stats.published >= 1);
    }

    #[test]
    fn a_stranger_publishes_nothing() {
        let mut app = app(MemoryConfig::default());
        let entity = spawn_npc(&mut app, 99, Some(1.0));
        app.update();
        let board = app.world().entity(entity).get::<NpcBlackboard>().or_die();
        assert_eq!(board.0.get(KEY_MEMORY_FAMILIARITY), None);
    }

    #[test]
    fn walking_away_clears_the_view() {
        let mut app = app(MemoryConfig::default());
        let entity = spawn_enrolled(&mut app, 1, MemoryIndex::new(0), Some(1.0));
        app.update();
        assert!(
            app.world()
                .entity(entity)
                .get::<NpcBlackboard>()
                .or_die()
                .0
                .get(KEY_MEMORY_FAMILIARITY)
                .is_some()
        );

        // The player leaves: perception clears the seen flag, the bridge clears the view.
        {
            let world = app.world_mut();
            let mut entity_mut = world.entity_mut(entity);
            let mut board = entity_mut.get_mut::<NpcBlackboard>().or_die();
            let _ = board.0.remove(KEY_PLAYER_SEEN);
        }
        app.update();
        let board = app.world().entity(entity).get::<NpcBlackboard>().or_die();
        assert_eq!(board.0.get(KEY_MEMORY_FAMILIARITY), None);
        assert!(app.world().resource::<MemoryBridgeStats>().cleared >= 1);
    }
}
