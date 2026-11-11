//! Slots: the index each NPC owns in the spatial grid's entry array.
//!
//! The grid's incremental rebuild remembers one cell per *entry index* and compares it with the cell
//! the entry at that index now belongs to, so the entry array has to be the same array from tick to
//! tick: the same length, and the same body at the same index. A query rebuilt from scratch every
//! tick cannot promise that. The order a query yields is the archetype order, and the archetype
//! order changes when a body is added or removed, so a body that changed archetype would look to
//! the patch like a body that crossed a cell boundary.
//!
//! A slot is that promise made explicit. The slot is the index, the component carries it, and the
//! table says which body owns which slot — including the bookkeeping a despawn needs, because a
//! despawned body has no component left to ask.

use std::collections::HashMap;

use bevy_ecs::prelude::*;

/// The index an NPC owns in the spatial grid's entry array.
///
/// The grid writes each body into its slot, so the entry at an index is the same body every tick
/// and the patch can compare the cell it was in with the cell it is in now. A body keeps its slot
/// until the table moves it.
///
/// ```
/// use bevy_flock_ecs::adapters::primary::grid_slots::NpcGridSlot;
///
/// let slot = NpcGridSlot(7);
/// assert_eq!(slot.index(), 7);
/// ```
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NpcGridSlot(pub u32);

impl NpcGridSlot {
    /// The index this body owns in the grid's entry array.
    #[must_use]
    pub const fn index(self) -> u32 {
        self.0
    }
}

/// A body that moved to another slot.
///
/// The caller has to be told: the body owns a different index now, and anything that cached the
/// old one is wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Moved {
    /// The body that moved.
    pub entity: Entity,
    /// The slot it owned before.
    pub from: u32,
    /// The slot it owns now.
    pub to: u32,
}

/// Which body owns which slot.
///
/// The table hands out slots, takes them back when a body is despawned, and keeps the live slots
/// dense so the grid's entry array can be indexed by slot: the live slots are always `0..n`, with
/// no gap in the middle. A gap would leave the grid's entry array with a body at an index that no
/// body owns, and the index table is the grid's own model of the population — it has no way to
/// describe a hole.
///
/// ```
/// use bevy_app::App;
/// use bevy_ecs::prelude::*;
/// use bevy_flock_ecs::adapters::primary::grid_slots::GridSlots;
///
/// let mut app = App::new();
/// app.init_resource::<GridSlots>();
/// let entity = app.world_mut().spawn_empty().id();
/// let slot = app.world_mut().resource_mut::<GridSlots>().allocate(entity);
/// assert_eq!(slot, 0);
/// assert_eq!(app.world().resource::<GridSlots>().owner(0), Some(entity));
/// ```
#[derive(Resource, Debug)]
pub struct GridSlots {
    /// Slot to body. A freed slot holds `None` until it is handed out again.
    slots: Vec<Option<Entity>>,
    /// Body to slot. A despawned body has no component left to ask, so the table keeps its own.
    owners: HashMap<Entity, u32>,
    /// Freed slots, reused before the table grows.
    free: Vec<u32>,
    /// How many slots are occupied.
    live: usize,
    /// Whether the occupied slots are exactly `0..live`.
    dense: bool,
}

impl Default for GridSlots {
    fn default() -> Self {
        Self {
            slots: Vec::new(),
            owners: HashMap::new(),
            free: Vec::new(),
            live: 0,
            // An empty table has no gap in it: the live range is 0..0, and the first slot handed
            // out continues that range. A table that started as "not dense" would stay that way
            // for ever, because the flag only ever clears itself.
            dense: true,
        }
    }
}

