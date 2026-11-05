//! Groups, formations and faction relations.
//!
//! A crowd is not a heap of independent agents: guards patrol in pairs, a
//! procession has a column, a squad has a leader and a queue of orders. Those are
//! relationships, and relationships in an ECS are the thing everyone gets wrong
//! once before settling on a design.
//!
//! The design here is a **plain index set**, not Bevy's relationship components,
//! for three reasons recorded in ADR 0035:
//!
//! 1. Group membership is read by directors during planning, not by a query over
//!    every NPC. A `GroupId` component plus this store answers "who is in group 7"
//!    in O(members); a relationship query answers "which entities point at group 7"
//!    by walking an archetype.
//! 2. Directives are ranged and consumed per member on its own think, so the store
//!    owns a queue rather than a component every member writes.
//! 3. Formations need stable slot numbers. A slot is an index into the group, and
//!    an index is something the store can hand out, free and reuse; it is not
//!    something a relationship can express.
//!
//! Nothing here iterates the population. Every operation is bounded by the group's
//! own size, which is what keeps a 10,000-member crowd from paying O(n) when a
//! directive is issued.

use std::collections::BTreeMap;
use bevy_math::{Vec3, Vec2};
use smallvec::SmallVec;
use crate::domain::ids::{GroupId, NpcId};
use crate::domain::tick::Tick;

/// The most factions a matrix can describe.
pub const MAX_FACTIONS: u8 = 16;

/// The deepest group nesting the model supports: a crowd containing squads.
///
/// A hard limit rather than an accident: two levels cover crowd → squad, and a
/// third would mean a director has to walk a tree on every decision, which is the
/// cost this design exists to avoid.
pub const MAX_NESTING_DEPTH: u8 = 2;

/// A faction identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct FactionId(u8);

impl FactionId {
    /// A faction identifier. Values at or above [`MAX_FACTIONS`] are refused by
    /// [`FactionMatrix::new`] rather than silently wrapping onto another faction.
    #[must_use]
    pub const fn new(raw: u8) -> Self {
        Self(raw)
    }

    /// The raw identifier.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

/// How two factions regard each other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Relation {
    /// They will come to each other's aid.
    Ally,
    /// They ignore each other.
    Neutral,
    /// They will fight on sight.
    Hostile,
    /// A standing feud: hostile, and it survives a truce elsewhere.
    Feud,
}

impl Relation {
    /// Whether this relation permits violence.
    #[must_use]
    pub const fn is_hostile(self) -> bool {
        matches!(self, Self::Hostile | Self::Feud)
    }

    /// A stable name for logs.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ally => "ally",
            Self::Neutral => "neutral",
            Self::Hostile => "hostile",
            Self::Feud => "feud",
        }
    }

    const fn code(self) -> u8 {
        match self {
            Self::Ally => 0,
            Self::Neutral => 1,
            Self::Hostile => 2,
            Self::Feud => 3,
        }
    }

    const fn from_code(code: u8) -> Self {
        match code {
            0 => Self::Ally,
            2 => Self::Hostile,
            3 => Self::Feud,
            _ => Self::Neutral,
        }
    }
}

/// A symmetric matrix of faction relations.
///
/// Symmetric by construction: setting A→B sets B→A, because an asymmetric matrix
/// is a table of interesting bugs ("they attack us, we ignore them") that no game
/// designer asked for.
#[derive(Debug, Clone)]
pub struct FactionMatrix {
    size: u8,
    cells: [u8; MAX_FACTIONS as usize * MAX_FACTIONS as usize],
}

