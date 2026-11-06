//! What a memory store is wasting (ADR 0015).
//!
//! Two failure modes are invisible in a running game and expensive once they are
//! large, because both consume the fixed capacity that makes social memory cheap:
//!
//! - **Ghost rows.** An NPC enrolled in the store who has met nobody. Games enrol on
//!   spawn and remember on encounter, so a wave of new arrivals is a wave of rows
//!   holding no relationships. No individual row is wrong; ten thousand of them is a
//!   store that cannot remember the people who matter.
//! - **Registry fragmentation.** Slots pointing at subjects the current registry no
//!   longer knows: retired characters, content cut in a patch, a mod uninstalled. A
//!   save file translates these (ADR 0013); a running store cannot, so it pays for
//!   them until they decay.
//!
//! This module is the measurement, not the drawing: [`measure`] reads a store and a
//! registry and returns integers, so a game can log it, alert on it, or plot it. The
//! `registry_telemetry` example draws the two graphs the numbers deserve.

use crate::domain::memory::{MemoryStore, SubjectId};
use crate::domain::saves::SubjectRegistry;

/// One moment's measurements of a store against a registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sample {
    /// Which session this was taken in, for plotting.
    pub session: usize,
    /// Rows enrolled in the store.
    pub enrolled: u32,
    /// Rows holding at least one relationship.
    pub populated: u32,
    /// Rows holding none: `enrolled - populated`.
    pub ghosts: u32,
    /// Relationships held, across every populated row.
    pub slots: u32,
    /// Relationships pointing at subjects the registry no longer knows.
    pub orphan_slots: u32,
    /// Subjects with at least one reference.
    pub live_subjects: u16,
    /// Subjects the registry knows about.
    pub registry_len: u16,
}

impl Sample {
    /// Ghost rows as a share of `capacity`, in permille, so it compares across
    /// store sizes without a float.
    #[must_use]
    pub const fn ghost_permille(self, capacity: u32) -> u32 {
        if capacity == 0 {
            return 0;
        }
        (self.ghosts * 1000) / capacity
    }

    /// Registered subjects that nothing references, in permille of the registry.
    #[must_use]
    pub fn orphan_subject_permille(self) -> u32 {
        if self.registry_len == 0 {
            return 0;
        }
        let orphans = self.registry_len.saturating_sub(self.live_subjects);
        (u32::from(orphans) * 1000) / u32::from(self.registry_len)
    }

    /// Slots no live subject can claim, in permille of all slots.
    #[must_use]
    pub const fn orphan_slot_permille(self) -> u32 {
        if self.slots == 0 {
            return 0;
        }
        (self.orphan_slots * 1000) / self.slots
    }

    /// Rows enrolled but empty, in permille of all enrolled rows.
    #[must_use]
    pub const fn ghost_row_permille(self) -> u32 {
        if self.enrolled == 0 {
            return 0;
        }
        (self.ghosts * 1000) / self.enrolled
    }
}

/// Measure a store against the registry the game currently holds.
///
/// Cost is `O(rows × subjects)`, the same walk the store already does for a sweep: a
/// diagnostic run once a minute, not once a frame.
#[must_use]
pub fn measure(store: &MemoryStore, registry: &SubjectRegistry, session: usize) -> Sample {
    let mut populated = 0_u32;
    let mut slots = 0_u32;
    let mut orphan_slots = 0_u32;
    let mut referenced = vec![false; usize::from(registry.len())];

    for (index, _) in store.enrolled() {
        let mut row_slots = 0_u32;
        for raw in 0..store.subjects() {
            if store.recall(index, SubjectId::new(raw)).is_none() {
                continue;
            }
            row_slots += 1;
            match referenced.get_mut(usize::from(raw)) {
                Some(flag) => *flag = true,
                None => orphan_slots += 1,
            }
        }
        if row_slots > 0 {
            populated += 1;
            slots += row_slots;
        }
    }

    let live_subjects =
        u16::try_from(referenced.iter().filter(|flag| **flag).count()).unwrap_or(u16::MAX);

    Sample {
        session,
        enrolled: store.len(),
        populated,
        ghosts: store.len() - populated,
        slots,
        orphan_slots,
        live_subjects,
        registry_len: registry.len(),
    }
}

/// How many rows reference one subject.
#[must_use]
pub fn references_to(store: &MemoryStore, subject: u16) -> u32 {
    let mut count = 0;
    for (index, _) in store.enrolled() {
        if store.recall(index, SubjectId::new(subject)).is_some() {
            count += 1;
        }
    }
    count
}

