//! Components that hold simulation state on an entity.
//!
//! Components are thin, `Copy` where the payload allows, and hold no behaviour.
//! Every one of them documents who writes it and when, because "which system owns
//! this field" is the question that decides whether a crowd simulation is
//! debuggable or a mystery.

use std::sync::Arc;

use bevy_ecs::prelude::*;
use bevy_math::Vec3;
use bevy_transform::prelude::Transform;

use crate::behavior::blackboard::Blackboard;
use crate::behavior::tree::{BehaviorTree, LeafDispatch, Status, TreeCursor};
use crate::domain::cost::VirtualCost;
use crate::domain::ids::NpcId;
use crate::domain::temperament::{Archetype, Mood, Needs, Temperament};
use crate::domain::tick::Tick;

/// An NPC. The identifier is the entity's identity for RNG, tie-breaks and
/// snapshots.
///
/// Written once at spawn; never changed. If it changes, every stored replay is
/// invalid, which is why no system takes `&mut Npc`.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Npc(pub NpcId);

/// What kind of person this is, and the seed that determines how they look.
///
/// Written at spawn. The appearance seed is deliberately opaque: the crate does
/// not know what a mesh or a sprite is, so it hands the renderer a number and
/// lets the game decide what that number means.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct NpcIdentity {
    /// The archetype this NPC was generated from.
    pub archetype: Archetype,
    /// A stable seed a renderer can hash into a palette, silhouette or material.
    pub appearance_seed: u64,
}

/// Personality. Written at spawn; a later task adds slow drift over a lifetime.
#[derive(Component, Debug, Clone, Copy, PartialEq)]
pub struct NpcTemperament(pub Temperament);

/// Drives. Written by whichever system satisfies or decays them.
#[derive(Component, Debug, Clone, Copy, PartialEq)]
pub struct NpcNeeds(pub Needs);

/// Emotional state. Written by the mood system; read by utility scoring.
#[derive(Component, Debug, Clone, Copy, PartialEq)]
pub struct NpcMood(pub Mood);

/// Current velocity, in metres per second.
///
/// Written by integration; read by steering and by the spatial index.
#[derive(Component, Debug, Clone, Copy, PartialEq, Default)]
pub struct NpcVelocity(pub Vec3);

/// The velocity steering wants this tick.
///
/// Written by steering; read by integration. Kept separate from
/// [`NpcVelocity`] so acceleration can be limited: a crowd that can change
/// direction instantly reads as a swarm of insects, not as people.
#[derive(Component, Debug, Clone, Copy, PartialEq, Default)]
pub struct NpcDesiredVelocity(pub Vec3);

/// How much this NPC yields to others in the reciprocal avoidance pass (ADR 0045).
///
/// 1.0 is an ordinary person and the value every NPC gets for free: without the component, the
/// crowd's configured default applies. Raise it for someone who should push through — a guard
/// walking a beat, a cart — and lower it for someone who steps aside — a fleeing villager, a
/// child. Zero would make an NPC ignore the pass entirely, which is why it is clamped to a
/// positive floor rather than stored raw.
#[derive(Component, Debug, Clone, Copy, PartialEq)]
pub struct NpcAvoidancePriority(pub f32);

impl NpcAvoidancePriority {
    /// The value an agent gets when nobody says otherwise.
    pub const DEFAULT: f32 = 1.0;

    /// A priority, clamped to a range where the solve still means something.
    ///
    /// A non-finite or non-positive value falls back to [`Self::DEFAULT`] rather than producing
    /// an agent that can never be avoided: this is a per-NPC authoring knob, and a bad one should
    /// cost the author their intent, not the crowd its separation.
    #[must_use]
    pub fn new(priority: f32) -> Self {
        Self::try_new(priority).unwrap_or(Self(Self::DEFAULT))
    }

    /// A priority, or `None` when the value is not usable.
    #[must_use]
    pub fn try_new(priority: f32) -> Option<Self> {
        if priority.is_finite() && (Self::FLOOR..=Self::CEILING).contains(&priority) {
            Some(Self(priority))
        } else {
            None
        }
    }