impl FactionMatrix {
    /// A matrix for `size` factions, every pair neutral and every faction an ally
    /// of itself.
    ///
    /// # Errors
    ///
    /// Returns [`GroupError::TooManyFactions`] above [`MAX_FACTIONS`], and
    /// [`GroupError::NoFactions`] for zero.
    pub fn new(size: u8) -> Result<Self, GroupError> {
        if size == 0 {
            return Err(GroupError::NoFactions);
        }
        if size > MAX_FACTIONS {
            return Err(GroupError::TooManyFactions { size });
        }
        let mut cells = [Relation::Neutral.code(); MAX_FACTIONS as usize * MAX_FACTIONS as usize];
        for faction in 0..size {
            if let Some(slot) = cells.get_mut(Self::offset(faction, faction)) {
                *slot = Relation::Ally.code();
            }
        }
        Ok(Self { size, cells })
    }

    fn offset(a: u8, b: u8) -> usize {
        a as usize * MAX_FACTIONS as usize + b as usize
    }

    /// How many factions the matrix knows about.
    #[must_use]
    pub const fn size(&self) -> u8 {
        self.size
    }

    /// Set a relation, both ways.
    ///
    /// # Errors
    ///
    /// [`GroupError::UnknownFaction`] for an id the matrix was not built for.
    pub fn set(&mut self, a: FactionId, b: FactionId, relation: Relation) -> Result<(), GroupError> {
        if a.0 >= self.size {
            return Err(GroupError::UnknownFaction { faction: a.0, size: self.size });
        }
        if b.0 >= self.size {
            return Err(GroupError::UnknownFaction { faction: b.0, size: self.size });
        }
        let code = relation.code();
        for (x, y) in [(a.0, b.0), (b.0, a.0)] {
            if let Some(slot) = self.cells.get_mut(Self::offset(x, y)) {
                *slot = code;
            }
        }
        Ok(())
    }

    /// The relation between two factions.
    ///
    /// O(1): one array read, which is the point of a fixed-size matrix over a map.
    /// An id outside the matrix reads as [`Relation::Neutral`] rather than
    /// panicking, because a stray faction is a content bug, not a crash.
    #[must_use]
    pub fn get(&self, a: FactionId, b: FactionId) -> Relation {
        self.cells
            .get(Self::offset(a.0, b.0))
            .copied()
            .map_or(Relation::Neutral, Relation::from_code)
    }

    /// Bytes the matrix occupies.
    #[must_use]
    pub const fn bytes(&self) -> usize {
        core::mem::size_of::<Self>()
    }
}

impl Default for FactionMatrix {
    fn default() -> Self {
        Self::new(4).unwrap_or(Self {
            size: 4,
            cells: [Relation::Neutral.code(); MAX_FACTIONS as usize * MAX_FACTIONS as usize],
        })
    }
}

/// How a group stands in space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Formation {
    /// A circle around the leader, evenly spaced.
    Ring,
    /// A V behind the leader, alternating sides.
    Wedge,
    /// Single file behind the leader.
    Column,
    /// No slot: members keep whatever position they had.
    Free,
}

impl Formation {
    /// Every formation, in a fixed order.
    pub const ALL: [Self; 4] = [Self::Ring, Self::Wedge, Self::Column, Self::Free];

    /// A stable name for logs.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ring => "ring",
            Self::Wedge => "wedge",
            Self::Column => "column",
            Self::Free => "free",
        }
    }

    /// The offset of a slot from the leader, in the leader's local frame.
    ///
    /// `+z` is behind the leader, `+x` is their right, so a caller rotates this by
    /// the leader's yaw and adds their position. `None` for [`Formation::Free`] and
    /// for a slot beyond the formation's shape.
    #[must_use]
    pub fn slot_offset(self, slot: u16, count: u16, spacing: f32) -> Option<Vec3> {
        if count == 0 || slot >= count {
            return None;
        }
        let index = f32::from(slot);
        let total = f32::from(count);
        match self {
            Self::Free => None,
            Self::Column => Some(Vec3::new(0.0, 0.0, spacing * (index + 1.0))),
            Self::Wedge => {
                let rank = (index / 2.0).floor() + 1.0;
                let side = if u16::from(slot as u16) % 2 == 0 { 1.0 } else { -1.0 };
                Some(Vec3::new(side * rank * spacing * 0.75, 0.0, rank * spacing))
            }
            Self::Ring => {
                let angle = core::f32::consts::TAU * index / total;
                // Radius grows with the square root of the count so a bigger group is
                // not a denser ring: density stays roughly constant.
                let radius = spacing * (total * 0.5).sqrt().max(1.0);
                Some(Vec3::new(angle.cos() * radius, 0.0, angle.sin() * radius))
            }
        }
    }
}