/// A registry without its last `count` subjects, as a content patch leaves it.
///
/// The later identifiers shift, which is exactly the situation ADR 0013's keys exist
/// to survive and exactly what leaves a running store holding slots nothing can claim.
#[must_use]
pub fn retire(registry: &SubjectRegistry, count: u16) -> SubjectRegistry {
    let keep = registry.len().saturating_sub(count);
    registry
        .entries()
        .iter()
        .take(usize::from(keep))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::ids::NpcId;
    use crate::domain::memory::{Encounter, EncounterKind};
    use crate::domain::saves::{SubjectEntry, SubjectKey, SubjectKind};
    use crate::domain::tick::Tick;
    use crate::testkit::OrDie;

    fn registry(len: u16) -> SubjectRegistry {
        (0..len)
            .map(|index| SubjectEntry::new(SubjectKey::new(u64::from(index)), SubjectKind::Npc))
            .collect()
    }

    #[test]
    fn ghosts_are_rows_that_hold_nothing() {
        let mut store = MemoryStore::new(1_000, 8);
        let registry = registry(8);
        for npc in 0..100_u32 {
            store.enrol(NpcId::unique(npc)).or_die();
        }
        let index = store.enrolled().next().or_die().0;
        store.remember(
            index,
            SubjectId::new(0),
            &Encounter::new(EncounterKind::Spoke, Tick::new(1)),
        );

        let sample = measure(&store, &registry, 0);
        assert_eq!(sample.enrolled, 100);
        assert_eq!(sample.populated, 1);
        assert_eq!(sample.ghosts, 99);
        assert_eq!(sample.slots, 1);
        assert_eq!(sample.orphan_slots, 0);
        assert_eq!(sample.live_subjects, 1);
        assert_eq!(sample.orphan_subject_permille(), 875, "7 of 8 unreferenced");
        assert_eq!(sample.ghost_row_permille(), 990);
        assert_eq!(sample.ghost_permille(1_000), 99);
    }

    #[test]
    fn a_retired_subject_leaves_slots_nothing_can_claim() {
        let mut store = MemoryStore::new(100, 8);
        let full = registry(8);
        for npc in 0..10_u32 {
            let index = store.enrol(NpcId::unique(npc)).or_die();
            store.remember(
                index,
                SubjectId::new(7),
                &Encounter::new(EncounterKind::Robbed, Tick::new(5)),
            );
        }
        let before = measure(&store, &full, 0);
        assert_eq!(before.live_subjects, 1);
        assert_eq!(before.orphan_slots, 0);

        let shrunk = retire(&full, 2);
        assert_eq!(shrunk.len(), 6, "the patch cut two characters");
        let after = measure(&store, &shrunk, 1);
        assert_eq!(
            after.orphan_slots, 10,
            "the references outlive the subjects"
        );
        assert_eq!(after.orphan_slot_permille(), 1_000);
        assert_eq!(
            after.live_subjects, 0,
            "subject 7 is outside the registry now"
        );
        assert_eq!(after.slots, 10, "and the store still counts them");
    }

    #[test]
    fn references_are_counted_per_subject() {
        let mut store = MemoryStore::new(10, 4);
        let registry = registry(4);
        for npc in 0..3_u32 {
            let index = store.enrol(NpcId::unique(npc)).or_die();
            store.remember(
                index,
                SubjectId::new(1),
                &Encounter::new(EncounterKind::Helped, Tick::new(1)),
            );
        }
        assert_eq!(references_to(&store, 1), 3);
        assert_eq!(references_to(&store, 2), 0);
        let sample = measure(&store, &registry, 0);
        assert_eq!(sample.live_subjects, 1);
        assert_eq!(sample.orphan_subject_permille(), 750);
    }

    #[test]
    fn an_empty_store_measures_as_zero_rather_than_dividing_by_it() {
        let store = MemoryStore::new(100, 4);
        let sample = measure(&store, &registry(4), 0);
        assert_eq!(sample.enrolled, 0);
        assert_eq!(sample.ghost_row_permille(), 0);
        assert_eq!(sample.orphan_slot_permille(), 0);
        assert_eq!(sample.ghost_permille(100), 0);
        let sample = measure(&store, &registry(0), 0);
        assert_eq!(sample.orphan_subject_permille(), 0);
    }
}
