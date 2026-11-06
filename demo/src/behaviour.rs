//! What the crowd's personalities did, measured per archetype.
//!
//! The library's digest says the simulation repeated and the tier histogram says the budget spread
//! the thinking. Neither says whether a guard behaves like a guard. This reads the finished world
//! back and answers that with two kinds of number: what a personality asked for (its mean steering
//! bias) and what the crowd did with it (mean speed and how tightly it holds together).
//!
//! The bias column is the causal one. Speed and spacing are effects — a crowd in a street is
//! constrained by the street — so they are reported, not asserted on. The bias is a pure function
//! of the sampled temperament, which makes it the number a test can hold still.

use crate::json;
use bevy_ecs::prelude::*;
use bevy_flock::adapters::primary::components::{
    Npc, NpcIdentity, NpcNeeds, NpcTemperament, NpcVelocity,
};
use bevy_flock::domain::temperament::{Archetype, TemperamentDrive};
use bevy_transform::prelude::Transform;
use std::collections::BTreeMap;

/// One archetype's behaviour, averaged over the crowd.
#[derive(Debug, Clone, PartialEq)]
pub struct ArchetypeBehaviour {
    /// Which archetype.
    pub archetype: Archetype,
    /// How many were measured.
    pub count: u32,
    /// Mean speed, metres per second.
    pub speed: f32,
    /// Mean distance to the nearest neighbour, metres: how loosely the group is packed.
    pub spacing: f32,
    /// Mean separation multiplier the personality asked for.
    pub separation: f32,
    /// Mean alignment multiplier the personality asked for: how strongly the group's heading pulls.
    pub alignment: f32,
    /// Mean cohesion multiplier the personality asked for.
    pub cohesion: f32,
    /// Mean remaining need for company, `0..=1`.
    pub company: f32,
}

/// Read every NPC's personality, needs and motion, and average them per archetype.
///
/// Nearest-neighbour distance is the expensive part — a quadratic sweep over the crowd — and it is
/// deliberate: a metric built out of the same spatial index the steering used would agree with the
/// steering by construction. At demo sizes this costs milliseconds and stays honest.
#[must_use]
pub fn summarise(world: &mut World) -> Vec<ArchetypeBehaviour> {
    let mut query: QueryState<(
        &Npc,
        &NpcIdentity,
        &NpcTemperament,
        &NpcNeeds,
        &NpcVelocity,
        &Transform,
    )> = world.query();
    let mut rows: Vec<Row> = query
        .iter(world)
        .map(|(npc, identity, temperament, needs, velocity, transform)| {
            let bias = TemperamentDrive::default().bias(&temperament.0, &needs.0);
            Row {
                entity: npc.0,
                archetype: identity.archetype,
                position: transform.translation,
                speed: velocity.0.length(),
                separation: bias.separation,
                alignment: bias.alignment,
                cohesion: bias.cohesion,
                company: needs.0.company,
            }
        })
        .collect();
    // Sorted by id, so the report does not depend on ECS iteration order.
    rows.sort_by_key(|row| row.entity.to_bits());
    let mut nearest = vec![f32::INFINITY; rows.len()];
    for (index, row) in rows.iter().enumerate() {
        for (other_index, other) in rows.iter().enumerate() {
            if index == other_index {
                continue;
            }
            let distance = row.position.distance(other.position);
            if let Some(slot) = nearest.get_mut(index) {
                *slot = slot.min(distance);
            }
        }
    }
    let mut groups: BTreeMap<u8, Accumulator> = BTreeMap::new();
    for (row, gap) in rows.iter().zip(nearest) {
        let key = Archetype::ALL
            .iter()
            .position(|candidate| *candidate == row.archetype)
            .unwrap_or(0) as u8;
        let group = groups.entry(key).or_default();
        group.count += 1;
        group.speed += row.speed;
        group.spacing += if gap.is_finite() { gap } else { 0.0 };
        group.separation += row.separation;
        group.alignment += row.alignment;
        group.cohesion += row.cohesion;
        group.company += row.company;
    }
    groups
        .into_iter()
        .filter_map(|(key, group)| {
            let archetype = Archetype::ALL.get(usize::from(key)).copied()?;
            let count = group.count.max(1) as f32;
            Some(ArchetypeBehaviour {
                archetype,
                count: group.count,
                speed: group.speed / count,
                spacing: group.spacing / count,
                separation: group.separation / count,
                alignment: group.alignment / count,
                cohesion: group.cohesion / count,
                company: group.company / count,
            })
        })
        .collect()
}

/// One NPC, flattened out of its components.
struct Row {
    entity: bevy_flock::domain::ids::NpcId,
    archetype: Archetype,
    position: bevy_math::Vec3,
    speed: f32,
    separation: f32,
    alignment: f32,
    cohesion: f32,
    company: f32,
}

/// Running totals for one archetype.
#[derive(Default)]
struct Accumulator {
    count: u32,
    speed: f32,
    spacing: f32,
    separation: f32,
    alignment: f32,
    cohesion: f32,
    company: f32,
}

