//! Where NPCs stand when they are spawned.
//!
//! A crowd's shape decides what the rest of the crate measures. The pack's frame cost, the grid's
//! occupancy and the density a designer sees all come from this one decision, so it is a type
//! rather than a line inside the spawn system: a game can say what it means, and the library can
//! validate it against the grid volume it will be simulated in.
//!
//! Five placements, and the default is the first:
//!
//! - [`Placement::spiral`] — a golden-angle sunflower, spacing-controlled, deterministic by index.
//! - [`Placement::uniform_rect`] — uniform random inside an axis-aligned rectangle.
//! - [`Placement::uniform_disc`] — uniform random inside a disc, which is what "a crowd here"
//!   usually means.
//! - [`Placement::uniform_volume`] — uniform random inside a box: a stairwell, a hangar, a city
//!   block with height.
//! - [`Placement::uniform_sphere`] — uniform random inside a sphere, volume-uniform (`r = R·∛u`),
//!   which is a swarm, a flock in the air, or debris.
//!
//! [`Placement::fixed`] covers the fifth: points you authored, at whatever height you like.
//!
//! Random placements draw from the NPC's own RNG stream (`RngStream::for_npc`), never from a
//! shared one: two NPCs with the same id and seed land in the same place no matter what order the
//! spawner visits them in, and a replay is a replay.

use bevy_math::Vec3;

use crate::domain::ids::{NpcId, SimSeed};
use crate::domain::num;
use crate::domain::rng::RngStream;
use crate::domain::spatial::SpatialGridConfig;
use crate::error::ConfigError;

/// The spacing the default spiral uses, in metres.
///
/// 1.6 m is roughly shoulder to shoulder for a standing adult, which is why the default crowd
/// reads as a crowd rather than as scattered individuals.
pub const DEFAULT_SPACING: f32 = 1.6;

const GOLDEN_ANGLE: f32 = 2.399_963_2;

/// Where new NPCs are placed on the plane `y = 0`.
#[derive(Debug, Clone, PartialEq)]
pub enum Placement {
    /// A golden-angle spiral around `centre`, `spacing` metres apart.
    ///
    /// Deterministic and index-ordered: NPC 0 is at `centre`, and the crowd grows outward in a
    /// disc. Density is uniform inside the disc, and the disc's radius is `spacing * sqrt(n)`, so
    /// the crowd is compact at any population.
    Spiral {
        /// Middle of the crowd.
        centre: Vec3,
        /// Distance between neighbouring points, in metres.
        spacing: f32,
    },
    /// Uniform random inside an axis-aligned rectangle.
    UniformRect {
        /// Corner with the smallest coordinates.
        min: Vec3,
        /// Corner with the largest coordinates.
        max: Vec3,
    },
    /// Uniform random inside a disc: the usual way to say "a crowd here".
    ///
    /// Area-uniform, not radius-uniform: half the crowd lands inside half the radius, which is
    /// what a uniform density looks like. (Sampling the radius linearly instead would pile agents
    /// toward the middle and is the classic mistake.)
    UniformDisc {
        /// Middle of the disc.
        centre: Vec3,
        /// Radius of the disc, in metres.
        radius: f32,
    },
    /// Uniform random inside a box. The three-dimensional answer for a space with height.
    UniformVolume {
        /// Corner with the smallest coordinates.
        min: Vec3,
        /// Corner with the largest coordinates.
        max: Vec3,
    },
    /// Uniform random inside a sphere.
    ///
    /// Volume-uniform, not radius-uniform: the distance is `R·∛u`, so half the crowd sits inside
    /// 79% of the radius, which is what an even density looks like in three dimensions.
    UniformSphere {
        /// Middle of the sphere.
        centre: Vec3,
        /// Radius of the sphere, in metres.
        radius: f32,
    },
    /// Positions you chose yourself: doorways, patrol anchors, the stools in a tavern.
    ///
    /// Indexed, not shuffled: NPC `i` stands at `positions[i % len]`. When the population is
    /// larger than the list the extras stand on top of earlier ones, and separation pushes them
    /// apart on the first frames — which is the honest behaviour for a spawner that was given
    /// fewer places than people, but supply as many points as you have NPCs if exact placement
    /// matters.
    Fixed {
        /// The authored positions, in the order NPCs are given them.
        positions: Vec<Vec3>,
    },
}

impl Placement {
    /// The default spiral: 1.6 m spacing, centred on the origin.
    #[must_use]
    pub const fn spiral(spacing: f32) -> Self {
        Self::Spiral {
            centre: Vec3::ZERO,
            spacing,
        }
    }

