//! ADR 0019 under long-duration simulation: anchor exemption and reclamation.
//!
//! The unit tests in `domain::memory` check one sweep. This checks the thing a game
//! actually does — tens of thousands of ticks with NPCs arriving, being remembered, going
//! away, and coming back — and asks three questions of the store at the end:
//!
//! 1. Did absence leak? A store with room for sixteen rows should still have room after
//!    5,000 visitors, because the ones who faded were reclaimed.
//! 2. Did anchoring hold? The NPC the player wronged is still a row after an hour of game
//!    time, and its grudge is intact while its warmth is gone.
//! 3. Did reclamation stay bounded in time? The sweep is a cadence, not a per-frame cost,
//!    and the work it does must not grow with the number of NPCs who ever existed.

use bevy_flock::domain::ids::NpcId;
use bevy_flock::domain::memory::{
    AgingPolicy, DecayPolicy, Encounter, EncounterKind, MemoryStore, SubjectId,
};
use bevy_flock::domain::tick::Tick;
use bevy_flock::testkit::OrDie;

const CADENCE: u64 = 600;
const HALF_LIFE: u32 = 3_600;

fn decay() -> DecayPolicy {
    DecayPolicy {
        half_life_ticks: HALF_LIFE,
    }
}

#[test]
fn an_hour_of_absent_npcs_does_not_exhaust_the_store() {
    // Sixteen rows for a village of five thousand passers-by, an hour of game time apart.
    let mut store = MemoryStore::new(16, 4);
    let policy = AgingPolicy::every(CADENCE);
    let mut tick = 0_u64;
    let mut refusals = 0_u32;

    for visit in 0..5_000_u32 {
        let npc = NpcId::unique(visit);
        match store.enrol(npc) {
            Some(index) => {
                assert!(store.remember(
                    index,
                    SubjectId::new(0),
                    &Encounter::new(EncounterKind::Spoke, Tick::new(tick)),
                ));
            }
            None => refusals += 1,
        }
        // An hour of game time between visits: everything unanchored has faded.
        tick += 216_000;
        if tick.is_multiple_of(CADENCE) {
            store.age(Tick::new(tick), decay(), policy, &|_| false);
        }
        assert!(
            store.len() <= 16,
            "the store held {} rows at visit {visit}",
            store.len()
        );
    }

    assert_eq!(refusals, 0, "absence leaked rows until enrolment failed");
}

#[test]
fn an_anchored_grudge_outlives_an_hour_of_silence() {
    // The player attacks somebody, then leaves for an hour. Six hundred sweeps later the
    // NPC still knows exactly who they are — and feels nothing about it any more.
    let mut store = MemoryStore::new(8, 4);
    let wronged = NpcId::unique(42);
    let index = store.enrol(wronged).or_die();
    let player = SubjectId::new(0);
    store.remember(
        index,
        player,
        &Encounter::new(EncounterKind::Attacked, Tick::new(0)),
    );

    let policy = AgingPolicy::every(CADENCE);
    let keep = |owner: NpcId| owner == wronged;
    let mut retained = 0_u32;
    for sweep in 1..=600_u64 {
        let now = Tick::new(sweep * CADENCE);
        let report = store.age(now, decay(), policy, &keep);
        retained = report.retained;
        assert_eq!(report.freed, 0, "an anchor is never swept");
    }

    assert_eq!(retained, 1, "the anchor was found on every pass");
    let now = Tick::new(600 * CADENCE);
    assert_eq!(store.len(), 1);

    // Warmth: nothing left to act on.
    let decayed = store.recall_decayed(index, player, now, decay());
    assert!(
        decayed.is_none_or(|(valence, familiarity)| familiarity == 0 && valence == 0),
        "an hour of silence should leave no warmth: {decayed:?}"
    );

    // History: everything left to remember.
    let slot = store.recall(index, player).or_die();
    assert!(slot.is_grudge(), "and it is still a grudge");
    assert_eq!(slot.last_seen(), Tick::new(0), "the date did not drift");
    assert_eq!(slot.encounters(), 1, "the count is history");
}

#[test]
fn anchoring_one_npc_does_not_protect_its_neighbours() {
    // The exemption is per owner. A store where every row is kept because one is anchored
    // is a store that leaks again, with better intentions.
    let mut store = MemoryStore::new(16, 2);
    let anchored = NpcId::unique(1);
    let anchored_index = store.enrol(anchored).or_die();
    store.remember(
        anchored_index,
        SubjectId::new(0),
        &Encounter::new(EncounterKind::Attacked, Tick::new(0)),
    );

    let mut others = Vec::new();
    for id in 2..12 {
        let npc = NpcId::unique(id);
        let index = store.enrol(npc).or_die();
        store.remember(
            index,
            SubjectId::new(0),
            &Encounter::new(EncounterKind::Spoke, Tick::new(0)),
        );
        others.push((npc, index));
    }

    let now = Tick::new(216_000);
    let report = store.age(now, decay(), AgingPolicy::every(CADENCE), &|owner| {
        owner == anchored
    });

    assert_eq!(report.freed, 10, "ten unanchored rows were reclaimed");
    assert_eq!(report.retained, 1);
    assert_eq!(store.len(), 1, "only the anchor is left");
    for (_, index) in others {
        assert!(
            !store.is_enrolled(index),
            "a reclaimed index is not enrolled"
        );
    }
    assert!(store.is_enrolled(anchored_index), "the anchor is");
}

#[test]
fn reclamation_work_follows_the_store_not_the_population() {
    // The sweep visits rows, not NPCs who ever lived. Two stores that end with the same
    // occupancy must do the same work, whatever churn got them there.
    let policy = AgingPolicy::every(CADENCE);
    let mut quiet = MemoryStore::new(16, 2);
    for id in 0..4 {
        let index = quiet.enrol(NpcId::unique(id)).or_die();
        quiet.remember(
            index,
            SubjectId::new(0),
            &Encounter::new(EncounterKind::Spoke, Tick::new(0)),
        );
    }

    let mut churned = MemoryStore::new(16, 2);
    let mut tick = 0_u64;
    for visit in 0..2_000_u32 {
        if let Some(index) = churned.enrol(NpcId::unique(visit)) {
            churned.remember(
                index,
                SubjectId::new(0),
                &Encounter::new(EncounterKind::Spoke, Tick::new(tick)),
            );
        }
        tick += 216_000;
        if tick.is_multiple_of(CADENCE) {
            churned.age(Tick::new(tick), decay(), policy, &|_| false);
        }
    }

    // Both stores are near-empty; the churned one did not accumulate work to do.
    assert!(churned.len() <= 16);
    let quiet_report = quiet.age(Tick::new(10_800_000), decay(), policy, &|_| false);
    let churned_report = churned.age(Tick::new(10_800_000), decay(), policy, &|_| false);
    assert!(quiet_report.freed <= 4, "four rows at most to free");
    assert!(
        churned_report.freed <= 16,
        "the churned store should hold no backlog: freed {}",
        churned_report.freed
    );
}
