//! A render plan: what the crowd looks like, decided once, drawn cheaply.
//!
//! This crate does not draw anything. What it does is answer the question every
//! renderer asks a crowd: *which instances, in what order, with which variant* —
//! and answer it into a buffer that is reused every frame.
//!
//! Two decisions matter here and both are about hardware rather than taste:
//!
//! * **Sorted by variant.** Draw calls are per material, not per instance, so the
//!   plan groups instances by visual variant. A crowd drawn in arbitrary order is a
//!   thousand draw calls; the same crowd sorted is a handful.
//! * **Culled by tier and distance.** Ambient NPCs are not drawn at all: they are
//!   the ones the band system is already simulating statistically, and drawing them
//!   would cost more than computing them.
//!
//! The plan is a resource with reserved capacity, cleared rather than reallocated,
//! so a steady-state frame does not allocate. That property is tested, because it
//! is the kind of thing that silently regresses when someone adds a `collect()`.

use crate::adapters::primary::components::{LodTier, Npc, NpcIdentity};
use crate::domain::ids::NpcId;
use crate::domain::num;
use bevy_ecs::prelude::*;
use bevy_math::Vec3;
use bevy_transform::prelude::Transform;

/// One instance the renderer should draw.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VisibleInstance {
    /// Which NPC.
    pub npc: NpcId,
    /// Where, in world space.
    pub position: Vec3,
    /// Facing, in radians around the up axis.
    pub yaw: f32,
    /// A per-instance tint a renderer can multiply into its material.
    pub tint: [f32; 4],
    /// The visual variant, used for batching. Instances are sorted by this.
    pub variant: u16,
}

/// How the crowd should be drawn.
#[derive(Resource, Debug, Clone, Copy, PartialEq)]
pub struct CrowdView {
    /// Where the camera is.
    pub position: Vec3,
    /// Beyond this distance nothing is drawn.
    pub cull_distance: f32,
    /// The most instances to emit, regardless of how many are in range.
    pub max_instances: usize,
}

impl Default for CrowdView {
    fn default() -> Self {
        Self {
            position: Vec3::ZERO,
            cull_distance: 120.0,
            max_instances: 8_192,
        }
    }
}

/// The plan itself.
#[derive(Resource, Debug, Default)]
pub struct RenderPlan {
    instances: Vec<VisibleInstance>,
    draw_calls: usize,
    culled: u32,
    truncated: u32,
}

impl RenderPlan {
    /// A plan with room for `capacity` instances.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            instances: Vec::with_capacity(capacity),
            draw_calls: 0,
            culled: 0,
            truncated: 0,
        }
    }

    /// The instances to draw, sorted by variant.
    #[must_use]
    pub fn instances(&self) -> &[VisibleInstance] {
        &self.instances
    }

    /// How many distinct variants are in the plan: the number of draw calls a
    /// renderer that batches by variant will issue.
    #[must_use]
    pub const fn draw_calls(&self) -> usize {
        self.draw_calls
    }

    /// How many NPCs were culled by tier or distance.
    #[must_use]
    pub const fn culled(&self) -> u32 {
        self.culled
    }

    /// How many in-range NPCs were dropped because the plan was full.
    #[must_use]
    pub const fn truncated(&self) -> u32 {
        self.truncated
    }

    /// The buffer's capacity, which must not grow from frame to frame.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.instances.capacity()
    }

    /// How many instances the plan holds.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.instances.len()
    }

    /// Whether the plan is empty.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.instances.is_empty()
    }

    /// Reserve room up front.
    pub fn reserve(&mut self, capacity: usize) {
        if capacity > self.instances.capacity() {
            self.instances.reserve(capacity - self.instances.len());
        }
    }
}

/// A deterministic tint from an appearance seed.
///
/// Cheap on purpose: three integer mixes and a normalisation. A renderer is free to
/// ignore it, but a crowd with per-person colour reads as people rather than clones
/// for almost no cost.
#[must_use]
pub fn tint_from_seed(seed: u64) -> [f32; 4] {
    let mixed = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let r = num::u64_to_f32((mixed >> 8) & 0xFF) / 255.0;
    let g = num::u64_to_f32((mixed >> 24) & 0xFF) / 255.0;
    let b = num::u64_to_f32((mixed >> 40) & 0xFF) / 255.0;
    // Bias toward mid tones so the crowd is not neon.
    [0.35 + r * 0.5, 0.35 + g * 0.5, 0.35 + b * 0.5, 1.0]
}