    /// A spiral around an arbitrary centre.
    #[must_use]
    pub const fn spiral_at(centre: Vec3, spacing: f32) -> Self {
        Self::Spiral { centre, spacing }
    }

    /// Uniform random inside a rectangle on the plane.
    #[must_use]
    pub const fn uniform_rect(min: Vec3, max: Vec3) -> Self {
        Self::UniformRect { min, max }
    }

    /// Uniform random inside a disc on the plane.
    #[must_use]
    pub const fn uniform_disc(centre: Vec3, radius: f32) -> Self {
        Self::UniformDisc { centre, radius }
    }

    /// Uniform random inside a box.
    #[must_use]
    pub const fn uniform_volume(min: Vec3, max: Vec3) -> Self {
        Self::UniformVolume { min, max }
    }

    /// Uniform random inside a sphere.
    #[must_use]
    pub const fn uniform_sphere(centre: Vec3, radius: f32) -> Self {
        Self::UniformSphere { centre, radius }
    }

    /// Use these positions, in this order.
    #[must_use]
    pub const fn fixed(positions: Vec<Vec3>) -> Self {
        Self::Fixed { positions }
    }

    /// Whether this placement draws random numbers.
    ///
    /// The spiral does not, which is what lets the spawner keep the draw order (and therefore
    /// every generated temperament and archetype) identical for a seed whether or not a game
    /// changes its placement to a spiral.
    #[must_use]
    pub const fn is_random(&self) -> bool {
        matches!(
            self,
            Self::UniformRect { min: _, max: _ }
                | Self::UniformDisc {
                    centre: _,
                    radius: _
                }
                | Self::UniformVolume { min: _, max: _ }
                | Self::UniformSphere {
                    centre: _,
                    radius: _
                }
        )
    }

    /// Where NPC number `index` goes.
    ///
    /// `rng` is that NPC's own stream. A spiral and an authored list consume nothing from it; a
    /// random placement consumes two draws and is reproducible from the seed alone.
    #[must_use]
    pub fn position(&self, index: u32, rng: &mut RngStream) -> Vec3 {
        match self {
            Self::Spiral { centre, spacing } => {
                let step = num::u32_to_f32(index);
                let angle = step * GOLDEN_ANGLE;
                // `sqrt` of the index is what makes the density uniform rather than the ring
                // spacing uniform: equal-area rings need radii in sqrt(k).
                let radius = *spacing * libm::sqrtf(step);
                Vec3::new(
                    libm::cosf(angle).mul_add(radius, centre.x),
                    centre.y,
                    libm::sinf(angle).mul_add(radius, centre.z),
                )
            }
            Self::UniformRect { min, max } => Vec3::new(
                (max.x - min.x).mul_add(rng.next_unit_f32(), min.x),
                min.y,
                (max.z - min.z).mul_add(rng.next_unit_f32(), min.z),
            ),
            Self::UniformDisc { centre, radius } => {
                let angle = rng.next_unit_f32() * core::f32::consts::TAU;
                // sqrt keeps the points area-uniform; a uniform radius would crowd the middle.
                let distance = *radius * libm::sqrtf(rng.next_unit_f32());
                Vec3::new(
                    libm::cosf(angle).mul_add(distance, centre.x),
                    centre.y,
                    libm::sinf(angle).mul_add(distance, centre.z),
                )
            }
            Self::UniformVolume { min, max } => Vec3::new(
                (max.x - min.x).mul_add(rng.next_unit_f32(), min.x),
                (max.y - min.y).mul_add(rng.next_unit_f32(), min.y),
                (max.z - min.z).mul_add(rng.next_unit_f32(), min.z),
            ),
            Self::UniformSphere { centre, radius } => {
                // A uniform direction on the sphere, then a radius that makes the *volume*
                // uniform: cube root, because volume grows as r³.
                let z = rng.next_unit_f32().mul_add(2.0, -1.0);
                let angle = rng.next_unit_f32() * core::f32::consts::TAU;
                let planar = libm::sqrtf((1.0 - z * z).max(0.0));
                let distance = *radius * libm::cbrtf(rng.next_unit_f32());
                Vec3::new(
                    (libm::cosf(angle) * planar).mul_add(distance, centre.x),
                    centre.y + z * distance,
                    (libm::sinf(angle) * planar).mul_add(distance, centre.z),
                )
            }
            Self::Fixed { positions } => {
                // Declared, not drawn: an authored list must not depend on the stream, and an
                // empty one is refused by `validate`, so zero is only the no-panic default.
                let len = positions.len();
                if len == 0 {
                    return Vec3::ZERO;
                }
                positions
                    .get((index as usize) % len)
                    .copied()
                    .unwrap_or(Vec3::ZERO)
            }
        }
    }