/// An order a group carries, with the tick it was issued and its lifetime.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QueuedDirective {
    /// The tick the order was issued on.
    pub issued_tick: Tick,
    /// How many ticks it stands for.
    pub ttl_ticks: u64,
    /// The order.
    pub kind: crate::behavior::registry::DirectiveKind,
}

impl QueuedDirective {
    /// Whether the order still stands at `now`.
    #[must_use]
    pub const fn is_active(&self, now: Tick) -> bool {
        now.since(self.issued_tick) < self.ttl_ticks
    }
}

/// A group: a leader, its members, their slots, and whatever it has been told.
#[derive(Debug, Clone)]
pub struct Group {
    id: GroupId,
    leader: NpcId,
    formation: Formation,
    /// Slot occupancy. A slot is stable while the member stays: the index is the
    /// slot, which is what stops the ring reshuffling when someone leaves.
    slots: SmallVec<[Option<NpcId>; 8]>,
    directives: SmallVec<[QueuedDirective; 4]>,
    parent: Option<GroupId>,
    children: SmallVec<[GroupId; 4]>,
    faction: Option<FactionId>,
}

impl Group {
    /// The group's identity.
    #[must_use]
    pub const fn id(&self) -> GroupId {
        self.id
    }

    /// The leader.
    #[must_use]
    pub const fn leader(&self) -> NpcId {
        self.leader
    }

    /// The formation.
    #[must_use]
    pub const fn formation(&self) -> Formation {
        self.formation
    }

    /// Replace the formation. Slots keep their indices, so the group reshapes
    /// without reshuffling who is where in the queue.
    pub fn set_formation(&mut self, formation: Formation) {
        self.formation = formation;
    }

    /// How many members the group has, leader included.
    #[must_use]
    pub fn len(&self) -> usize {
        self.slots.iter().filter(|slot| slot.is_some()).count()
    }

    /// Whether the group has no members. A group is dissolved before it can reach
    /// this state through the public API; it exists so the type can be inspected
    /// safely.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.slots.iter().all(Option::is_none)
    }

    /// The members, in slot order.
    #[must_use]
    pub fn members(&self) -> impl Iterator<Item = NpcId> + '_ {
        self.slots.iter().filter_map(|slot| *slot)
    }

    /// The slot a member occupies.
    #[must_use]
    pub fn slot_of(&self, npc: NpcId) -> Option<u16> {
        self.slots
            .iter()
            .position(|slot| *slot == Some(npc))
            .and_then(|index| u16::try_from(index).ok())
    }

    /// The founding faction, if the group belongs to one.
    #[must_use]
    pub const fn faction(&self) -> Option<FactionId> {
        self.faction
    }

    /// The directives still standing at `now`.
    pub fn active_directives(&self, now: Tick) -> impl Iterator<Item = QueuedDirective> + '_ {
        self.directives.iter().copied().filter(move |d| d.is_active(now))
    }

    /// Whether the group is nested under another.
    #[must_use]
    pub const fn is_child(&self) -> bool {
        self.parent.is_some()
    }

    /// The parent group.
    #[must_use]
    pub const fn parent(&self) -> Option<GroupId> {
        self.parent
    }

    /// The child groups.
    #[must_use]
    pub fn children(&self) -> &[GroupId] {
        &self.children
    }
}

/// A store of groups, with the membership index that keeps operations O(group).
#[derive(Debug, Clone, Default)]
pub struct GroupStore {
    groups: Vec<Option<Group>>,
    free: Vec<usize>,
    member_index: BTreeMap<NpcId, GroupId>,
    next_id: u32,
}

