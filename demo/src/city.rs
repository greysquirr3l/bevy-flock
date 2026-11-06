//! The city: districts, ruined blocks, roads, shelters and wayfinding.
//!
//! The layout is the spatial argument this demo is making, so it is generated rather than
//! authored, and generated *deterministically*: every draw comes from a stream selected by
//! subsystem, so adding a shelter does not move a block, and the same seed always produces the
//! same city. `tests/city_layout.rs` holds the invariants that make it a city rather than a
//! scatter of boxes.
//!
//! The shape is taken from the Third Floor brief: a mostly-ruined urban expanse, inhabited
//! settlements of graded size rather than named towns, independent shelters out in the ruins,
//! and wayfinding at the junctions between them. The names, the districts and every asset are
//! original; see `README.md`.

use bevy_flock::domain::ids::{NpcId, SimSeed};
use bevy_flock::domain::num;
use bevy_flock::domain::rng::RngStream;
use bevy_math::Vec3;

/// How large a settlement is.
///
/// The brief grades settlements tiny through extra-large instead of naming them, which is also how a
/// procedural city has to think: a size class decides the crowd it holds, the stalls it can support,
/// and whether it gets a guild frontage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SettlementClass {
    /// A crossroads with a well and two stalls.
    Tiny,
    /// A street of shops with a gate ward.
    Small,
    /// The smallest settlement with a guild frontage and a full market.
    Medium,
    /// A district that other settlements route through.
    Large,
}

impl SettlementClass {
    /// The population this class carries at [`CityLayout::population_scale`] 1.0.
    const fn people(self) -> u32 {
        match self {
            Self::Tiny => 40,
            Self::Small => 120,
            Self::Medium => 340,
            Self::Large => 700,
        }
    }

    /// Market stalls this class supports.
    const fn stalls(self) -> u32 {
        match self {
            Self::Tiny => 2,
            Self::Small => 5,
            Self::Medium => 9,
            Self::Large => 14,
        }
    }

    /// Whether the settlement has a guild frontage. Only medium and up, per the brief: a crawler
    /// walks to a bigger town for a guild, and that walk is the interesting decision.
    const fn has_guild(self) -> bool {
        matches!(self, Self::Medium | Self::Large)
    }

    /// The label used in the plan and the report.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Tiny => "tiny",
            Self::Small => "small",
            Self::Medium => "medium",
            Self::Large => "large",
        }
    }
}

/// One inhabited settlement.
#[derive(Debug, Clone)]
pub struct District {
    /// Stable identifier, used in the artefact filenames and the scene.
    pub slug: String,
    /// How big it is.
    pub class: SettlementClass,
    /// Centre in world metres.
    pub centre: Vec3,
    /// Radius of the built-up area.
    pub radius: f32,
    /// Whether a guild frontage exists here.
    pub guild: bool,
    /// Share of the crowd that lives here, summing to 1 over the layout.
    pub population_share: f32,
}

/// A ruined building footprint.
#[derive(Debug, Clone, Copy)]
pub struct Block {
    /// Centre of the footprint at ground level.
    pub centre: Vec3,
    /// Half-extents in x and z, and full height.
    pub size: Vec3,
    /// 0 is a standing shell, 1 is a pile of rubble with a surviving corner.
    pub ruin: f32,
}

/// A place to hide: independent of any settlement, as the brief has them.
#[derive(Debug, Clone, Copy)]
pub struct Shelter {
    /// Where the door is.
    pub position: Vec3,
    /// Which way it faces, radians.
    pub yaw: f32,
}

/// Wayfinding. One per settlement gate, plus the junctions in the ruins.
#[derive(Debug, Clone, Copy)]
pub struct Pylon {
    /// Where it stands.
    pub position: Vec3,
    /// Which way the plates face, radians.
    pub yaw: f32,
    /// Index into [`CityLayout::districts`], or `None` for a junction out in the ruins.
    pub district: Option<usize>,
}

