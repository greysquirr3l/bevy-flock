//! The manifest band: entities inside a radius, density outside it (ADR 0020).
//!
//! This is the ECS half of the continuum. It owns three things and refuses to own a fourth:
//!
//! - **The field.** Population outside [`Band::fold_radius`] lives in cells, advected on a
//!   stride.
//! - **Manifestation.** Crossing inward takes people out of a cell with
//!   [`AmbientField::take`] and spawns entities for them, rationed per frame. A failed take
//!   means no entity, which is the only thing standing between this and a crowd that
//!   doubles.
//! - **Steering.** Band agents are copied into a [`Pack`] once per frame, steered as
//!   contiguous arrays, and copied back. That is the 1.50 ms per 5,000 measured in
//!   `benches/packed.rs`, against 9.47 ms through the ECS crowd path.
//!
//! What it does not own is memory. Anchored entities (ADR 0019) are skipped by folding
//! entirely: a [`BandAnchor`] is never despawned into a cell, because identity is the one
//! thing the continuum cannot reconstruct.

use bevy_app::prelude::*;
use bevy_ecs::prelude::*;
use bevy_transform::prelude::Transform;

use crate::adapters::primary::cognition::PlayerFocus;
use crate::application::packed::{Pack, SteerParams};
use crate::domain::continuum::{AmbientField, CellId, FieldError, Occupation};
use crate::domain::ids::NpcId;
use crate::domain::tick::Tick;

/// Marks an entity the band owns: it is steered by the packed kernel, not by the ECS crowd
/// systems, and it is the unit that folds into density.
#[derive(Component, Debug, Clone, Copy, PartialEq)]
pub struct BandAgent {
    /// Current velocity in metres per second, carried between frames.
    pub velocity: [f32; 2],
}

impl BandAgent {
    /// An agent at rest.
    #[must_use]
    pub const fn still() -> Self {
        Self {
            velocity: [0.0, 0.0],
        }
    }
}

/// A mid-band agent: individually present, steered on a cadence rather than every frame.
/// ADR 0021's second ring. Fidelity is a rate, not a population.
#[derive(Component, Debug, Clone, Copy, PartialEq)]
pub struct MidAgent {
    /// Current velocity in metres per second.
    pub velocity: [f32; 2],
}

/// Never folded, whatever the distance. Threads of the story, not population.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct BandAnchor;

/// How the band is sized and rationed.
#[derive(Resource, Debug, Clone, Copy, PartialEq)]
pub struct BandConfig {
    /// How many agents should be individually present, at most.
    pub target: u32,
    /// Agents per square metre inside the band. Drives the radius, because the packed
    /// kernel is bound by neighbours rather than by count (ADR 0020).
    pub density: f32,
    /// Fold radius as a multiple of the manifest radius. Above one, or agents oscillate.
    pub fold_slack: f32,
    /// Most agents manifested or folded on one frame.
    pub ration: u32,
    /// Mid-band target, steered on a cadence (ADR 0021).
    pub mid_target: u32,
    /// Frames between mid-band passes. Three is 20 Hz at 60.
    pub mid_cadence: u32,
    /// The mid band's world extent. It is a ring outside the near band, so its grid must be
    /// larger than the near one — sizing it from `extent` puts every mid agent in the
    /// grid's clamped edge cells, where they all become neighbours (see the gate-1M run).
    pub mid_extent: f32,
    /// Metres per cell.
    pub spacing: f32,
    /// The extent of the world the pack's grid must cover.
    pub extent: f32,
}

impl Default for BandConfig {
    fn default() -> Self {
        Self {
            target: 5_000,
            density: 1.0 / 40.0,
            fold_slack: 1.15,
            ration: 256,
            mid_target: 15_000,
            mid_cadence: 3,
            mid_extent: 2_400.0,
            spacing: 4.0,
            // The field must reach past the fold radius in every direction, or an agent
            // that walks out has no cell to fold into and stays an entity forever. The
            // gate_9 harness caught exactly that with 512.
            extent: 1_024.0,
        }
    }
}

impl BandConfig {
    /// The manifest radius that makes `target` agents sit at `density`.
    ///
    /// `sqrt(target / (density * PI))`: a disc. Raising the target without raising the
    /// radius buys neighbours, not people, which is the mistake the density bench exists to
    /// catch.
    #[must_use]
    pub fn radius(self) -> f32 {
        if self.density <= 0.0 || !self.density.is_finite() {
            return 0.0;
        }
        (self.target as f32 / (self.density * core::f32::consts::PI))
            .sqrt()
            .max(1.0)
    }