impl GroupStore {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// How many live groups there are.
    #[must_use]
    pub fn len(&self) -> usize {
        self.groups.iter().filter(|group| group.is_some()).count()
    }

    /// Whether there are no live groups.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.groups.iter().all(Option::is_none)
    }

    /// A group by id.
    #[must_use]
    pub fn get(&self, id: GroupId) -> Option<&Group> {
        self.groups
            .get(id.index() as usize)
            .and_then(Option::as_ref)
            .filter(|group| group.id == id)
    }

    /// A group by id, mutably.
    pub fn get_mut(&mut self, id: GroupId) -> Option<&mut Group> {
        self.groups
            .get_mut(id.index() as usize)
            .and_then(Option::as_mut)
            .filter(|group| group.id == id)
    }

    /// The group a member belongs to.
    #[must_use]
    pub fn group_of(&self, npc: NpcId) -> Option<&Group> {
        self.member_index.get(&npc).and_then(|id| self.get(*id))
    }

    /// Create a group with a leader and no directive.
    ///
    /// # Errors
    ///
    /// [`GroupError::AlreadyGrouped`] if the leader already belongs to a group:
    /// a leader cannot lead two, and silently moving them would orphan the first.
    pub fn create(&mut self, leader: NpcId, formation: Formation) -> Result<GroupId, GroupError> {
        if self.member_index.contains_key(&leader) {
            return Err(GroupError::AlreadyGrouped { npc: leader });
        }
        let raw = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        let id = GroupId::new(raw);
        let group = Group {
            id,
            leader,
            formation,
            slots: smallvec::smallvec![Some(leader)],
            directives: SmallVec::new(),
            parent: None,
            children: SmallVec::new(),
            faction: None,
        };
        match self.free.pop() {
            Some(index) => {
                if let Some(slot) = self.groups.get_mut(index) {
                    *slot = Some(group);
                }
            }
            None => self.groups.push(Some(group)),
        }
        self.member_index.insert(leader, id);
        Ok(id)
    }

    /// Give a group a faction.
    ///
    /// # Errors
    ///
    /// [`GroupError::UnknownGroup`] for an id that is not live.
    pub fn set_faction(&mut self, id: GroupId, faction: FactionId) -> Result<(), GroupError> {
        let group = self.get_mut(id).ok_or(GroupError::UnknownGroup { group: id })?;
        group.faction = Some(faction);
        Ok(())
    }

    /// Add a member, returning the slot it was given.
    ///
    /// Reuses a freed slot before growing, so a member who leaves and rejoins lands
    /// where they were rather than at the end of the queue.
    ///
    /// # Errors
    ///
    /// [`GroupError::UnknownGroup`] or [`GroupError::AlreadyGrouped`].
    pub fn join(&mut self, id: GroupId, npc: NpcId) -> Result<u16, GroupError> {
        if let Some(existing) = self.member_index.get(&npc) {
            if *existing == id {
                return Err(GroupError::AlreadyGrouped { npc });
            }
            return Err(GroupError::AlreadyGrouped { npc });
        }
        let group = self.get_mut(id).ok_or(GroupError::UnknownGroup { group: id })?;
        let slot = group
            .slots
            .iter()
            .position(Option::is_none)
            .unwrap_or_else(|| group.slots.len());
        if slot == group.slots.len() {
            group.slots.push(Some(npc));
        } else if let Some(entry) = group.slots.get_mut(slot) {
            *entry = Some(npc);
        }
        self.member_index.insert(npc, id);
        u16::try_from(slot).map_err(|_| GroupError::GroupFull { group: id })
    }

    /// Remove a member, freeing its slot and dissolving the group if it was the
    /// last member.
    ///
    /// # Errors
    ///
    /// [`GroupError::UnknownGroup`] or [`GroupError::NotAMember`].
    pub fn leave(&mut self, id: GroupId, npc: NpcId) -> Result<u16, GroupError> {
        let group = self.get_mut(id).ok_or(GroupError::UnknownGroup { group: id })?;
        let Some(slot) = group.slots.iter().position(|entry| *entry == Some(npc)) else {
            return Err(GroupError::NotAMember { npc, group: id });
        };
        if group.len() == 1 {
            // The last member leaving dissolves the group rather than leaving an
            // empty husk the director might still try to command.
            self.dissolve(id);
            return u16::try_from(slot).map_err(|_| GroupError::GroupFull { group: id });
        }
        if group.leader == npc {
            // Promote deterministically: the lowest surviving identity leads, so a
            // replay promotes the same person as the live run.
            let next_leader = group.slots.iter().filter_map(|entry| *entry).min();
            if let Some(next) = next_leader {
                group.leader = next;
            }
        }
        if let Some(entry) = group.slots.get_mut(slot) {
            *entry = None;
        }
        self.member_index.remove(&npc);
        u16::try_from(slot).map_err(|_| GroupError::GroupFull { group: id })
    }

    /// Handle a despawn: leave every group the NPC was in.
    ///
    /// # Returns
    ///
    /// The group the NPC left, if any.
    pub fn on_despawn(&mut self, npc: NpcId) -> Option<GroupId> {
        let id = *self.member_index.get(&npc)?;
        let _ = self.leave(id, npc);
        Some(id)
    }

    /// Dissolve a group, detaching it from its parent.
    pub fn dissolve(&mut self, id: GroupId) {
        let Some(index) = self.groups.iter().position(|group| {
            group.as_ref().is_some_and(|group| group.id == id)
        }) else {
            return;
        };
        let Some(Some(group)) = self.groups.get(index) else {
            return;
        };
        let members: SmallVec<[NpcId; 8]> = group.members().collect();
        let parent = group.parent;
        for member in members {
            self.member_index.remove(&member);
        }
        if let Some(parent_id) = parent {
            if let Some(parent) = self.get_mut(parent_id) {
                parent.children.retain(|child| *child != id);
            }
        }
        if let Some(slot) = self.groups.get_mut(index) {
            *slot = None;
        }
        self.free.push(index);
    }

    /// Nest a group under another, one level deep at most.
    ///
    /// # Errors
    ///
    /// [`GroupError::UnknownGroup`], [`GroupError::AlreadyNested`] when the child
    /// already has a parent, and [`GroupError::NestingTooDeep`] when the parent is
    /// itself a child.
    pub fn nest(&mut self, child: GroupId, parent: GroupId) -> Result<(), GroupError> {
        let parent_is_child = self
            .get(parent)
            .ok_or(GroupError::UnknownGroup { group: parent })?
            .is_child();
        if parent_is_child {
            return Err(GroupError::NestingTooDeep { depth: MAX_NESTING_DEPTH });
        }
        let child_group = self
            .get_mut(child)
            .ok_or(GroupError::UnknownGroup { group: child })?;
        if child_group.is_child() {
            return Err(GroupError::AlreadyNested { group: child });
        }
        child_group.parent = Some(parent);
        if let Some(parent_group) = self.get_mut(parent) {
            parent_group.children.push(child);
        }
        Ok(())
    }

    /// Queue a directive on a group.
    ///
    /// # Errors
    ///
    /// [`GroupError::UnknownGroup`].
    pub fn issue(
        &mut self,
        id: GroupId,
        kind: crate::behavior::registry::DirectiveKind,
        tick: Tick,
        ttl_ticks: u64,
    ) -> Result<(), GroupError> {
        let group = self.get_mut(id).ok_or(GroupError::UnknownGroup { group: id })?;
        if group.directives.len() >= 4 {
            group.directives.remove(0);
        }
        group.directives.push(QueuedDirective {
            issued_tick: tick,
            ttl_ticks,
            kind,
        });
        Ok(())
    }

    /// Where a member should stand, given its leader's transform.
    ///
    /// # Errors
    ///
    /// [`GroupError::NotAMember`] if the NPC is not in a group.
    pub fn slot_target(
        &self,
        npc: NpcId,
        leader_position: Vec3,
        leader_yaw: f32,
        spacing: f32,
    ) -> Result<Option<Vec3>, GroupError> {
        let Some(id) = self.member_index.get(&npc).copied() else {
            return Err(GroupError::NotAMember {
                npc,
                group: GroupId::new(0),
            });
        };
        let Some(group) = self.get(id) else {
            return Ok(None);
        };
        let Some(slot) = group.slot_of(npc) else {
            return Ok(None);
        };
        let count = u16::try_from(group.len()).unwrap_or(u16::MAX);
        let Some(offset) = group.formation.slot_offset(slot, count, spacing) else {
            return Ok(None);
        };
        let (sin, cos) = (leader_yaw.sin(), leader_yaw.cos());
        // Rotate the local offset by the leader's yaw. `+z` behind the leader means
        // the offset is subtracted along the facing direction.
        let rotated = Vec3::new(
            offset.x * cos - offset.z * sin,
            0.0,
            offset.x * sin + offset.z * cos,
        );
        Ok(Some(leader_position + rotated))
    }

    /// Everything a diagnostics report wants to know about the store.
    #[must_use]
    pub fn snapshot(&self) -> GroupSnapshot {
        let mut members = 0;
        let mut nested = 0;
        let mut directed = 0;
        for group in self.groups.iter().flatten() {
            members += group.len();
            if group.is_child() {
                nested += 1;
            }
            if !group.directives.is_empty() {
                directed += 1;
            }
        }
        GroupSnapshot {
            groups: self.len(),
            members,
            nested,
            with_directives: directed,
        }
    }
}