/// A market stall.
#[derive(Debug, Clone, Copy)]
pub struct Stall {
    /// Where it stands.
    pub position: Vec3,
    /// Which way it faces, radians.
    pub yaw: f32,
}

/// The whole layout.
#[derive(Debug, Clone)]
pub struct CityLayout {
    /// The seed it was generated from.
    pub seed: SimSeed,
    /// Half-width of the square the city occupies, in metres.
    pub extent: f32,
    /// Distance between road centrelines, in metres.
    pub block_pitch: f32,
    /// Width of a road, in metres.
    pub road_width: f32,
    /// Inhabited settlements, largest first so the plan reads top-down.
    pub districts: Vec<District>,
    /// Ruined footprints.
    pub blocks: Vec<Block>,
    /// Shelters in the ruins.
    pub shelters: Vec<Shelter>,
    /// Wayfinding pylons.
    pub pylons: Vec<Pylon>,
    /// Market stalls, inside the settlements.
    pub stalls: Vec<Stall>,
}

/// The unit float from a raw draw. One division, no libm, same answer on every platform.
fn unit(raw: u32) -> f32 {
    num::u32_to_f32(raw >> 8) / 16_777_216.0
}

/// A draw in `-1.0..=1.0`.
fn signed(raw: u32) -> f32 {
    unit(raw) * 2.0 - 1.0
}

