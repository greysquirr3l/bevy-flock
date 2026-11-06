//! Group membership on entities, and the reconciliation that keeps it honest.
//!
//! The group model lives in the domain ([`crate::domain::groups`]); this module is
//! the ECS half: two components that say which group an entity belongs to, a
//! resource holding the store, and the system that notices when an entity is gone.
//!
//! Reconciliation is driven by `RemovedComponents`, not by scanning the crowd. A
//! system that walked every NPC each tick to find dead members would cost O(n) for
//! an event that happens a handful of times a second, and the whole point of the
//! index set in the domain model is that group work is O(group). The one thing the
//! index cannot know on its own is which entity was which, so an [`NpcIndex`] maps
//! entities to identities and outlives the entity just long enough for the removal
//! to be attributed.

use crate::domain::groups::{Formation, GroupError, GroupStore};
use crate::domain::ids::{GroupId, NpcId};
use bevy_ecs::prelude::*;
use std::collections::BTreeMap;

/// The group this entity belongs to.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GroupMember(pub GroupId);

/// Marks the leader of a group.
///
/// Kept separate from [`GroupMember`] because a leader is also a member: the marker
/// answers "whose orders do the others follow", not "is this person in the group".
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GroupLeader(pub GroupId);

/// The crowd's groups.
#[derive(Resource, Debug, Default)]
pub struct Groups(pub GroupStore);

impl Groups {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The underlying store.
    #[must_use]
    pub const fn store(&self) -> &GroupStore {
        &self.0
    }

    /// The underlying store, mutably.
    pub const fn store_mut(&mut self) -> &mut GroupStore {
        &mut self.0
    }

    /// Form a group around a leader.
    ///
    /// # Errors
    ///
    /// [`GroupError::AlreadyGrouped`] if the leader already leads or belongs to one.
    pub fn create(&mut self, leader: NpcId, formation: Formation) -> Result<GroupId, GroupError> {
        self.0.create(leader, formation)
    }

    /// Add a member.
    ///
    /// # Errors
    ///
    /// As [`GroupStore::join`].
    pub fn join(&mut self, group: GroupId, npc: NpcId) -> Result<u16, GroupError> {
        self.0.join(group, npc)
    }

    /// The group an entity's identity belongs to.
    #[must_use]
    pub fn group_of(&self, npc: NpcId) -> Option<GroupId> {
        self.0.group_of(npc).map(crate::domain::groups::Group::id)
    }
}

/// Entity to identity, for attributing removals after the entity is gone.
///
/// Insertion happens once per spawn and removal happens once per despawn, so both
/// are O(log n) and neither grows with the crowd over time.
#[derive(Resource, Debug, Default)]
pub struct NpcIndex {
    by_entity: BTreeMap<Entity, NpcId>,
}

impl NpcIndex {
    /// Record that an entity is an NPC.
    pub fn insert(&mut self, entity: Entity, npc: NpcId) {
        self.by_entity.insert(entity, npc);
    }

    /// The identity behind an entity.
    #[must_use]
    pub fn get(&self, entity: Entity) -> Option<NpcId> {
        self.by_entity.get(&entity).copied()
    }

    /// Forget an entity.
    pub fn remove(&mut self, entity: Entity) -> Option<NpcId> {
        self.by_entity.remove(&entity)
    }

    /// How many entities are indexed.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_entity.len()
    }

    /// Whether the index is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_entity.is_empty()
    }
}

/// Record newly spawned NPCs in the index.
pub fn index_new_npcs(
    mut index: ResMut<NpcIndex>,
    npcs: Query<
        (Entity, &crate::adapters::primary::components::Npc),
        Added<crate::adapters::primary::components::Npc>,
    >,
) {
    for (entity, npc) in &npcs {
        index.insert(entity, npc.0);
    }
}

/// Remove despawning NPCs from their groups.
///
/// Runs after the spawn/drain set so an entity that was spawned and despawned in
/// the same frame is handled once.
pub fn reconcile_despawns(
    mut removed: RemovedComponents<GroupMember>,
    mut index: ResMut<NpcIndex>,
    mut groups: ResMut<Groups>,
) {
    for entity in removed.read() {
        let Some(npc) = index.remove(entity) else {
            continue;
        };
        let _ = groups.0.on_despawn(npc);
    }
}

