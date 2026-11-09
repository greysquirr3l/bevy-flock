//! Noticing people: the bridge between a crowd and the characters a player owns.
//!
//! An NPC that never notices the player cannot greet them, resent them or flee
//! from them. This module is the smallest thing that makes that possible: a marker
//! component for player characters, a per-NPC record of who has been noticed and
//! when, and a system that fills both in — plus the two blackboard values every
//! behaviour reads (`KEY_PLAYER_SEEN`, `KEY_PLAYER_DISTANCE`).
//!
//! It is deliberately not a raycast and not a field of view. A crowd of ten
//! thousand cannot afford a physics query each, and a game that wants line of
//! sight should say so by filtering the NPCs that matter — the near tier — rather
//! than by making perception expensive for everyone. Distance is the cheap default
//! and it is enough to make a town feel inhabited.

use crate::adapters::primary::cognition::NpcBlackboard;
use crate::adapters::primary::components::Npc;
use crate::application::SimClock;
use crate::behavior::blackboard::Value;
use crate::domain::ids::NpcId;
use crate::domain::tick::Tick;
use bevy_ecs::prelude::*;
use bevy_transform::prelude::Transform;
use smallvec::SmallVec;

// The keys live in the behaviour layer, because the leaves that read them are core code and must not import an
// adapter to learn their own inputs. Re-exported here so `perception::KEY_PLAYER_SEEN` — the path this module has
// published since it was written — keeps resolving, and so the writer and the reader are documented in one place.
pub use crate::behavior::social::{KEY_PLAYER_DISTANCE, KEY_PLAYER_SEEN};

/// The most people one NPC remembers having noticed.
pub const NOTICED_CAPACITY: usize = 4;

/// Marks an entity as a player character.
///
/// The crate does not own player movement, input or rendering; it only needs to
/// know which entities are worth reacting to.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PlayerCharacter;

/// How far an NPC notices, and how long it remembers.
#[derive(Resource, Debug, Clone, Copy, PartialEq)]
pub struct PerceptionConfig {
    /// Distance at which a player character is noticed, in metres.
    pub notice_radius: f32,
    /// How long a noticed person stays in working memory.
    pub forget_after_ticks: u64,
}

impl Default for PerceptionConfig {
    fn default() -> Self {
        Self {
            notice_radius: 24.0,
            forget_after_ticks: 900,
        }
    }
}

/// Who an NPC has noticed lately.
///
/// A fixed-capacity list rather than a map: an NPC remembers the last few people
/// it bumped into, and the oldest entry is evicted when it meets another. That is
/// both cheaper and truer than an unbounded set — nobody remembers everyone.
#[derive(Component, Debug, Clone, Default, PartialEq, Eq)]
pub struct NoticedPeople {
    seen: SmallVec<[(NpcId, Tick); NOTICED_CAPACITY]>,
}

impl NoticedPeople {
    /// Note a person at a tick, refreshing their entry if it exists.
    pub fn note(&mut self, npc: NpcId, at: Tick) {
        if let Some(entry) = self.seen.iter_mut().find(|(seen, _)| *seen == npc) {
            entry.1 = at;
            return;
        }
        if self.seen.len() >= NOTICED_CAPACITY {
            // Evict the least recently seen, not the oldest-added: someone you spoke
            // to a moment ago is more present than someone you passed an hour ago.
            if let Some((index, _)) = self
                .seen
                .iter()
                .enumerate()
                .min_by_key(|(_, (_, tick))| tick.get())
            {
                self.seen.remove(index);
            }
        }
        self.seen.push((npc, at));
    }

    /// When a person was last seen, if they are still remembered.
    #[must_use]
    pub fn last_seen(&self, npc: NpcId) -> Option<Tick> {
        self.seen
            .iter()
            .find(|(seen, _)| *seen == npc)
            .map(|(_, tick)| *tick)
    }

    /// The most recently noticed person.
    #[must_use]
    pub fn most_recent(&self) -> Option<NpcId> {
        self.seen
            .iter()
            .max_by_key(|(_, tick)| tick.get())
            .map(|(npc, _)| *npc)
    }

    /// How many people are remembered.
    #[must_use]
    pub fn len(&self) -> usize {
        self.seen.len()
    }