    /// The same position for an NPC, drawing from that NPC's own stream.
    #[must_use]
    pub fn position_for(&self, npc: NpcId, seed: SimSeed) -> Vec3 {
        let mut rng = RngStream::for_npc(seed, npc);
        self.position(npc.index(), &mut rng)
    }

    /// Check the placement against the volume it will be simulated in.
    ///
    /// The grid clamps out-of-bounds positions into the nearest cell, so a crowd spawned outside
    /// the volume does not fail loudly: it piles into the border cells and the frame cost of those
    /// cells explodes. This is the check that stops that happening, and it is why the placement is
    /// validated against the grid rather than on its own.
    ///
    /// # Errors
    ///
    /// [`ConfigError::InvalidDistance`] for a spacing or radius that is not finite and positive,
    /// [`ConfigError::PlacementOutsideGrid`] when the reachable area leaves the volume, and
    /// [`ConfigError::Empty`] for an inverted rectangle or an empty list of positions.
    pub fn validate(&self, grid: &SpatialGridConfig) -> Result<(), ConfigError> {
        match self {
            Self::Spiral { centre, spacing } => {
                if !spacing.is_finite() || *spacing <= 0.0 {
                    return Err(ConfigError::InvalidDistance {
                        field: "placement.spacing",
                        value: *spacing,
                    });
                }
                inside(*centre, grid, "placement.centre")
            }
            Self::UniformRect { min, max } => {
                if min.x > max.x || min.z > max.z {
                    return Err(ConfigError::Empty {
                        kind: "placement rectangle",
                    });
                }
                inside(*min, grid, "placement.min")?;
                inside(*max, grid, "placement.max")
            }
            Self::UniformDisc { centre, radius } => {
                if !radius.is_finite() || *radius <= 0.0 {
                    return Err(ConfigError::InvalidDistance {
                        field: "placement.radius",
                        value: *radius,
                    });
                }
                inside(*centre, grid, "placement.centre")?;
                // The disc has to fit. Check the four extremes so the message names the axis that
                // actually leaves the volume rather than a bounding box that merely contains it.
                for (offset, field) in [
                    (Vec3::new(*radius, 0.0, 0.0), "placement.radius (+x)"),
                    (Vec3::new(-*radius, 0.0, 0.0), "placement.radius (-x)"),
                    (Vec3::new(0.0, 0.0, *radius), "placement.radius (+z)"),
                    (Vec3::new(0.0, 0.0, -*radius), "placement.radius (-z)"),
                ] {
                    inside(*centre + offset, grid, field)?;
                }
                Ok(())
            }
            Self::UniformVolume { min, max } => {
                if min.x > max.x || min.y > max.y || min.z > max.z {
                    return Err(ConfigError::Empty {
                        kind: "placement volume",
                    });
                }
                inside(*min, grid, "placement.min")?;
                inside(*max, grid, "placement.max")
            }
            Self::UniformSphere { centre, radius } => {
                if !radius.is_finite() || *radius <= 0.0 {
                    return Err(ConfigError::InvalidDistance {
                        field: "placement.radius",
                        value: *radius,
                    });
                }
                inside(*centre, grid, "placement.centre")?;
                for (offset, field) in [
                    (Vec3::new(*radius, 0.0, 0.0), "placement.radius (+x)"),
                    (Vec3::new(-*radius, 0.0, 0.0), "placement.radius (-x)"),
                    (Vec3::new(0.0, *radius, 0.0), "placement.radius (+y)"),
                    (Vec3::new(0.0, -*radius, 0.0), "placement.radius (-y)"),
                    (Vec3::new(0.0, 0.0, *radius), "placement.radius (+z)"),
                    (Vec3::new(0.0, 0.0, -*radius), "placement.radius (-z)"),
                ] {
                    inside(*centre + offset, grid, field)?;
                }
                Ok(())
            }
            Self::Fixed { positions } => {
                if positions.is_empty() {
                    return Err(ConfigError::Empty {
                        kind: "placement.positions",
                    });
                }
                for point in positions {
                    inside(*point, grid, "placement.positions")?;
                }
                Ok(())
            }
        }
    }
}