    /// The lowest priority accepted.
    pub const FLOOR: f32 = 0.05;
    /// The highest priority accepted.
    pub const CEILING: f32 = 20.0;

    /// The value, for the steering kernel.
    #[must_use]
    pub const fn get(self) -> f32 {
        self.0
    }
}

impl Default for NpcAvoidancePriority {
    fn default() -> Self {
        Self(Self::DEFAULT)
    }
}

/// Where this NPC is trying to get to.
///
/// Optional: an NPC with no target flocks with its neighbours instead of heading
/// anywhere, which is what makes ambient crowds look aimless in the right way.
#[derive(Component, Debug, Clone, Copy, PartialEq)]
pub struct NpcTarget(pub Vec3);

/// How much detail this NPC's simulation gets.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum LodTier {
    /// Full cognition every tick, full steering, rendered.
    #[default]
    Near,
    /// Cognition every few ticks, steering every tick, rendered.
    Mid,
    /// Cognition rarely, steering at reduced fidelity, rendered as a billboard.
    Far,
    /// No per-entity cognition: part of the ambient field.
    Ambient,
}

impl LodTier {
    /// Every tier, most detailed first.
    pub const ALL: [Self; 4] = [Self::Near, Self::Mid, Self::Far, Self::Ambient];

    /// A lowercase name for debug output and snapshots.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Near => "near",
            Self::Mid => "mid",
            Self::Far => "far",
            Self::Ambient => "ambient",
        }
    }

    /// Index into a per-tier array.
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Near => 0,
            Self::Mid => 1,
            Self::Far => 2,
            Self::Ambient => 3,
        }
    }
}

/// Which flock or group this NPC belongs to.
///
/// Optional: an NPC with no membership still steers, it just has no group
/// blackboard and no director issuing it orders.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlockMember {
    /// The group identifier.
    pub group: crate::domain::ids::GroupId,
}

/// An NPC's compiled behaviour tree and where it currently is in it.
///
/// The tree is behind an `Arc` because it is shared: ten thousand guards run one
/// compiled tree, not ten thousand copies of it. Only the [`TreeCursor`] is
/// per-NPC, and it is bounded by tree depth rather than tree size.
///
/// [`NpcBrain::cost`] is the declared cost of one think, read by the think
/// scheduler when it decides who gets the budget (ADR 0009). It is cached here
/// rather than recomputed because the scheduler asks this question every tick and
/// the answer never changes.
#[derive(Component, Debug, Clone)]
pub struct NpcBrain {
    tree: Arc<BehaviorTree>,
    cursor: TreeCursor,
    cost: VirtualCost,
}

impl NpcBrain {
    /// Wrap a compiled tree.
    #[must_use]
    pub fn new(tree: Arc<BehaviorTree>, cost: VirtualCost) -> Self {
        Self {
            tree,
            cursor: TreeCursor::new(),
            cost,
        }
    }

    /// The tree this NPC thinks with.
    #[must_use]
    pub fn tree(&self) -> &BehaviorTree {
        &self.tree
    }

    /// The declared cost of one think.
    #[must_use]
    pub const fn cost(&self) -> VirtualCost {
        self.cost
    }

    /// Where in the tree this NPC currently is.
    #[must_use]
    pub const fn cursor(&self) -> &TreeCursor {
        &self.cursor
    }

    /// Evaluate one think.
    ///
    /// Called by whichever scheduler owns the budget; this component does not
    /// decide *when* it runs, only *what* happens when it does.
    pub fn tick(
        &mut self,
        dispatch: &mut dyn LeafDispatch,
        blackboard: &mut Blackboard,
        tick: Tick,
        npc: NpcId,
    ) -> Status {
        let mut ctx = crate::behavior::tree::LeafContext {
            blackboard,
            tick,
            npc,
        };
        self.tree.tick(&mut self.cursor, dispatch, &mut ctx)
    }