/// Render the summary as one JSON array, indented to sit inside the run report.
#[must_use]
pub fn to_json_block(rows: &[ArchetypeBehaviour]) -> String {
    let entries: Vec<String> = rows
        .iter()
        .map(|row| {
            json::inline(&[
                ("archetype", json::string(row.archetype.as_str())),
                ("count", row.count.to_string()),
                ("speed", json::number(row.speed, 3)),
                ("spacing", json::number(row.spacing, 2)),
                ("separation", json::number(row.separation, 3)),
                ("alignment", json::number(row.alignment, 3)),
                ("cohesion", json::number(row.cohesion, 3)),
                ("company", json::number(row.company, 3)),
            ])
        })
        .collect();
    json::array_block(&entries, 2)
}

/// One line for the terminal: the separation multiplier of each archetype, widest first.
#[must_use]
pub fn headline(rows: &[ArchetypeBehaviour]) -> String {
    let mut sorted: Vec<&ArchetypeBehaviour> = rows.iter().collect();
    sorted.sort_by(|a, b| {
        b.separation
            .partial_cmp(&a.separation)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let parts: Vec<String> = sorted
        .iter()
        .map(|row| format!("{} x{:.2}", row.archetype.as_str(), row.separation))
        .collect();
    format!("people  separation {}", parts.join(" / "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_flock::adapters::primary::components::NpcBundle;
    use bevy_flock::domain::ids::NpcId;
    use bevy_flock::domain::temperament::{Needs, Temperament};
    use bevy_flock::testkit::OrDie;
    use bevy_math::Vec3;
    use bevy_transform::prelude::Transform;

    fn world_with(pairs: &[(Archetype, Temperament, Vec3)]) -> World {
        let mut world = World::new();
        for (index, (archetype, temperament, position)) in pairs.iter().enumerate() {
            let mut bundle = NpcBundle::default_for(NpcId::unique(index as u32));
            bundle.identity.archetype = *archetype;
            bundle.temperament = NpcTemperament(*temperament);
            bundle.needs = NpcNeeds(Needs::satisfied());
            bundle.transform = Transform::from_translation(*position);
            world.spawn(bundle);
        }
        world
    }

    #[test]
    fn a_guard_holds_rank_where_an_urchin_scatters() {
        let guard = Temperament::blend(
            Temperament::archetype(Archetype::Guard),
            Temperament::balanced(),
            0.34,
        );
        let urchin = Temperament::blend(
            Temperament::archetype(Archetype::Urchin),
            Temperament::balanced(),
            0.34,
        );
        let mut world = world_with(&[
            (Archetype::Guard, guard, Vec3::new(0.0, 0.0, 0.0)),
            (Archetype::Guard, guard, Vec3::new(1.0, 0.0, 0.0)),
            (Archetype::Urchin, urchin, Vec3::new(8.0, 0.0, 0.0)),
            (Archetype::Urchin, urchin, Vec3::new(9.0, 0.0, 0.0)),
        ]);
        let rows = summarise(&mut world);
        let find = |archetype: Archetype| {
            rows.iter()
                .find(|row| row.archetype == archetype)
                .cloned()
                .or_die()
        };
        let guard = find(Archetype::Guard);
        let urchin = find(Archetype::Urchin);
        // Discipline reads as: less jostling, more matching the neighbours' heading. It is the
        // opposite direction to the intuition that a guard would push hardest, and it is what the
        // measured table in the README shows.
        assert!(
            guard.separation < urchin.separation,
            "guard {} vs urchin {}",
            guard.separation,
            urchin.separation
        );
        assert!(
            guard.alignment > urchin.alignment,
            "guard {} vs urchin {}",
            guard.alignment,
            urchin.alignment
        );
        assert_eq!(guard.count, 2);
    }

    #[test]
    fn spacing_is_the_nearest_neighbour_not_the_crowd() {
        let plain = Temperament::balanced();
        // A tight pair and a distant third: the pair's spacing is one metre, not four.
        let mut world = world_with(&[
            (Archetype::Wanderer, plain, Vec3::new(0.0, 0.0, 0.0)),
            (Archetype::Wanderer, plain, Vec3::new(1.0, 0.0, 0.0)),
            (Archetype::Wanderer, plain, Vec3::new(5.0, 0.0, 0.0)),
        ]);
        let rows = summarise(&mut world);
        let row = rows.first().or_die();
        // 1, 1 and 4 metres: a mean of two.
        assert!((row.spacing - 2.0).abs() < 1e-4, "spacing {}", row.spacing);
    }

    #[test]
    fn the_report_names_every_archetype_it_measured() {
        let plain = Temperament::balanced();
        let mut world = world_with(&[
            (Archetype::Merchant, plain, Vec3::ZERO),
            (Archetype::Noble, plain, Vec3::new(2.0, 0.0, 0.0)),
        ]);
        let rows = summarise(&mut world);
        let text = to_json_block(&rows);
        assert!(text.contains("\"archetype\": \"merchant\""), "{text}");
        assert!(text.contains("\"archetype\": \"noble\""), "{text}");
        assert!(!text.contains("guard"), "{text}");
        assert!(headline(&rows).starts_with("people  separation "));
    }
}