impl GridSlots {
    /// The number of slots handed out, freed ones included.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.slots.len()
    }

    /// Whether no slot has been handed out.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// How many bodies own a slot.
    #[must_use]
    pub const fn live(&self) -> usize {
        self.live
    }

    /// Whether the occupied slots are exactly `0..live`, with no gap in the middle.
    ///
    /// The grid's entry array is indexed by slot, and it has no way to describe a hole, so the
    /// slots have to be dense for the index to mean what it says.
    #[must_use]
    pub const fn dense(&self) -> bool {
        self.dense
    }

    /// The body that owns a slot.
    #[must_use]
    pub fn owner(&self, slot: u32) -> Option<Entity> {
        self.slots.get(slot as usize).copied().flatten()
    }

    /// The slot a body owns.
    #[must_use]
    pub fn slot_of(&self, entity: Entity) -> Option<u32> {
        self.owners.get(&entity).copied()
    }

    /// Give a body a slot, reusing a freed one before the table grows.
    ///
    /// The slot is not necessarily the next index: a freed slot is reused first, so a crowd that
    /// churns does not make the table grow for ever. A reused slot can leave a gap, which
    /// [`GridSlots::compact`] closes.
    pub fn allocate(&mut self, entity: Entity) -> u32 {
        let slot = if let Some(freed) = self.free.pop() {
            freed
        } else {
            let next = u32::try_from(self.slots.len()).unwrap_or(u32::MAX);
            self.slots.push(None);
            next
        };
        // The dense range only grows when the slot that was handed out continues it.
        self.dense = self.dense && slot == u32::try_from(self.live).unwrap_or(u32::MAX);
        self.live = self.live.saturating_add(1);
        if let Some(entry) = self.slots.get_mut(slot as usize) {
            *entry = Some(entity);
        }
        self.owners.insert(entity, slot);
        slot
    }

    /// Take a body's slot back, and hand the slot out again later.
    ///
    /// The slot is not reused immediately: the live slots have to stay dense, and a gap in the
    /// middle is closed by [`GridSlots::compact`].
    pub fn release(&mut self, entity: Entity) -> Option<u32> {
        let slot = self.owners.remove(&entity)?;
        if let Some(entry) = self.slots.get_mut(slot as usize) {
            *entry = None;
        }
        // Only the highest slot leaves the dense range intact.
        self.dense =
            self.dense && slot == u32::try_from(self.live.saturating_sub(1)).unwrap_or(u32::MAX);
        self.live = self.live.saturating_sub(1);
        self.free.push(slot);
        Some(slot)
    }

    /// Close a gap in the live slots, and report the body that moved.
    ///
    /// The live slots are `0..n`. A slot freed in the middle leaves a gap, and the grid's entry
    /// array cannot describe one, so the body in the highest live slot moves down into the gap and
    /// the slot it left is freed. One body moves, whatever the size of the crowd.
    ///
    /// Returns `None` when the slots are already dense, which is the case on every tick but the
    /// one after a despawn.
    pub fn compact(&mut self) -> Option<Moved> {
        if self.dense {
            return None;
        }
        let mut highest = None;
        for (index, slot) in self.slots.iter().enumerate() {
            if slot.is_some() {
                highest = Some(u32::try_from(index).unwrap_or(u32::MAX));
            }
        }
        if self.live == 0 {
            self.dense = true;
            return None;
        }
        let mut gap = None;
        for index in 0..self.live {
            let occupied = self
                .slots
                .get(index)
                .and_then(|slot| slot.as_ref())
                .is_some();
            if !occupied {
                gap = u32::try_from(index).ok();
                break;
            }
        }
        let gap = gap?;
        let highest = highest?;
        if highest <= gap {
            return None;
        }
        let moved_entity = self.owner(highest)?;
        // The body in the highest slot moves into the gap; the slot it leaves is free again.
        if let Some(entry) = self.slots.get_mut(gap as usize) {
            *entry = Some(moved_entity);
        }
        if let Some(entry) = self.slots.get_mut(highest as usize) {
            *entry = None;
        }
        self.owners.insert(moved_entity, gap);
        self.free.push(highest);
        self.dense = true;
        Some(Moved {
            entity: moved_entity,
            from: highest,
            to: gap,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::OrDie;

    fn entity(world: &mut World) -> Entity {
        world.spawn_empty().id()
    }

    #[test]
    fn a_table_starts_dense_and_stays_dense_while_bodies_come_in_order() {
        let mut world = World::new();
        let mut slots = GridSlots::default();
        assert!(slots.is_empty());
        assert!(slots.dense(), "an empty table has no gap in it");
        for _ in 0..3 {
            slots.allocate(entity(&mut world));
            assert!(
                slots.dense(),
                "a body in the next slot keeps the range whole"
            );
        }
    }

    #[test]
    fn a_slot_is_handed_out_once_and_released_back() {
        let mut world = World::new();
        let first = entity(&mut world);
        let second = entity(&mut world);
        let mut slots = GridSlots::default();
        assert_eq!(slots.allocate(first), 0);
        assert_eq!(slots.allocate(second), 1);
        assert_eq!(slots.owner(1), Some(second));
        assert_eq!(slots.release(first), Some(0));
        assert_eq!(slots.owner(0), None);
        assert_eq!(slots.slot_of(first), None);
    }

    #[test]
    fn a_freed_slot_is_handed_out_again() {
        let mut world = World::new();
        let first = entity(&mut world);
        let second = entity(&mut world);
        let mut slots = GridSlots::default();
        slots.allocate(first);
        slots.allocate(second);
        slots.release(first).or_die();
        assert_eq!(slots.allocate(second), 0);
        assert_eq!(slots.len(), 2);
    }

    #[test]
    fn a_gap_is_closed_by_moving_the_highest_body_down() {
        let mut world = World::new();
        let bodies: Vec<Entity> = (0..4).map(|_| entity(&mut world)).collect();
        let body = |index: usize| bodies.get(index).copied().or_die();
        let mut slots = GridSlots::default();
        for body in &bodies {
            slots.allocate(*body);
        }
        slots.release(body(1)).or_die();
        let moved = slots.compact().or_die();
        assert_eq!(moved.entity, body(3));
        assert_eq!(moved.from, 3);
        assert_eq!(moved.to, 1);
        // The live slots are dense again: 0, 1, 2.
        assert_eq!(slots.owner(0), Some(body(0)));
        assert_eq!(slots.owner(1), Some(body(3)));
        assert_eq!(slots.owner(2), Some(body(2)));
        assert_eq!(slots.owner(3), None);
        assert_eq!(slots.compact(), None);
    }

    #[test]
    fn a_dense_table_needs_no_compaction() {
        let mut world = World::new();
        let mut slots = GridSlots::default();
        for _ in 0..3 {
            slots.allocate(entity(&mut world));
        }
        assert_eq!(slots.compact(), None);
    }
}
