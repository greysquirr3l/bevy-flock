//! Everything the binding remembers about one body, in one component.
//!
//! Three facts travel together and are written by the same pass: which LOD level the body was last
//! evaluated at, the pose it is actually drawn in while a start or stop window eases it, and the foot
//! slide measured this tick. Keeping them in one component means a body is inserted into one
//! archetype when the binding first sees it, the query stays readable, and a game that wants to read
//! any of it has one name to look up.
use bevy_ecs::prelude::Component;

use crate::anim::lod::AnimationLod;
use crate::anim::settle::Settle;
use crate::anim::slide::FootSlide;

/// What the binding has done to one body, as of the last tick it was evaluated.
#[derive(Component, Debug, Clone, Copy, Default, PartialEq)]
pub struct AnimationState {
    lod: AnimationLod,
    settle: Settle,
    slide: FootSlide,
}

impl AnimationState {}

impl AnimationState {
    /// The LOD level and the counters behind it.
    #[must_use]
    pub const fn lod(&self) -> AnimationLod {
        self.lod
    }

    /// The pose the body is drawn in, eased by its start and stop window.
    #[must_use]
    pub const fn settle(&self) -> Settle {
        self.settle
    }

    /// The slide measured so far.
    #[must_use]
    pub const fn slide(&self) -> FootSlide {
        self.slide
    }

    /// The LOD counters, for the binding pass to advance.
    pub(crate) const fn lod_mut(&mut self) -> &mut AnimationLod {
        &mut self.lod
    }

    /// The settle ease, for the binding pass to advance.
    pub(crate) const fn settle_mut(&mut self) -> &mut Settle {
        &mut self.settle
    }

    /// The slide measurement, for the binding pass to advance.
    pub(crate) const fn slide_mut(&mut self) -> &mut FootSlide {
        &mut self.slide
    }
}

#[cfg(test)]
mod tests {
    use super::AnimationState;
    use crate::anim::lod::LodLevel;

    #[test]
    fn a_fresh_state_is_full_weight_and_has_measured_nothing() {
        let state = AnimationState::default();
        assert_eq!(state.lod().level(), LodLevel::Full);
        assert_eq!(state.lod().evaluations(), 0);
        assert_eq!(state.slide().ticks(), 0);
        assert!(state.slide().total_metres().abs() < f32::EPSILON);
    }
}