/// Place members at their formation slots.
///
/// A no-op for a group whose formation is [`Formation::Free`], and for an NPC with
/// no leader transform to read: the system writes a target and lets steering do the
/// walking, because teleporting a member into formation is how a crowd starts
/// looking like a puppet show.
#[expect(
    clippy::needless_pass_by_value,
    reason = "Bevy hands a system its parameters by value; `&Res<T>` is not a `SystemParam`"
)]
pub fn assign_formation_slots(
    groups: Res<Groups>,
    leaders: Query<(&GroupLeader, &bevy_transform::prelude::Transform)>,
    mut members: Query<(
        &GroupMember,
        &crate::adapters::primary::components::Npc,
        &mut crate::adapters::primary::components::NpcTarget,
    )>,
) {
    if groups.0.is_empty() {
        return;
    }
    for (member, npc, mut target) in &mut members {
        let Some(group) = groups.0.get(member.0) else {
            continue;
        };
        if group.formation() == Formation::Free {
            continue;
        }
        let Some(leader) = groups
            .0
            .get(member.0)
            .map(crate::domain::groups::Group::leader)
        else {
            continue;
        };
        if leader == npc.0 {
            continue;
        }
        let Some((_, transform)) = leaders.iter().find(|(marker, _)| marker.0 == member.0) else {
            continue;
        };
        let yaw = transform.rotation.to_euler(bevy_math::EulerRot::YXZ).0;
        if let Ok(Some(slot)) = groups.0.slot_target(npc.0, transform.translation, yaw, 1.6) {
            target.0 = slot;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::primary::components::Npc;
    use crate::adapters::primary::plugin::{FlockConfig, FlockPlugin};
    use crate::testkit::OrDie;
    use bevy_app::{App, Update};

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins(FlockPlugin::new(FlockConfig::default()));
        app.init_resource::<Groups>().init_resource::<NpcIndex>();
        app.add_systems(Update, (index_new_npcs, reconcile_despawns).chain());
        app
    }

    #[test]
    fn a_despawn_frees_the_membership() {
        let mut app = app();
        let leader = NpcId::unique(1);
        let member = NpcId::unique(2);
        let group = app
            .world_mut()
            .resource_mut::<Groups>()
            .create(leader, Formation::Ring)
            .or_die();
        app.world_mut()
            .resource_mut::<Groups>()
            .join(group, member)
            .or_die();

        let leader_entity = app
            .world_mut()
            .spawn((Npc(leader), GroupMember(group), GroupLeader(group)))
            .id();
        let member_entity = app
            .world_mut()
            .spawn((Npc(member), GroupMember(group)))
            .id();
        app.update();
        assert!(
            app.world()
                .resource::<NpcIndex>()
                .get(member_entity)
                .is_some()
        );

        app.world_mut().despawn(member_entity);
        app.update();
        let store = app.world().resource::<Groups>();
        assert!(store.0.group_of(member).is_none());
        assert_eq!(
            store.0.get(group).map(crate::domain::groups::Group::len),
            Some(1)
        );
        assert!(
            app.world()
                .resource::<NpcIndex>()
                .get(member_entity)
                .is_none()
        );
        let _ = leader_entity;
    }

    #[test]
    fn despawning_the_leader_promotes_the_lowest_identity() {
        let mut app = app();
        let leader = NpcId::unique(9);
        let member = NpcId::unique(4);
        let group = app
            .world_mut()
            .resource_mut::<Groups>()
            .create(leader, Formation::Ring)
            .or_die();
        app.world_mut()
            .resource_mut::<Groups>()
            .join(group, member)
            .or_die();
        let leader_entity = app
            .world_mut()
            .spawn((Npc(leader), GroupMember(group), GroupLeader(group)))
            .id();
        let _ = app
            .world_mut()
            .spawn((Npc(member), GroupMember(group)))
            .id();
        app.update();
        app.world_mut().despawn(leader_entity);
        app.update();
        let store = app.world().resource::<Groups>();
        assert_eq!(
            store.0.get(group).map(crate::domain::groups::Group::leader),
            Some(member)
        );
    }

    #[test]
    fn a_free_formation_writes_no_target() {
        let mut app = app();
        let leader = NpcId::unique(1);
        let member = NpcId::unique(2);
        let group = app
            .world_mut()
            .resource_mut::<Groups>()
            .create(leader, Formation::Free)
            .or_die();
        app.world_mut()
            .resource_mut::<Groups>()
            .join(group, member)
            .or_die();
        app.add_systems(Update, assign_formation_slots);
        let _leader_entity = app
            .world_mut()
            .spawn((
                Npc(leader),
                GroupMember(group),
                GroupLeader(group),
                bevy_transform::prelude::Transform::from_xyz(5.0, 0.0, 5.0),
            ))
            .id();
        let member_entity = app
            .world_mut()
            .spawn((
                Npc(member),
                GroupMember(group),
                crate::adapters::primary::components::NpcTarget(bevy_math::Vec3::new(
                    1.0, 0.0, 1.0,
                )),
            ))
            .id();
        app.update();
        let target = app
            .world()
            .get::<crate::adapters::primary::components::NpcTarget>(member_entity)
            .copied();
        assert_eq!(
            target,
            Some(crate::adapters::primary::components::NpcTarget(
                bevy_math::Vec3::new(1.0, 0.0, 1.0)
            ))
        );
    }

    #[test]
    fn an_unknown_group_is_survivable() {
        let mut app = app();
        let entity = app
            .world_mut()
            .spawn((
                Npc(NpcId::unique(3)),
                GroupMember(GroupId::new(99)),
                crate::adapters::primary::components::NpcTarget(bevy_math::Vec3::ZERO),
            ))
            .id();
        app.update();
        assert!(app.world().get::<GroupMember>(entity).is_some());
    }
}