    /// The steering parameters implied by the band's geometry.
    #[must_use]
    pub fn steer_params(self) -> SteerParams {
        SteerParams {
            extent: self.extent,
            ..SteerParams::default()
        }
    }
}

/// The band's runtime state: the field, the pack, and the radii derived from config.
#[derive(Resource)]
pub struct Band {
    /// Population outside the band.
    pub field: AmbientField,
    /// The steering buffer, sized once.
    pub pack: Pack,
    /// Inside this, an entity is individually present.
    pub radius: f32,
    /// The mid band's steering buffer, run every `mid_cadence` frames.
    pub mid_pack: Pack,
    /// Frames between mid-band steering passes.
    pub mid_cadence: u32,
    /// Frames elapsed, for the mid cadence.
    pub frame: u32,
    /// Outside this, an entity folds back into density.
    pub fold_radius: f32,
    /// Manifestations and folds performed since the plugin started.
    pub manifested: u64,
    /// Folds performed since the plugin started.
    pub folded: u64,
}

impl Band {
    /// Build a band from configuration.
    ///
    /// # Errors
    ///
    /// [`FieldError`] if the derived grid exceeds the field's cell ceiling. The plugin
    /// substitutes [`Band::minimal`] rather than panicking in a game loop.
    pub fn new(config: BandConfig) -> Result<Self, FieldError> {
        let radius = config.radius();
        // 2048² is exactly AmbientField::MAX_CELLS: the largest grid a band may ask for.
        let side = ((config.extent / config.spacing).ceil() as u32).clamp(1, 2048);
        let field = AmbientField::new(side, side, config.spacing, 0x5EED_BA9D)?;
        Ok(Self {
            field,
            pack: Pack::with_capacity(config.target as usize + 1, config.steer_params()),
            radius,
            mid_pack: Pack::with_capacity(
                config.mid_target as usize + 1,
                SteerParams {
                    extent: config.mid_extent,
                    ..config.steer_params()
                },
            ),
            mid_cadence: config.mid_cadence.max(1),
            frame: 0,
            fold_radius: radius * config.fold_slack.max(1.0),
            manifested: 0,
            folded: 0,
        })
    }

    /// A degenerate band: one cell, no grid. Used when the configured geometry is
    /// impossible, so a bad config costs population rather than a panic.
    #[must_use]
    pub fn minimal(config: BandConfig) -> Self {
        let radius = config.radius();
        Self {
            field: AmbientField::minimal(config.spacing, 0x5EED_BA9D),
            pack: Pack::with_capacity(1, config.steer_params()),
            radius,
            mid_pack: Pack::with_capacity(1, config.steer_params()),
            mid_cadence: 1,
            frame: 0,
            fold_radius: radius * config.fold_slack.max(1.0),
            manifested: 0,
            folded: 0,
        }
    }

    /// Population held as density plus the agents currently manifest.
    #[must_use]
    pub fn total(&self, manifested: usize) -> u64 {
        self.field.population() + manifested as u64
    }
}

/// The band plugin. Add it after [`crate::adapters::primary::plugin::FlockPlugin`].
pub struct BandPlugin {
    config: BandConfig,
}

impl BandPlugin {
    /// A band with the given configuration.
    #[must_use]
    pub const fn new(config: BandConfig) -> Self {
        Self { config }
    }
}

impl Default for BandPlugin {
    fn default() -> Self {
        Self::new(BandConfig::default())
    }
}

/// Systems that fold, manifest and steer, in the order ADR 0020 fixes.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BandSet {
    /// Entities beyond the fold radius become density.
    Fold,
    /// Density inside the manifest radius becomes entities.
    Manifest,
    /// Copy in, steer, copy out.
    Steer,
}

impl Plugin for BandPlugin {
    fn build(&self, app: &mut App) {
        let config = self.config;
        app.insert_resource(config)
            .insert_resource(Band::new(config).unwrap_or_else(|_| Band::minimal(config)))
            .configure_sets(
                Update,
                (BandSet::Fold, BandSet::Manifest, BandSet::Steer).chain(),
            )
            .add_systems(
                Update,
                (band_fold, band_manifest, band_steer, band_steer_mid)
                    .chain()
                    .in_set(BandSet::Steer),
            );
    }
}