    /// Whether nobody is remembered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }

    /// Drop anyone not seen within the configured window.
    pub fn forget(&mut self, now: Tick, after_ticks: u64) -> usize {
        let before = self.seen.len();
        self.seen.retain(|(_, tick)| now.since(*tick) < after_ticks);
        before - self.seen.len()
    }

    /// The remembered people, newest first.
    #[must_use = "iterators are lazy and do nothing unless consumed"]
    pub fn iter(&self) -> impl Iterator<Item = (NpcId, Tick)> + '_ {
        self.seen.iter().copied()
    }
}

/// Notice player characters within range, and write what was noticed into the
/// blackboard.
///
/// Runs in `FlockSet::Perception`, after the spatial index is rebuilt, so the
/// distance test uses this tick's positions rather than last tick's.
/// What `observe_players` reads and writes per NPC.
type ObserverTargets<'a> = (
    &'a Npc,
    &'a Transform,
    Option<&'a mut NoticedPeople>,
    Option<&'a mut NpcBlackboard>,
);

/// Notice player characters within range, and write what was noticed into the blackboard.
///
/// Runs in `FlockSet::Perception`, after the spatial index is rebuilt, so the distance test uses
/// this tick's positions rather than last tick's.
#[expect(
    clippy::needless_pass_by_value,
    reason = "Bevy hands a system its parameters by value; `&Res<T>` is not a `SystemParam`"
)]
pub fn observe_players(
    clock: Res<SimClock>,
    config: Res<PerceptionConfig>,
    players: Query<(&Npc, &Transform), With<PlayerCharacter>>,
    mut npcs: Query<ObserverTargets<'_>, Without<PlayerCharacter>>,
) {
    let radius = config.notice_radius.max(0.0);
    let radius_squared = radius * radius;
    for (npc, transform, noticed, blackboard) in &mut npcs {
        let mut nearest: Option<(NpcId, f32)> = None;
        for (player, player_transform) in &players {
            if player.0 == npc.0 {
                continue;
            }
            let delta = player_transform.translation - transform.translation;
            let distance_squared = delta.length_squared();
            if distance_squared > radius_squared {
                continue;
            }
            let distance = distance_squared.sqrt();
            if nearest.is_none_or(|(_, best)| distance < best) {
                nearest = Some((player.0, distance));
            }
        }
        if let Some((player, distance)) = nearest {
            if let Some(mut noticed) = noticed {
                noticed.note(player, clock.tick);
            }
            if let Some(mut slot) = blackboard {
                let _ = slot.0.set(KEY_PLAYER_SEEN, Value::Bool(true));
                let _ = slot.0.set(KEY_PLAYER_DISTANCE, Value::Float(distance));
            }
        } else if let Some(mut slot) = blackboard {
            let _ = slot.0.remove(KEY_PLAYER_SEEN);
            let _ = slot.0.remove(KEY_PLAYER_DISTANCE);
        }
    }
}