impl Default for Placement {
    /// The golden-angle spiral at [`DEFAULT_SPACING`], centred on the origin.
    fn default() -> Self {
        Self::spiral(DEFAULT_SPACING)
    }
}

fn inside(point: Vec3, grid: &SpatialGridConfig, field: &'static str) -> Result<(), ConfigError> {
    let axes = [
        (point.x, grid.min.x, grid.max.x, 'x'),
        (point.y, grid.min.y, grid.max.y, 'y'),
        (point.z, grid.min.z, grid.max.z, 'z'),
    ];
    for (value, min, max, axis) in axes {
        if value < min || value > max {
            return Err(ConfigError::PlacementOutsideGrid {
                field,
                axis,
                value,
                min,
                max,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::OrDie;

    fn grid() -> SpatialGridConfig {
        SpatialGridConfig::default()
    }

    fn stream(npc: u32, seed: u64) -> RngStream {
        RngStream::for_npc(SimSeed::new(seed), NpcId::unique(npc))
    }

    #[test]
    fn the_spiral_is_the_documented_sunflower() {
        let placement = Placement::default();
        let first = placement.position_for(NpcId::unique(0), SimSeed::new(7));
        assert!(first.length() < 1e-6, "index zero sits at the centre");

        // Radius grows as spacing * sqrt(index), which is what makes it area-uniform.
        for index in [1_u32, 10, 100, 1_000] {
            let point = placement.position_for(NpcId::unique(index), SimSeed::new(7));
            let expected = DEFAULT_SPACING * libm::sqrtf(num::u32_to_f32(index));
            assert!(
                (point.length() - expected).abs() < 1e-3,
                "index {index} should be {expected} m out, was {}",
                point.length()
            );
        }
    }

    #[test]
    fn a_spiral_draws_nothing_from_the_stream() {
        let placement = Placement::spiral(DEFAULT_SPACING);
        let mut rng = stream(3, 11);
        let before = rng.state_bits();
        let _ = placement.position(3, &mut rng);
        assert_eq!(before, rng.state_bits(), "the spiral must be draw-free");
    }

    #[test]
    fn random_placements_stay_inside_their_bounds() {
        let rect =
            Placement::uniform_rect(Vec3::new(-50.0, 0.0, -20.0), Vec3::new(30.0, 0.0, 60.0));
        let disc = Placement::uniform_disc(Vec3::new(10.0, 0.0, -5.0), 40.0);
        for index in 0..2_000 {
            let point = rect.position_for(NpcId::unique(index), SimSeed::new(5));
            assert!((-50.0..=30.0).contains(&point.x), "x {} escaped", point.x);
            assert!((-20.0..=60.0).contains(&point.z), "z {} escaped", point.z);
            assert!(point.y.abs() < 1e-6, "the crowd is planar");

            let point = disc.position_for(NpcId::unique(index), SimSeed::new(5));
            let offset = Vec3::new(point.x - 10.0, 0.0, point.z + 5.0);
            assert!(
                offset.length() <= 40.0 + 1e-3,
                "{} escaped the disc",
                offset.length()
            );
        }
    }

    #[test]
    fn a_disc_is_area_uniform_rather_than_radius_uniform() {
        let placement = Placement::uniform_disc(Vec3::ZERO, 100.0);
        let mut inner = 0_u32;
        let samples = 20_000_u32;
        for index in 0..samples {
            let point = placement.position_for(NpcId::unique(index), SimSeed::new(99));
            if point.length() <= 50.0 {
                inner += 1;
            }
        }
        // Half the radius is a quarter of the area. Allow generous slack for a 20k sample:
        // radius-uniform sampling would land near 50% here, so the two are unmistakable.
        let fraction = f64::from(inner) / f64::from(samples);
        assert!(
            (0.22..0.28).contains(&fraction),
            "inner half held {fraction:.3} of the crowd, expected about 0.25"
        );
    }

    #[test]
    fn a_volume_stays_inside_its_box() {
        let min = Vec3::new(-30.0, 1.0, -10.0);
        let max = Vec3::new(20.0, 12.0, 40.0);
        let placement = Placement::uniform_volume(min, max);
        placement.validate(&grid()).or_die();
        for index in 0..2_000 {
            let point = placement.position_for(NpcId::unique(index), SimSeed::new(3));
            assert!((min.x..=max.x).contains(&point.x), "x {} escaped", point.x);
            assert!((min.y..=max.y).contains(&point.y), "y {} escaped", point.y);
            assert!((min.z..=max.z).contains(&point.z), "z {} escaped", point.z);
        }
    }

    #[test]
    fn a_sphere_is_volume_uniform_rather_than_radius_uniform() {
        // Half the radius is an eighth of the volume, so a volume-uniform sphere puts about 12.5%
        // inside it. Sampling the radius linearly would put half the crowd there and make a
        // swarm look like a shell around an empty middle.
        let placement = Placement::uniform_sphere(Vec3::new(0.0, 5.0, 0.0), 60.0);
        let mut inner = 0_u32;
        let samples = 20_000_u32;
        for index in 0..samples {
            let point = placement.position_for(NpcId::unique(index), SimSeed::new(77));
            let offset = point - Vec3::new(0.0, 5.0, 0.0);
            assert!(offset.length() <= 60.0 + 1e-3, "escaped the sphere");
            if offset.length() <= 30.0 {
                inner += 1;
            }
        }
        let fraction = f64::from(inner) / f64::from(samples);
        assert!(
            (0.10..0.15).contains(&fraction),
            "inner eighth held {fraction:.3} of the crowd, expected about 0.125"
        );
    }

    #[test]
    fn a_sphere_and_a_volume_can_leave_the_grid() {
        let grid = grid();
        assert!(matches!(
            Placement::uniform_volume(Vec3::ZERO, Vec3::new(10.0, 900.0, 10.0)).validate(&grid),
            Err(ConfigError::PlacementOutsideGrid {
                axis: 'y',
                field: _,
                value: _,
                min: _,
                max: _
            })
        ));
        assert!(matches!(
            Placement::uniform_sphere(Vec3::new(0.0, 0.0, 450.0), 100.0).validate(&grid),
            Err(ConfigError::PlacementOutsideGrid {
                field: _,
                axis: _,
                value: _,
                min: _,
                max: _
            })
        ));
        let inverted =
            Placement::uniform_volume(Vec3::new(0.0, 5.0, 0.0), Vec3::new(0.0, 1.0, 0.0));
        assert!(matches!(
            inverted.validate(&grid),
            Err(ConfigError::Empty { kind: _ })
        ));
    }

    #[test]
    fn authored_positions_keep_their_height() {
        let points = vec![Vec3::new(1.0, 3.5, 2.0), Vec3::new(-2.0, 0.0, 4.0)];
        let placement = Placement::fixed(points.clone());
        placement.validate(&grid()).or_die();
        for (index, want) in points.iter().enumerate() {
            let got =
                placement.position_for(NpcId::unique(num::usize_to_u32(index)), SimSeed::new(1));
            assert_eq!(got, *want, "authored height must survive");
        }
    }

    #[test]
    fn the_same_npc_lands_in_the_same_place() {
        let disc = Placement::uniform_disc(Vec3::new(3.0, 0.0, 4.0), 25.0);
        for index in [0_u32, 1, 17, 4_096] {
            assert_eq!(
                disc.position_for(NpcId::unique(index), SimSeed::new(42)),
                disc.position_for(NpcId::unique(index), SimSeed::new(42)),
                "the same id and seed must be reproducible"
            );
        }
        // And the seeds do what seeds do.
        assert_ne!(
            disc.position_for(NpcId::unique(9), SimSeed::new(42)),
            disc.position_for(NpcId::unique(9), SimSeed::new(43))
        );
    }

    #[test]
    fn placement_inside_the_volume_validates_and_outside_does_not() {
        let grid = grid();
        Placement::spiral(DEFAULT_SPACING).validate(&grid).or_die();
        Placement::uniform_disc(Vec3::ZERO, 200.0)
            .validate(&grid)
            .or_die();

        let outside = Placement::uniform_disc(Vec3::new(450.0, 0.0, 0.0), 100.0);
        assert!(matches!(
            outside.validate(&grid),
            Err(ConfigError::PlacementOutsideGrid {
                axis: 'x',
                field: _,
                value: _,
                min: _,
                max: _
            })
        ));

        let inverted =
            Placement::uniform_rect(Vec3::new(10.0, 0.0, 0.0), Vec3::new(-10.0, 0.0, 0.0));
        assert!(matches!(
            inverted.validate(&grid),
            Err(ConfigError::Empty { kind: _ })
        ));

        let bad_radius = Placement::uniform_disc(Vec3::ZERO, f32::NAN);
        assert!(matches!(
            bad_radius.validate(&grid),
            Err(ConfigError::InvalidDistance { field: _, value: _ })
        ));
    }
}