impl CityLayout {
    /// Generate a city of `extent` metres half-width from `seed`.
    ///
    /// Six settlements on a ring, one at the centre, and ruins between and beyond them. Every
    /// parameter below is either a rule from the brief or a number chosen once and written down,
    /// never a draw whose meaning depends on call order.
    #[must_use]
    #[expect(
        clippy::too_many_lines,
        reason = "one pass per layer of the city -- districts, blocks, shelters, pylons, stalls -- \
                  in the order the layers depend on each other; splitting it would hide that order"
    )]
    pub fn generate(seed: SimSeed, extent: f32) -> Self {
        let block_pitch = 34.0;
        let road_width = 9.0;

        // Settlements. The centre holds the largest district because that is where the roads
        // meet; the ring alternates size so no route is all small towns.
        let ring = [
            ("gate-row", SettlementClass::Large, 0.0_f32),
            ("aerie", SettlementClass::Medium, 60.0),
            ("the-kiln", SettlementClass::Small, 120.0),
            ("salt-market", SettlementClass::Medium, 180.0),
            ("low-yard", SettlementClass::Small, 240.0),
            ("tannery-row", SettlementClass::Tiny, 300.0),
        ];
        let mut district_rng = RngStream::new(seed, 0x01);
        let mut districts = Vec::with_capacity(ring.len() + 1);
        let settlement_radius = extent * 0.42;
        districts.push(District {
            slug: "gate-row".to_owned(),
            class: SettlementClass::Large,
            centre: Vec3::new(
                signed(district_rng.next_u32()) * extent * 0.05,
                0.0,
                signed(district_rng.next_u32()) * extent * 0.05,
            ),
            radius: extent * 0.16,
            guild: SettlementClass::Large.has_guild(),
            population_share: 0.0,
        });
        for (slug, class, degrees) in ring.iter().skip(1) {
            let angle = degrees.to_radians();
            let jitter = signed(district_rng.next_u32()) * extent * 0.03;
            let distance = settlement_radius + jitter;
            districts.push(District {
                slug: (*slug).to_owned(),
                class: *class,
                centre: Vec3::new(distance * angle.cos(), 0.0, distance * angle.sin()),
                radius: extent
                    * match class {
                        SettlementClass::Tiny => 0.055,
                        SettlementClass::Small => 0.075,
                        SettlementClass::Medium => 0.10,
                        SettlementClass::Large => 0.13,
                    },
                guild: class.has_guild(),
                population_share: 0.0,
            });
        }

        // Population shares follow the class weights, normalised, so the crowd is where the city
        // says people live rather than spread evenly over the ruins.
        let total: u32 = districts.iter().map(|d| d.class.people()).sum();
        for district in &mut districts {
            district.population_share =
                num::u32_to_f32(district.class.people()) / num::u32_to_f32(total);
        }

        // Ruined blocks on a jittered grid. A cell is skipped when it is inside a settlement (the
        // settlement builds its own frontage) or when the roads have taken it.
        let mut block_rng = RngStream::new(seed, 0x02);
        let mut blocks = Vec::new();
        let cells = num::f32_to_i32((extent * 2.0 / block_pitch).round());
        let half = f32::from(i16::try_from(cells).unwrap_or(i16::MAX)) * block_pitch * 0.5;
        for ix in 0..cells {
            for iz in 0..cells {
                let x = f32::from(i16::try_from(ix).unwrap_or(0)) * block_pitch - half;
                let z = f32::from(i16::try_from(iz).unwrap_or(0)) * block_pitch - half;
                let mut centre = Vec3::new(
                    x + signed(block_rng.next_u32()) * block_pitch * 0.08,
                    0.0,
                    z + signed(block_rng.next_u32()) * block_pitch * 0.08,
                );
                if districts
                    .iter()
                    .any(|district| centre.distance(district.centre) < district.radius * 1.25)
                {
                    continue;
                }
                // A fifth of the cells are empty lots. A grid where every cell is built reads as
                // wallpaper; the gaps are what makes the ruins read as a city that lost things.
                if unit(block_rng.next_u32()) < 0.19 {
                    continue;
                }
                let footprint = block_pitch * 0.5;
                let width = footprint * (0.55 + unit(block_rng.next_u32()) * 0.75);
                let depth = footprint * (0.55 + unit(block_rng.next_u32()) * 0.75);
                // Taller toward the middle of the city: a dying downtown, not a uniform suburb.
                let centrality = 1.0 - (centre.length() / (extent * 1.2)).clamp(0.0, 1.0);
                let storeys = 1.0 + centrality * 5.0 + unit(block_rng.next_u32()) * 2.0;
                let ruin = (0.15 + unit(block_rng.next_u32()) * 0.8).clamp(0.0, 1.0);
                centre.y = 0.0;
                blocks.push(Block {
                    centre,
                    size: Vec3::new(width, storeys * 3.4, depth),
                    ruin,
                });
            }
        }

        // Shelters: out in the ruins, each roughly a block from a road junction so it is findable.
        let mut shelter_rng = RngStream::new(seed, 0x03);
        let shelter_count = 4 + districts.len();
        let mut shelters = Vec::with_capacity(shelter_count);
        let mut attempts = 0_u32;
        while shelters.len() < shelter_count && attempts < 512 {
            attempts += 1;
            let candidate = Vec3::new(
                signed(shelter_rng.next_u32()) * extent * 0.92,
                0.0,
                signed(shelter_rng.next_u32()) * extent * 0.92,
            );
            let in_settlement = districts
                .iter()
                .any(|district| candidate.distance(district.centre) < district.radius * 1.35);
            let too_close = shelters
                .iter()
                .any(|shelter: &Shelter| candidate.distance(shelter.position) < extent * 0.12);
            if in_settlement || too_close {
                continue;
            }
            shelters.push(Shelter {
                position: candidate,
                yaw: unit(shelter_rng.next_u32()) * core::f32::consts::TAU,
            });
        }

        // Wayfinding: one at each settlement gate, plus junctions in the ruins, because a sign is
        // useful exactly where a route decision is.
        let mut pylon_rng = RngStream::new(seed, 0x04);
        let mut pylons = Vec::with_capacity(districts.len() + 4);
        for (index, district) in districts.iter().enumerate() {
            let angle = unit(pylon_rng.next_u32()) * core::f32::consts::TAU;
            pylons.push(Pylon {
                position: Vec3::new(
                    district.centre.x + district.radius * 1.02 * angle.cos(),
                    0.0,
                    district.centre.z + district.radius * 1.02 * angle.sin(),
                ),
                yaw: angle + core::f32::consts::PI,
                district: Some(index),
            });
        }
        for step in 0..4 {
            let angle = core::f32::consts::FRAC_PI_2 * num::i32_to_f32(step) + 0.4;
            let distance = extent * 0.72;
            pylons.push(Pylon {
                position: Vec3::new(distance * angle.cos(), 0.0, distance * angle.sin()),
                yaw: angle,
                district: None,
            });
        }

        // Stalls: inside the built-up radius, in a loose arc facing the district's middle, so the
        // market reads as a street rather than a scatter.
        let mut stall_rng = RngStream::new(seed, 0x05);
        let mut stalls = Vec::new();
        for district in &districts {
            for index in 0..district.class.stalls() {
                let fraction =
                    (num::u32_to_f32(index) + 0.5) / num::u32_to_f32(district.class.stalls());
                let angle = fraction * core::f32::consts::TAU + unit(stall_rng.next_u32()) * 0.3;
                let radius = district.radius * (0.35 + unit(stall_rng.next_u32()) * 0.4);
                stalls.push(Stall {
                    position: Vec3::new(
                        district.centre.x + radius * angle.cos(),
                        0.0,
                        district.centre.z + radius * angle.sin(),
                    ),
                    yaw: angle + core::f32::consts::PI,
                });
            }
        }

        districts.sort_by(|left, right| {
            right
                .class
                .cmp(&left.class)
                .then_with(|| left.slug.cmp(&right.slug))
        });

        Self {
            seed,
            extent,
            block_pitch,
            road_width,
            districts,
            blocks,
            shelters,
            pylons,
            stalls,
        }
    }

    /// Where the `slot`-th NPC of `district` stands, given the NPC's identity.
    ///
    /// Keyed on the NPC id rather than a call counter, so the answer does not depend on the order
    /// the ECS happens to iterate: two runs that spawn in a different order place every NPC in the
    /// same spot.
    #[must_use]
    pub fn npc_position(&self, district: usize, id: NpcId) -> Vec3 {
        let Some(area) = self.districts.get(district) else {
            return Vec3::ZERO;
        };
        let mut rng = RngStream::for_npc(self.seed, id);
        let angle = unit(rng.next_u32()) * core::f32::consts::TAU;
        let distance = area.radius * unit(rng.next_u32()).sqrt() * 0.92;
        Vec3::new(
            area.centre.x + distance * angle.cos(),
            0.0,
            area.centre.z + distance * angle.sin(),
        )
    }

    /// Which district the `slot`-th of `population` NPCs lives in.
    ///
    /// Deterministic and independent of spawn order: the shares partition the population by
    /// cumulative weight, so district sizes are exact rather than sampled.
    #[must_use]
    pub fn district_for(&self, slot: u32, population: u32) -> usize {
        if population == 0 || self.districts.is_empty() {
            return 0;
        }
        let position = (f64::from(slot) + 0.5) / f64::from(population);
        let mut cumulative = 0.0_f64;
        for (index, district) in self.districts.iter().enumerate() {
            cumulative += f64::from(district.population_share);
            if position <= cumulative {
                return index;
            }
        }
        self.districts.len() - 1
    }

    /// Total civilians the districts hold at scale 1.0.
    #[must_use]
    pub fn nominal_population(&self) -> u32 {
        self.districts.iter().map(|d| d.class.people()).sum()
    }

    /// A settlement by slug.
    #[must_use]
    pub fn district(&self, slug: &str) -> Option<&District> {
        self.districts.iter().find(|district| district.slug == slug)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_flock::testkit::OrDie;

    fn sample() -> CityLayout {
        CityLayout::generate(SimSeed::new(7), 900.0)
    }

    #[test]
    fn generation_is_deterministic() {
        let a = sample();
        let b = sample();
        assert_eq!(a.blocks.len(), b.blocks.len());
        assert_eq!(a.shelters.len(), b.shelters.len());
        assert_eq!(a.pylons.len(), b.pylons.len());
        assert_eq!(a.stalls.len(), b.stalls.len());
        for (left, right) in a.blocks.iter().zip(&b.blocks) {
            assert_eq!(left.centre, right.centre);
            assert_eq!(left.size, right.size);
        }
        for (left, right) in a.pylons.iter().zip(&b.pylons) {
            assert_eq!(left.position, right.position);
        }
    }

    #[test]
    fn a_different_seed_is_a_different_city() {
        let a = CityLayout::generate(SimSeed::new(7), 900.0);
        let b = CityLayout::generate(SimSeed::new(8), 900.0);
        let moved = a
            .blocks
            .iter()
            .zip(&b.blocks)
            .filter(|(left, right)| left.centre != right.centre)
            .count();
        assert!(moved > a.blocks.len() / 2, "only {moved} blocks moved");
    }

    #[test]
    fn every_settlement_has_wayfinding() {
        let city = sample();
        for (index, district) in city.districts.iter().enumerate() {
            let served = city
                .pylons
                .iter()
                .any(|pylon| pylon.district == Some(index));
            assert!(served, "{} has no pylon", district.slug);
        }
        assert_eq!(
            city.pylons.iter().filter(|p| p.district.is_none()).count(),
            4,
            "the ruins junctions are missing"
        );
    }

    #[test]
    fn shelters_stand_outside_the_settlements() {
        let city = sample();
        for shelter in &city.shelters {
            for district in &city.districts {
                assert!(
                    shelter.position.distance(district.centre) > district.radius,
                    "a shelter is inside {}",
                    district.slug
                );
            }
        }
    }

    #[test]
    fn blocks_keep_out_of_the_settlements() {
        let city = sample();
        for block in &city.blocks {
            for district in &city.districts {
                assert!(
                    block.centre.distance(district.centre) >= district.radius * 1.2,
                    "a block is inside {}",
                    district.slug
                );
            }
        }
    }

    #[test]
    fn the_district_assignment_partitions_the_population() {
        let city = sample();
        let population = 1_000;
        let mut counts = vec![0_u32; city.districts.len()];
        for slot in 0..population {
            let index = city.district_for(slot, population);
            if let Some(count) = counts.get_mut(index) {
                *count += 1;
            }
        }
        assert_eq!(counts.iter().sum::<u32>(), population);
        // Every district gets someone, and the largest gets the most: the shares are honoured
        // rather than rounded away.
        assert!(counts.iter().all(|&count| count > 0));
        let largest = city
            .districts
            .iter()
            .position(|district| district.class == SettlementClass::Large)
            .or_die();
        assert_eq!(
            counts.iter().copied().max().unwrap_or(0),
            counts.get(largest).copied().unwrap_or(0),
            "the large district should hold the most people"
        );
    }

    #[test]
    fn an_npc_stands_inside_its_own_district() {
        let city = sample();
        for slot in 0..64_u32 {
            let index = city.district_for(slot, 64);
            let position = city.npc_position(index, NpcId::unique(slot));
            let district = city.districts.get(index).or_die();
            assert!(
                position.distance(district.centre) <= district.radius,
                "npc {slot} left {}",
                district.slug
            );
        }
    }

    #[test]
    fn npc_positions_do_not_depend_on_the_order_they_are_asked_for() {
        let city = sample();
        let first: Vec<Vec3> = (0..32)
            .map(|id| city.npc_position(0, NpcId::unique(id)))
            .collect();
        let mut second: Vec<Vec3> = (0..32)
            .rev()
            .map(|id| city.npc_position(0, NpcId::unique(id)))
            .collect();
        second.reverse();
        assert_eq!(first, second);
    }

    #[test]
    fn a_smaller_city_is_still_a_city() {
        let city = CityLayout::generate(SimSeed::new(3), 260.0);
        assert!(!city.blocks.is_empty());
        assert!(!city.shelters.is_empty());
        assert_eq!(city.pylons.len(), city.districts.len() + 4);
    }
}