/// A cheap summary of the group store for stats and diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GroupSnapshot {
    /// Live groups.
    pub groups: usize,
    /// Members across all groups.
    pub members: usize,
    /// Groups nested under another.
    pub nested: usize,
    /// Groups carrying at least one directive.
    pub with_directives: usize,
}

/// Why a group operation failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum GroupError {
    /// More factions than the matrix can hold.
    #[error("a faction matrix holds at most {MAX_FACTIONS} factions, {size} requested")]
    TooManyFactions {
        /// The requested size.
        size: u8,
    },
    /// A matrix with no factions.
    #[error("a faction matrix needs at least one faction")]
    NoFactions,
    /// A faction id outside the matrix.
    #[error("faction {faction} is outside a matrix of {size}")]
    UnknownFaction {
        /// The unknown id.
        faction: u8,
        /// The matrix size.
        size: u8,
    },
    /// A group id that is not live.
    #[error("group {group:?} is not live")]
    UnknownGroup {
        /// The id.
        group: GroupId,
    },
    /// An NPC already in a group.
    #[error("npc {npc:?} is already in a group")]
    AlreadyGrouped {
        /// The NPC.
        npc: NpcId,
    },
    /// An NPC not in the group it was removed from.
    #[error("npc {npc:?} is not in group {group:?}")]
    NotAMember {
        /// The NPC.
        npc: NpcId,
        /// The group.
        group: GroupId,
    },
    /// A group with more members than a slot index can address.
    #[error("group {group:?} has more members than a u16 slot index can address")]
    GroupFull {
        /// The group.
        group: GroupId,
    },
    /// A group that already has a parent.
    #[error("group {group:?} is already nested")]
    AlreadyNested {
        /// The group.
        group: GroupId,
    },
    /// Nesting deeper than the documented limit.
    #[error("groups nest at most {depth} levels deep")]
    NestingTooDeep {
        /// The limit.
        depth: u8,
    },
}