/// The player's position, or the origin when there is no player. The band is centred on the
/// first [`PlayerFocus`]; a game with several players needs a policy, and this is not one.
fn focus_at(focus: &Query<&Transform, With<PlayerFocus>>) -> [f32; 2] {
    focus.iter().next().map_or([0.0, 0.0], |transform| {
        [transform.translation.x, transform.translation.y]
    })
}

fn distance_squared(a: [f32; 2], b: [f32; 2]) -> f32 {
    (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)
}

/// Fold agents beyond the fold radius into density. Anchors are skipped.
pub fn band_fold(
    mut commands: Commands,
    config: Res<BandConfig>,
    mut band: ResMut<Band>,
    focus: Query<&Transform, With<PlayerFocus>>,
    agents: Query<(Entity, &Transform, &BandAgent), Without<BandAnchor>>,
) {
    let centre = focus_at(&focus);
    let limit = band.fold_radius * band.fold_radius;
    let mut budget = config.ration;

    for (entity, transform, agent) in &agents {
        if budget == 0 {
            break;
        }
        let at = [transform.translation.x, transform.translation.y];
        if distance_squared(at, centre) <= limit {
            continue;
        }
        let Some(cell) = band.field.cell_of(at) else {
            continue;
        };
        band.field.fold(cell, 1, agent.velocity, 0);
        commands.entity(entity).despawn();
        band.folded += 1;
        budget -= 1;
    }
}

/// Manifest density inside the manifest radius, rationed, taking before spawning.
pub fn band_manifest(
    mut commands: Commands,
    config: Res<BandConfig>,
    mut band: ResMut<Band>,
    focus: Query<&Transform, With<PlayerFocus>>,
    agents: Query<&Transform, With<BandAgent>>,
    clock: Option<Res<crate::application::SimClock>>,
) {
    let centre = focus_at(&focus);
    let live = agents.iter().count() as u32;
    let capacity = config.target.saturating_sub(live);
    if capacity == 0 {
        return;
    }
    let Some(centre_cell) = band.field.cell_of(centre) else {
        return;
    };
    let tick = clock.map_or(Tick::new(0), |clock| clock.tick);
    let mut budget = capacity.min(config.ration);

    // A disc over the manifest radius, centre outward: the nearest cells first, so a sudden
    // capacity is filled by the people standing closest rather than by a corner.
    //
    // Circular, not square, and that is the whole of the churn fix (ADR 0025). A square
    // window reaches `radius * sqrt(2)` into its corners, which is 1.41 radius against a
    // 1.15 radius fold bound: the corners of the window are *outside the band*, so anyone
    // manifested from them is folded again on the next tick. At the default geometry that
    // was 1,548 of 16,641 window cells, and with the ration spent on them every frame the
    // band never converged - it held 256 agents forever, manifesting and folding the same
    // 256 people.
    let radius_squared = band.radius * band.radius;
    let fold_squared = band.fold_radius * band.fold_radius;
    let reach = (band.radius / config.spacing).ceil() as i32;
    'cells: for ring in 0..=reach {
        // Rings are Chebyshev shells; once even the nearest point of a shell is beyond the
        // radius, every outer shell is too.
        if (ring as f32 - 1.0) * config.spacing > band.radius {
            break 'cells;
        }
        for dy in -ring..=ring {
            for dx in -ring..=ring {
                if ring > 0 && dx.abs() != ring && dy.abs() != ring {
                    continue;
                }
                let x = centre_cell.x as i32 + dx;
                let y = centre_cell.y as i32 + dy;
                if x < 0 || y < 0 {
                    continue;
                }
                let cell = CellId::new(x as u32, y as u32);
                // Eligibility is a property of where the person would *appear*, not of where
                // they are stored: the spawn point is the cell centre, so that is what the
                // radius has to bound.
                let at = band.field.centre_of(cell);
                if distance_squared(at, centre) > radius_squared {
                    continue;
                }
                // Belt and braces: if a caller sets `fold_slack` to 1.0 the two radii coincide
                // and an agent spawned on the boundary can be folded the moment it is steered.
                // Nobody is manifested onto ground they would immediately leave.
                if distance_squared(at, centre) >= fold_squared {
                    continue;
                }
                let available = band.field.cell(cell).map_or(0, |state| state.population());
                if available == 0 {
                    continue;
                }
                let take = available.min(budget);
                if take == 0 {
                    break 'cells;
                }
                if !band.field.take(cell, take) {
                    continue;
                }
                let centre_of_cell = at;
                let velocity = band.field.cell(cell).map_or([0.0, 0.0], |c| c.velocity());
                for id in band.field.manifest(cell, take, tick) {
                    commands.spawn((
                        crate::adapters::primary::components::Npc(id),
                        BandAgent { velocity },
                        Transform::from_xyz(centre_of_cell[0], centre_of_cell[1], 0.0),
                    ));
                    band.manifested += 1;
                }
                budget -= take;
                if budget == 0 {
                    break 'cells;
                }
            }
        }
    }
}