/// Forget people who have not been seen for long enough.
#[expect(
    clippy::needless_pass_by_value,
    reason = "Bevy hands a system its parameters by value; `&Res<T>` is not a `SystemParam`"
)]
pub fn forget_the_absent(
    clock: Res<SimClock>,
    config: Res<PerceptionConfig>,
    mut npcs: Query<&mut NoticedPeople>,
) {
    for mut noticed in &mut npcs {
        let _ = noticed.forget(clock.tick, config.forget_after_ticks);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::num;
    use bevy_app::App;
    use bevy_math::Vec3;
    fn app_with_players() -> App {
        // A bare app rather than the whole plugin: these tests are about perception,
        // and a plugin that spawns its own crowd would make every count ambiguous.
        let mut app = App::new();
        app.insert_resource(SimClock::default())
            .init_resource::<PerceptionConfig>()
            .add_systems(bevy_app::Update, (observe_players, forget_the_absent));
        app
    }

    fn spawn_at(app: &mut App, id: u32, position: Vec3, player: bool) {
        let mut entity = app.world_mut().spawn((
            Npc(NpcId::unique(id)),
            Transform::from_translation(position),
            NoticedPeople::default(),
            NpcBlackboard::default(),
        ));
        if player {
            entity.insert(PlayerCharacter);
        }
    }

    #[test]
    fn a_player_in_range_is_noticed_and_written_to_the_blackboard() {
        let mut app = app_with_players();
        spawn_at(&mut app, 1, Vec3::ZERO, false);
        spawn_at(&mut app, 2, Vec3::new(5.0, 0.0, 0.0), true);
        app.update();
        let noticed = app
            .world_mut()
            .query::<(&Npc, &NoticedPeople)>()
            .iter(app.world())
            .find(|(npc, _)| npc.0 == NpcId::unique(1))
            .map(|(_, noticed)| noticed.clone());
        assert_eq!(
            noticed.and_then(|noticed| noticed.last_seen(NpcId::unique(2))),
            Some(Tick::ZERO)
        );
        let seen = {
            let mut query = app.world_mut().query::<(&Npc, &NpcBlackboard)>();
            query
                .iter(app.world())
                .find(|(npc, _)| npc.0 == NpcId::unique(1))
                .and_then(|(_, board)| board.0.get(KEY_PLAYER_SEEN))
        };
        assert_eq!(seen, Some(Value::Bool(true)));
    }

    #[test]
    fn a_player_out_of_range_is_not_noticed() {
        let mut app = app_with_players();
        spawn_at(&mut app, 1, Vec3::ZERO, false);
        spawn_at(&mut app, 2, Vec3::new(200.0, 0.0, 0.0), true);
        app.update();
        let noticed = {
            let mut query = app.world_mut().query::<&NoticedPeople>();
            query.iter(app.world()).next().cloned()
        };
        assert!(noticed.is_none_or(|noticed| noticed.is_empty()));
    }

    #[test]
    fn npcs_do_not_notice_each_other_as_players() {
        let mut app = app_with_players();
        spawn_at(&mut app, 1, Vec3::ZERO, false);
        spawn_at(&mut app, 2, Vec3::new(1.0, 0.0, 0.0), false);
        app.update();
        for noticed in app.world_mut().query::<&NoticedPeople>().iter(app.world()) {
            assert!(noticed.is_empty(), "an NPC was treated as a player");
        }
    }

    #[test]
    fn the_nearest_player_wins_when_two_are_in_range() {
        let mut app = app_with_players();
        spawn_at(&mut app, 1, Vec3::ZERO, false);
        spawn_at(&mut app, 2, Vec3::new(10.0, 0.0, 0.0), true);
        spawn_at(&mut app, 3, Vec3::new(3.0, 0.0, 0.0), true);
        app.update();
        let distance = {
            let mut query = app.world_mut().query::<(&Npc, &NpcBlackboard)>();
            query
                .iter(app.world())
                .find(|(npc, _)| npc.0 == NpcId::unique(1))
                .and_then(|(_, board)| board.0.get(KEY_PLAYER_DISTANCE))
        };
        assert_eq!(distance, Some(Value::Float(3.0)));
    }

    #[test]
    fn memory_is_bounded_and_evicts_the_least_recent() {
        let mut noticed = NoticedPeople::default();
        for index in 0..(num::usize_to_u32(NOTICED_CAPACITY) + 3) {
            noticed.note(NpcId::unique(index), Tick::new(u64::from(index)));
        }
        assert_eq!(noticed.len(), NOTICED_CAPACITY);
        assert!(noticed.last_seen(NpcId::unique(0)).is_none());
        assert_eq!(
            noticed.most_recent(),
            Some(NpcId::unique(num::usize_to_u32(NOTICED_CAPACITY) + 2))
        );
    }

    #[test]
    fn noticing_again_refreshes_rather_than_duplicates() {
        let mut noticed = NoticedPeople::default();
        noticed.note(NpcId::unique(1), Tick::ZERO);
        noticed.note(NpcId::unique(1), Tick::new(50));
        assert_eq!(noticed.len(), 1);
        assert_eq!(noticed.last_seen(NpcId::unique(1)), Some(Tick::new(50)));
    }

    #[test]
    fn the_absent_are_forgotten() {
        let mut noticed = NoticedPeople::default();
        noticed.note(NpcId::unique(1), Tick::ZERO);
        assert_eq!(noticed.forget(Tick::new(100), 50), 1);
        assert!(noticed.is_empty());
    }
}