/// The planar distance between two slots of a formation, for tests and tuning.
#[must_use]
pub fn slot_spacing(formation: Formation, count: u16, spacing: f32) -> Option<f32> {
    let a = formation.slot_offset(0, count, spacing)?;
    let b = formation.slot_offset(1, count, spacing)?;
    let delta = Vec2::new(a.x - b.x, a.z - b.z);
    Some(delta.length())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::behavior::registry::DirectiveKind;

    fn npc(raw: u32) -> NpcId {
        NpcId::unique(raw)
    }

    #[test]
    fn faction_relations_are_symmetric_and_constant_time() {
        let mut matrix = FactionMatrix::new(4).expect("matrix");
        matrix
            .set(FactionId::new(1), FactionId::new(2), Relation::Hostile)
            .expect("set");
        assert_eq!(matrix.get(FactionId::new(2), FactionId::new(1)), Relation::Hostile);
        assert_eq!(matrix.get(FactionId::new(1), FactionId::new(1)), Relation::Ally);
        assert_eq!(matrix.get(FactionId::new(0), FactionId::new(3)), Relation::Neutral);
    }

    #[test]
    fn a_faction_outside_the_matrix_is_refused_on_write_and_neutral_on_read() {
        let mut matrix = FactionMatrix::new(2).expect("matrix");
        assert!(matches!(
            matrix.set(FactionId::new(2), FactionId::new(0), Relation::Ally).unwrap_err(),
            GroupError::UnknownFaction { .. }
        ));
        assert_eq!(matrix.get(FactionId::new(9), FactionId::new(0)), Relation::Neutral);
    }

    #[test]
    fn membership_operations_are_bounded_by_the_group() {
        let mut store = GroupStore::new();
        let group = store.create(npc(1), Formation::Column).expect("create");
        for index in 2..=64 {
            let slot = store.join(group, npc(index)).expect("join");
            assert_eq!(u32::from(slot), index - 1);
        }
        assert_eq!(store.get(group).map(Group::len), Some(64));
        // Leaving frees the slot for reuse rather than shifting everyone.
        let freed = store.leave(group, npc(5)).expect("leave");
        let reused = store.join(group, npc(99)).expect("join");
        assert_eq!(freed, reused);
        assert_eq!(store.group_of(npc(99)).map(Group::id), Some(group));
    }

    #[test]
    fn the_leader_cannot_lead_two_groups() {
        let mut store = GroupStore::new();
        let _ = store.create(npc(1), Formation::Ring).expect("create");
        assert!(matches!(
            store.create(npc(1), Formation::Ring).unwrap_err(),
            GroupError::AlreadyGrouped { .. }
        ));
    }

    #[test]
    fn despawning_the_leader_promotes_the_lowest_identity() {
        let mut store = GroupStore::new();
        let group = store.create(npc(7), Formation::Ring).expect("create");
        let _ = store.join(group, npc(3)).expect("join");
        let _ = store.join(group, npc(5)).expect("join");
        let _ = store.on_despawn(npc(7));
        assert_eq!(store.get(group).map(Group::leader), Some(npc(3)));
        assert_eq!(store.get(group).map(Group::len), Some(2));
    }

    #[test]
    fn the_last_member_leaving_dissolves_the_group() {
        let mut store = GroupStore::new();
        let group = store.create(npc(1), Formation::Ring).expect("create");
        let _ = store.on_despawn(npc(1));
        assert!(store.is_empty());
        assert!(store.get(group).is_none());
    }

    #[test]
    fn nesting_stops_at_two_levels() {
        let mut store = GroupStore::new();
        let crowd = store.create(npc(1), Formation::Free).expect("crowd");
        let squad = store.create(npc(2), Formation::Wedge).expect("squad");
        let other = store.create(npc(3), Formation::Ring).expect("other");
        store.nest(squad, crowd).expect("nest");
        assert_eq!(store.get(crowd).map(|g| g.children().len()), Some(1));
        assert!(matches!(
            store.nest(other, squad).unwrap_err(),
            GroupError::NestingTooDeep { .. }
        ));
        assert!(matches!(
            store.nest(squad, other).unwrap_err(),
            GroupError::AlreadyNested { .. }
        ));
    }

    #[test]
    fn a_dissolved_child_leaves_its_parent() {
        let mut store = GroupStore::new();
        let crowd = store.create(npc(1), Formation::Free).expect("crowd");
        let squad = store.create(npc(2), Formation::Wedge).expect("squad");
        store.nest(squad, crowd).expect("nest");
        store.dissolve(squad);
        assert_eq!(store.get(crowd).map(|g| g.children().len()), Some(0));
    }

    #[test]
    fn directives_expire() {
        let mut store = GroupStore::new();
        let group = store.create(npc(1), Formation::Ring).expect("create");
        store
            .issue(group, DirectiveKind::Hold, Tick::new(10), 5)
            .expect("issue");
        let group_ref = store.get(group).expect("live");
        assert_eq!(group_ref.active_directives(Tick::new(12)).count(), 1);
        assert_eq!(group_ref.active_directives(Tick::new(15)).count(), 0);
    }

    #[test]
    fn slot_indices_survive_a_member_leaving() {
        let mut store = GroupStore::new();
        let leader = npc(1);
        let group = store.create(leader, Formation::Ring).expect("create");
        let _ = store.join(group, npc(2)).expect("join");
        let _ = store.join(group, npc(3)).expect("join");
        assert_eq!(store.get(group).and_then(|g| g.slot_of(npc(2))), Some(1));
        // Removing a different member must not move this one's slot index: a ring
        // reshapes as the count changes, but a queue must not lose its place.
        let _ = store.leave(group, npc(3)).expect("leave");
        assert_eq!(store.get(group).and_then(|g| g.slot_of(npc(2))), Some(1));
        assert_eq!(store.get(group).map(Group::len), Some(2));
    }

    #[test]
    fn a_column_keeps_its_targets_as_the_group_shrinks() {
        let mut store = GroupStore::new();
        let group = store.create(npc(1), Formation::Column).expect("create");
        let _ = store.join(group, npc(2)).expect("join");
        let _ = store.join(group, npc(3)).expect("join");
        let anchor = Vec3::new(10.0, 0.0, 10.0);
        let first = store
            .slot_target(npc(2), anchor, 0.0, 1.5)
            .expect("member")
            .expect("slot");
        let _ = store.leave(group, npc(3)).expect("leave");
        let again = store
            .slot_target(npc(2), anchor, 0.0, 1.5)
            .expect("member")
            .expect("slot");
        assert!((first - again).length() < 1e-5, "a column should not reshuffle");
    }

    #[test]
    fn formation_offsets_are_behind_the_leader() {
        let column = Formation::Column.slot_offset(0, 4, 2.0).expect("offset");
        assert!(column.z > 0.0);
        let ring = Formation::Ring.slot_offset(0, 8, 2.0).expect("offset");
        assert!(ring.length() > 0.0);
        assert!(Formation::Free.slot_offset(0, 8, 2.0).is_none());
        assert!(Formation::Ring.slot_offset(9, 8, 2.0).is_none());
    }

    #[test]
    fn a_snapshot_counts_what_is_there() {
        let mut store = GroupStore::new();
        let a = store.create(npc(1), Formation::Ring).expect("a");
        let _ = store.join(a, npc(2)).expect("join");
        let b = store.create(npc(3), Formation::Column).expect("b");
        store.nest(b, a).expect("nest");
        store.issue(a, DirectiveKind::Hold, Tick::ZERO, 10).expect("issue");
        let snapshot = store.snapshot();
        assert_eq!(snapshot.groups, 2);
        assert_eq!(snapshot.members, 3);
        assert_eq!(snapshot.nested, 1);
        assert_eq!(snapshot.with_directives, 1);
    }

    #[test]
    fn the_matrix_reports_its_own_footprint() {
        let matrix = FactionMatrix::new(16).expect("matrix");
        assert_eq!(matrix.bytes(), core::mem::size_of::<FactionMatrix>());
        assert_eq!(matrix.size(), 16);
    }
}