/// Copy the band into the pack, steer it, and copy the result back.
pub fn band_steer(
    mut band: ResMut<Band>,
    config: Res<BandConfig>,
    mut agents: Query<(&mut Transform, &mut BandAgent), Without<BandAnchor>>,
) {
    if agents.is_empty() {
        return;
    }
    band.pack.clear();
    for (transform, agent) in &agents {
        band.pack.push(
            [transform.translation.x, transform.translation.y],
            agent.velocity,
        );
    }

    let params = config.steer_params();
    let _ = band.pack.steer(&params);
    band.pack.integrate(params.max_speed);
    band.pack.advance(1.0 / 60.0, params.extent);

    for (index, (mut transform, mut agent)) in agents.iter_mut().enumerate() {
        let Some(position) = band.pack.position(index) else {
            continue;
        };
        let velocity = band.pack.velocity(index).unwrap_or(agent.velocity);
        transform.translation.x = position[0];
        transform.translation.y = position[1];
        agent.velocity = velocity;
    }
}

/// Steer the mid band every `mid_cadence` frames. Same kernel, a third of the rate.
pub fn band_steer_mid(
    mut band: ResMut<Band>,
    config: Res<BandConfig>,
    mut mids: Query<(&mut Transform, &mut MidAgent), Without<BandAnchor>>,
) {
    band.frame = band.frame.wrapping_add(1);
    if !band.frame.is_multiple_of(band.mid_cadence) || mids.is_empty() {
        return;
    }
    band.mid_pack.clear();
    for (transform, mid) in &mids {
        band.mid_pack.push(
            [transform.translation.x, transform.translation.y],
            mid.velocity,
        );
    }
    let params = SteerParams {
        extent: config.mid_extent,
        ..config.steer_params()
    };
    let _ = band.mid_pack.steer(&params);
    let dt = 1.0 / 60.0 * band.mid_cadence as f32;
    band.mid_pack.integrate(params.max_speed);
    band.mid_pack.advance(dt, params.extent);
    for (index, (mut transform, mut mid)) in mids.iter_mut().enumerate() {
        if let Some(position) = band.mid_pack.position(index) {
            transform.translation.x = position[0];
            transform.translation.y = position[1];
        }
        if let Some(velocity) = band.mid_pack.velocity(index) {
            mid.velocity = velocity;
        }
    }
}

/// A convenience for games: seed the field from a population that is not yet spawned.
///
/// Fills the field **uniformly**, one or more people per cell, by Bresenham distribution over
/// the cell indices rather than by filling cells until the count runs out. The earlier version
/// walked rows from the origin and stopped when it was spent, which put a whole 40,000-person
/// town in the first hundred and fifty rows of a 256-row field: the focus at the centre stood
/// in empty country, the manifest reached outward for people it should never have touched, and
/// every one of them was folded on the next tick. A seeding utility that puts the population
/// somewhere other than the field is a fixture that measures itself.
pub fn seed_field(band: &mut Band, count: u32) {
    let width = band.field.width();
    let height = band.field.height();
    let cells = u64::from(width) * u64::from(height);
    if cells == 0 {
        return;
    }
    let total = u64::from(count);
    let base = total / cells;
    let remainder = total % cells;

    for y in 0..height {
        for x in 0..width {
            let index = u64::from(y) * u64::from(width) + u64::from(x);
            // `index` gets one of the `remainder` extra people when the running share crosses
            // an integer boundary between this cell and the next: evenly spaced, no allocation,
            // and reproducible for a given count and geometry.
            let extra = index * remainder / cells != (index + 1) * remainder / cells;
            let here = u32::try_from(base + u64::from(extra)).unwrap_or(u32::MAX);
            if here == 0 {
                continue;
            }
            let Some(cell) = band.field.cell_mut(CellId::new(x, y)) else {
                continue;
            };
            cell.absorb(here, [0.4, 0.0], 0, Occupation::default());
        }
    }
}

/// Whether an identifier is present as an entity. Used by the fold-completeness test.
pub fn is_manifested(
    agents: &Query<&crate::adapters::primary::components::Npc>,
    id: NpcId,
) -> bool {
    agents.iter().any(|npc| npc.0 == id)
}