/// Build the plan for this frame.
///
/// Ordering is `(variant, npc)` so the result is stable across runs and across
/// frame-to-frame iteration order changes in the ECS.
#[expect(
    clippy::needless_pass_by_value,
    reason = "Bevy hands a system its parameters by value; `&Res<T>` is not a `SystemParam`"
)]
pub fn build_render_plan(
    view: Res<CrowdView>,
    mut plan: ResMut<RenderPlan>,
    npcs: Query<(&Npc, &Transform, &LodTier, Option<&NpcIdentity>)>,
) {
    plan.instances.clear();
    plan.culled = 0;
    plan.truncated = 0;
    let limit = view.max_instances;
    let cull_squared = view.cull_distance.max(0.0) * view.cull_distance.max(0.0);
    for (npc, transform, tier, identity) in &npcs {
        if *tier == LodTier::Ambient {
            plan.culled = plan.culled.saturating_add(1);
            continue;
        }
        let delta = transform.translation - view.position;
        if delta.length_squared() > cull_squared {
            plan.culled = plan.culled.saturating_add(1);
            continue;
        }
        if plan.instances.len() >= limit {
            plan.truncated = plan.truncated.saturating_add(1);
            continue;
        }
        let seed = identity.map_or(0, |identity| identity.appearance_seed);
        let yaw = transform.rotation.to_euler(bevy_math::EulerRot::YXZ).0;
        plan.instances.push(VisibleInstance {
            npc: npc.0,
            position: transform.translation,
            yaw,
            tint: tint_from_seed(seed),
            variant: u16::try_from(seed % u64::from(u16::MAX)).unwrap_or(0),
        });
    }
    plan.instances
        .sort_unstable_by_key(|instance| (instance.variant, instance.npc));
    // Count distinct variants without allocating: the slice is sorted, so a run
    // count is the number of draw calls.
    let mut draw_calls = 0;
    let mut previous: Option<u16> = None;
    for instance in &plan.instances {
        if previous != Some(instance.variant) {
            draw_calls += 1;
            previous = Some(instance.variant);
        }
    }
    plan.draw_calls = draw_calls;
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_app::{App, Update};

    fn app() -> App {
        let mut app = App::new();
        app.init_resource::<CrowdView>()
            .init_resource::<RenderPlan>()
            .add_systems(Update, build_render_plan);
        app
    }

    fn spawn(app: &mut App, id: u32, position: Vec3, tier: LodTier, seed: u64) {
        app.world_mut().spawn((
            Npc(NpcId::unique(id)),
            Transform::from_translation(position),
            tier,
            NpcIdentity {
                archetype: crate::domain::temperament::Archetype::Wanderer,
                appearance_seed: seed,
            },
        ));
    }

    #[test]
    fn the_plan_holds_what_is_in_range_and_not_what_is_not() {
        let mut app = app();
        spawn(&mut app, 1, Vec3::ZERO, LodTier::Near, 1);
        spawn(&mut app, 2, Vec3::new(10_000.0, 0.0, 0.0), LodTier::Near, 2);
        app.update();
        let plan = app.world().resource::<RenderPlan>();
        assert_eq!(plan.len(), 1);
        assert_eq!(plan.culled(), 1);
    }

    #[test]
    fn ambient_npcs_are_never_drawn() {
        let mut app = app();
        spawn(&mut app, 1, Vec3::ZERO, LodTier::Ambient, 3);
        app.update();
        assert!(app.world().resource::<RenderPlan>().is_empty());
    }

    #[test]
    fn instances_are_sorted_by_variant_so_batching_works() {
        let mut app = app();
        for id in 0..32_u32 {
            // Seeds chosen so variants interleave in entity order.
            spawn(
                &mut app,
                id,
                Vec3::ZERO,
                LodTier::Near,
                u64::from(id * 7 % 5),
            );
        }
        app.update();
        let plan = app.world().resource::<RenderPlan>();
        let variants: Vec<u16> = plan.instances().iter().map(|i| i.variant).collect();
        let mut sorted = variants.clone();
        sorted.sort_unstable();
        assert_eq!(variants, sorted);
        assert!(plan.draw_calls() <= plan.len());
        assert!(plan.draw_calls() >= 1);
    }

    #[test]
    fn the_plan_is_bounded_and_says_how_much_it_dropped() {
        let mut app = app();
        app.world_mut().insert_resource(CrowdView {
            max_instances: 4,
            ..CrowdView::default()
        });
        for id in 0..10_u32 {
            spawn(&mut app, id, Vec3::ZERO, LodTier::Near, u64::from(id));
        }
        app.update();
        let plan = app.world().resource::<RenderPlan>();
        assert_eq!(plan.len(), 4);
        assert_eq!(plan.truncated(), 6);
    }

    #[test]
    fn a_steady_state_frame_does_not_reallocate() {
        let mut app = app();
        for id in 0..64_u32 {
            spawn(&mut app, id, Vec3::ZERO, LodTier::Near, u64::from(id));
        }
        app.update();
        let capacity = app.world().resource::<RenderPlan>().capacity();
        for _ in 0..5 {
            app.update();
            assert_eq!(app.world().resource::<RenderPlan>().capacity(), capacity);
        }
    }

    #[test]
    fn the_same_world_produces_the_same_plan() {
        let plan = || {
            let mut app = app();
            for id in 0..9_u32 {
                spawn(
                    &mut app,
                    id,
                    Vec3::new(num::u32_to_f32(id), 0.0, 0.0),
                    LodTier::Mid,
                    u64::from(id),
                );
            }
            app.update();
            app.world().resource::<RenderPlan>().instances().to_vec()
        };
        assert_eq!(plan(), plan());
    }

    #[test]
    fn a_tint_is_mid_tone_and_deterministic() {
        let first = tint_from_seed(42);
        // The tint is a pure function of the seed, so the comparison is exact by intent:
        // two runs must agree bit for bit, not merely within a tolerance.
        assert_eq!(
            first.map(f32::to_bits),
            tint_from_seed(42).map(f32::to_bits)
        );
        assert!((0.3..=0.9).contains(&first[0]));
        assert_eq!(first[3].to_bits(), 1.0_f32.to_bits());
    }
}