    /// Abandon the current behaviour and release whatever it holds.
    pub fn abort(
        &mut self,
        dispatch: &mut dyn LeafDispatch,
        blackboard: &mut Blackboard,
        tick: Tick,
        npc: NpcId,
    ) {
        let mut ctx = crate::behavior::tree::LeafContext {
            blackboard,
            tick,
            npc,
        };
        self.tree.abort(&mut self.cursor, dispatch, &mut ctx);
    }
}

/// A bundle for the minimum viable NPC.
///
/// Games that want their own components can spawn these five by hand; the bundle
/// exists so the quickstart does not need a paragraph of explanation.
#[derive(Bundle, Debug, Clone, Copy)]
pub struct NpcBundle {
    /// The NPC marker.
    pub npc: Npc,
    /// Transform, shared with the rest of Bevy.
    pub transform: Transform,
    /// Identity and appearance.
    pub identity: NpcIdentity,
    /// Personality.
    pub temperament: NpcTemperament,
    /// Drives: energy, satiety, company, safety.
    ///
    /// Part of the bundle rather than an optional extra: a need that starts missing cannot decay,
    /// and the temperament drive reads it every frame.
    pub needs: NpcNeeds,
    /// Current velocity.
    pub velocity: NpcVelocity,
    /// Desired velocity.
    pub desired: NpcDesiredVelocity,
    /// Detail tier.
    pub lod: LodTier,
}

impl NpcBundle {
    /// A complete NPC with neutral personality at the origin.
    ///
    /// The five-line quickstart path: a caller that has an [`NpcId`] has
    /// everything else this needs.
    #[must_use]
    pub fn default_for(id: NpcId) -> Self {
        Self {
            npc: Npc(id),
            transform: Transform::from_translation(Vec3::ZERO),
            identity: NpcIdentity {
                archetype: Archetype::Wanderer,
                appearance_seed: u64::from(id.to_bits()),
            },
            temperament: NpcTemperament(Temperament::balanced()),
            needs: NpcNeeds(Needs::satisfied()),
            velocity: NpcVelocity(Vec3::ZERO),
            desired: NpcDesiredVelocity(Vec3::ZERO),
            lod: LodTier::Near,
        }
    }

    /// Place the NPC.
    #[must_use]
    pub fn with_position(mut self, position: Vec3) -> Self {
        self.transform.translation = position;
        self
    }

    /// Give the NPC a personality.
    #[must_use]
    pub const fn with_temperament(mut self, temperament: Temperament) -> Self {
        self.temperament = NpcTemperament(temperament);
        self
    }

    /// Give the NPC a detail tier.
    #[must_use]
    pub const fn with_lod(mut self, tier: LodTier) -> Self {
        self.lod = tier;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lod_tiers_have_distinct_indices_and_names() {
        for (expected, tier) in LodTier::ALL.iter().enumerate() {
            assert_eq!(tier.index(), expected);
            assert_ne!(tier.as_str(), "");
        }
    }

    #[test]
    fn lod_tier_orders_from_most_to_least_detailed() {
        assert!(LodTier::Near < LodTier::Mid);
        assert!(LodTier::Mid < LodTier::Far);
        assert!(LodTier::Far < LodTier::Ambient);
    }

    #[test]
    fn npc_bundle_can_be_spawned_into_a_world() {
        let mut world = World::new();
        let entity = world
            .spawn(NpcBundle {
                npc: Npc(NpcId::unique(0)),
                transform: Transform::from_xyz(1.0, 0.0, 2.0),
                identity: NpcIdentity {
                    archetype: Archetype::Merchant,
                    appearance_seed: 7,
                },
                temperament: NpcTemperament(Temperament::balanced()),
                needs: NpcNeeds(Needs::satisfied()),
                velocity: NpcVelocity(Vec3::ZERO),
                desired: NpcDesiredVelocity(Vec3::ZERO),
                lod: LodTier::Near,
            })
            .id();
        assert!(world.get::<Npc>(entity).is_some());
    }
}
