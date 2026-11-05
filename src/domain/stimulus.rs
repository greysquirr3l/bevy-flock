//! What an NPC can see and hear, decided from its own position (ADR 0047).
//!
//! Behaviour trees and utility scorers both want the same question answered: *what is happening
//! near me, and how much does it matter?* Answering it per NPC per tick from the entity list is
//! the classic crowd cost — a hundred agents each walking a thousand others. This module answers
//! it from a bounded, decaying buffer instead:
//!
//! * **Emitters publish, observers read.** A sighting, a shout, a smell is pushed once, with a
//!   position, a radius and a lifetime. Everyone in range reads the same record.
//! * **The buffer is bounded and old records die.** Pushing into a full buffer overwrites the
//!   oldest, so a noisy frame cannot grow memory, and `ttl` retires anything nobody refreshed.
//! * **Perception is deterministic.** Records are examined in ring order and the result is sorted
//!   by distance then kind, never by hash order or by floating-point luck. Two runs of the same
//!   world see the same list.
//! * **Occlusion is a hook, not a dependency.** [`Occluder`] is one trait with one method; the
//!   default [`OpenAir`] blocks nothing, so a game without physics pays nothing, and a game with
//!   a physics query passes it in without this module knowing what a collider is.
//!
//! The ECS half lives in `adapters::primary::stimulus`.

use bevy_math::Vec3;
use smallvec::SmallVec;

use super::tick::Tick;

/// What kind of thing was perceived.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum StimulusKind {
    /// Something visible: a player, a body, a light.
    #[default]
    Sight,
    /// Something audible: a shout, a footstep, an explosion.
    Sound,
    /// Something smelled: blood, smoke, perfume.
    Smell,
    /// A game-defined channel. Values below 1,024 are reserved for future built-ins.
    Custom(u16),
}

impl StimulusKind {
    /// Whether this kind is blocked by walls.
    ///
    /// Sound and smell go round corners; light does not. A `Custom` channel is treated as
    /// omni-directional and unoccluded, because a game that invented it knows better than this
    /// module does.
    #[must_use]
    pub const fn is_occluded(self) -> bool {
        matches!(self, Self::Sight)
    }

    /// Whether this kind is narrowed by the observer's field of view.
    #[must_use]
    pub const fn uses_field_of_view(self) -> bool {
        matches!(self, Self::Sight)
    }
}

/// One thing that happened, in a form every observer can read.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Stimulus {
    /// What it is.
    pub kind: StimulusKind,
    /// Where it is.
    pub position: Vec3,
    /// How far it carries, in metres.
    pub radius: f32,
    /// How strong it is at the source, 0.0 to 1.0.
    pub intensity: f32,
    /// The tick it was published.
    pub emitted: Tick,
    /// How many ticks it stays interesting for.
    pub ttl: u32,
}

impl Stimulus {
    /// A stimulus that lives for one second at 60 Hz.
    #[must_use]
    pub fn new(
        kind: StimulusKind,
        position: Vec3,
        radius: f32,
        intensity: f32,
        emitted: Tick,
    ) -> Self {
        Self {
            kind,
            position,
            radius: sanitise(radius, 1.0),
            intensity: sanitise(intensity, 1.0).min(1.0),
            emitted,
            ttl: 60,
        }
    }

    /// Change the lifetime, in ticks.
    #[must_use]
    pub const fn with_ttl(mut self, ttl: u32) -> Self {
        self.ttl = ttl;
        self
    }

    /// How many ticks old this is at `now`.
    #[must_use]
    pub fn age(self, now: Tick) -> u32 {
        u32::try_from(now.since(self.emitted)).unwrap_or(u32::MAX)
    }

    /// Whether this is still worth perceiving at `now`.
    #[must_use]
    pub fn is_live(self, now: Tick) -> bool {
        self.ttl > 0 && self.age(now) <= self.ttl
    }

    /// How much of its intensity survives, given the observer's distance.
    ///
    /// Linear falloff to zero at the radius, multiplied by the age curve for anything that
    /// carries — a shout heard three seconds ago is a rumour, not a location.
    #[must_use]
    pub fn strength_at(self, observer: Vec3, now: Tick) -> f32 {
        self.strength_at_distance(observer.distance(self.position), now)
    }

    /// The same, with the distance already known.
    ///
    /// Perception computes the distance once to reject out-of-range records, so handing it back
    /// here saves the second square root per record per observer — which at 4,096 records and
    /// 512 observers is two million square roots a pass.
    #[must_use]
    pub fn strength_at_distance(self, distance: f32, now: Tick) -> f32 {
        if !self.is_live(now) || self.radius <= 0.0 || distance > self.radius {
            return 0.0;
        }
        let distance_falloff = 1.0 - (distance / self.radius);
        #[allow(clippy::cast_precision_loss)]
        let age_falloff = 1.0 - (self.age(now) as f32 / self.ttl.max(1) as f32);
        (self.intensity * distance_falloff * age_falloff).clamp(0.0, 1.0)
    }
}

fn sanitise(value: f32, fallback: f32) -> f32 {
    if value.is_finite() && value > 0.0 {
        value
    } else {
        fallback
    }
}

/// Decides whether something is in the way.
///
/// One method, because that is the entire contract a physics engine needs to satisfy. The
/// implementation is free to raycast, sample a heightmap or answer from a precomputed
/// visibility set; this module only ever asks.
pub trait Occluder {
    /// Whether the straight line from `from` to `to` is blocked.
    fn blocks(&self, from: Vec3, to: Vec3) -> bool;
}

/// Nothing is ever in the way.
#[derive(Debug, Clone, Copy, Default)]
pub struct OpenAir;

impl Occluder for OpenAir {
    fn blocks(&self, _from: Vec3, _to: Vec3) -> bool {
        false
    }
}

/// The bounded, decaying list of things that happened.
///
/// A ring buffer: `push` on a full buffer overwrites the oldest record, which is the one an
/// observer is least likely to still care about. Capacity is fixed at construction so a frame
/// never allocates.
#[derive(Debug, Clone)]
pub struct StimulusBuffer {
    items: Vec<Stimulus>,
    cursor: usize,
    capacity: usize,
}

impl StimulusBuffer {
    /// A buffer that holds `capacity` records.
    ///
    /// # Errors
    ///
    /// Returns [`StimulusError::Empty`] when the capacity is zero: a perception buffer that can
    /// hold nothing is a configuration mistake, not a valid crowd.
    pub fn new(capacity: usize) -> Result<Self, StimulusError> {
        if capacity == 0 {
            return Err(StimulusError::Empty);
        }
        Ok(Self {
            items: Vec::with_capacity(capacity),
            cursor: 0,
            capacity,
        })
    }

    /// How many records the buffer can hold.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// How many records are held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Whether the buffer is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Publish a stimulus, overwriting the oldest record when full.
    ///
    /// Returns whether a live record was overwritten, which is the signal a game wants if it can
    /// afford to grow the buffer instead.
    pub fn push(&mut self, stimulus: Stimulus) -> bool {
        if self.items.len() < self.capacity {
            self.items.push(stimulus);
            if self.items.len() == self.capacity {
                self.cursor = 0;
            }
            return false;
        }
        let overwritten = self.items.get(self.cursor).is_some_and(|old| old.ttl > 0);
        if let Some(slot) = self.items.get_mut(self.cursor) {
            *slot = stimulus;
        }
        self.cursor = (self.cursor + 1) % self.capacity;
        overwritten
    }

    /// Drop records that have expired. Returns how many were dropped.
    ///
    /// Order is preserved, so the deterministic ring order survives a sweep.
    pub fn retain_live(&mut self, now: Tick) -> usize {
        let before = self.items.len();
        self.items.retain(|item| item.is_live(now));
        self.cursor = 0;
        before - self.items.len()
    }

    /// Forget everything.
    pub fn clear(&mut self) {
        self.items.clear();
        self.cursor = 0;
    }

    /// The records, oldest first.
    pub fn iter(&self) -> impl Iterator<Item = &Stimulus> {
        self.items.iter()
    }
}

/// Why a buffer could not be built.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum StimulusError {
    /// A capacity of zero.
    #[error("a stimulus buffer needs room for at least one record")]
    Empty,
}

/// How far an observer senses, and how much it can hold in mind at once.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PerceptionQuery {
    /// How far sight reaches, in metres.
    pub sight_radius: f32,
    /// How far hearing reaches. Usually larger than sight.
    pub hearing_radius: f32,
    /// The cosine of the half-angle of the sight cone. -1.0 sees everything.
    pub field_of_view: f32,
    /// The most stimuli one observer keeps. The nearest win.
    pub max_results: usize,
}

impl PerceptionQuery {
    /// Human-ish defaults: 30 m of sight, 45 m of hearing, a 120° cone, six slots.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            sight_radius: 30.0,
            hearing_radius: 45.0,
            field_of_view: 0.5,
            max_results: 6,
        }
    }

    /// Set both ranges.
    #[must_use]
    pub const fn with_ranges(mut self, sight: f32, hearing: f32) -> Self {
        self.sight_radius = sight;
        self.hearing_radius = hearing;
        self
    }

    /// Set the cosine of the sight cone's half-angle.
    #[must_use]
    pub const fn with_field_of_view(mut self, cosine: f32) -> Self {
        self.field_of_view = cosine;
        self
    }

    /// Set how many results one observer keeps.
    #[must_use]
    pub const fn with_max_results(mut self, max: usize) -> Self {
        self.max_results = max;
        self
    }
}

impl Default for PerceptionQuery {
    fn default() -> Self {
        Self::new()
    }
}

/// One thing an observer perceived, with the strength it arrived at.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Perceived {
    /// What it is.
    pub kind: StimulusKind,
    /// Where it came from.
    pub position: Vec3,
    /// How far away it was.
    pub distance: f32,
    /// How strong it arrived, after distance, age and occlusion.
    pub strength: f32,
}

/// What `observer` can perceive right now.
///
/// The results are ordered by descending strength, then by kind for a stable tie-break, then by
/// position, so two identical worlds produce identical lists. At most
/// [`PerceptionQuery::max_results`] are returned.
#[must_use]
pub fn perceive(
    buffer: &StimulusBuffer,
    observer: Vec3,
    forward: Vec3,
    now: Tick,
    query: &PerceptionQuery,
    occluder: &dyn Occluder,
) -> SmallVec<[Perceived; 8]> {
    let mut found = SmallVec::<[Perceived; 8]>::new();
    let forward = forward.normalize_or_zero();
    for stimulus in buffer.iter() {
        if !stimulus.is_live(now) {
            continue;
        }
        let limit = match stimulus.kind {
            StimulusKind::Sight => query.sight_radius.min(stimulus.radius),
            _ => query.hearing_radius.min(stimulus.radius),
        };
        // Squared first: most records in a full buffer are out of range, and a comparison saves
        // the square root for the ones that are not.
        let distance_squared = (stimulus.position - observer).length_squared();
        if distance_squared > limit * limit {
            continue;
        }
        let distance = distance_squared.sqrt();
        if stimulus.kind.uses_field_of_view() && query.field_of_view > -1.0 {
            // The cone test without a normalize: comparing the dot against `cos * |offset|` is the
            // same predicate, and saves a square root and a divide per record per observer.
            let offset = stimulus.position - observer;
            if forward.dot(offset) < query.field_of_view * distance {
                continue;
            }
        }
        if stimulus.kind.is_occluded() && occluder.blocks(observer, stimulus.position) {
            continue;
        }
        let strength = stimulus.strength_at_distance(distance, now);
        if strength <= 0.0 {
            continue;
        }
        found.push(Perceived {
            kind: stimulus.kind,
            position: stimulus.position,
            distance,
            strength,
        });
    }
    // Sorted by what an observer would notice first. `total_cmp` so two equal strengths order by
    // kind and position rather than by whichever arrived first in the ring.
    found.sort_unstable_by(|a, b| {
        b.strength
            .total_cmp(&a.strength)
            .then_with(|| a.distance.total_cmp(&b.distance))
            .then_with(|| a.kind.cmp(&b.kind))
            .then_with(|| a.position.x.total_cmp(&b.position.x))
            .then_with(|| a.position.z.total_cmp(&b.position.z))
    });
    found.truncate(query.max_results);
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(x: f32, z: f32) -> Vec3 {
        Vec3::new(x, 0.0, z)
    }

    fn buffer(count: usize) -> StimulusBuffer {
        StimulusBuffer::new(count).expect("buffer")
    }

    /// A wall at `x = 5` that only blocks sight.
    struct HalfWall;

    impl Occluder for HalfWall {
        fn blocks(&self, from: Vec3, to: Vec3) -> bool {
            (from.x - 5.0).signum() != (to.x - 5.0).signum()
        }
    }

    #[test]
    fn a_sighting_behind_the_observer_is_not_perceived() {
        let mut buffer = buffer(8);
        buffer.push(Stimulus::new(
            StimulusKind::Sight,
            at(0.0, -10.0),
            30.0,
            1.0,
            Tick::ZERO,
        ));
        let seen = perceive(
            &buffer,
            Vec3::ZERO,
            Vec3::Z,
            Tick::ZERO,
            &PerceptionQuery::new(),
            &OpenAir,
        );
        assert!(
            seen.is_empty(),
            "a 120 degree cone facing +z cannot see straight back"
        );
    }

    #[test]
    fn a_shout_behind_the_observer_is_still_heard() {
        let mut buffer = buffer(8);
        buffer.push(Stimulus::new(
            StimulusKind::Sound,
            at(0.0, -10.0),
            45.0,
            1.0,
            Tick::ZERO,
        ));
        let heard = perceive(
            &buffer,
            Vec3::ZERO,
            Vec3::Z,
            Tick::ZERO,
            &PerceptionQuery::new(),
            &OpenAir,
        );
        assert_eq!(heard.len(), 1);
        assert_eq!(heard[0].kind, StimulusKind::Sound);
        assert!(heard[0].strength > 0.0 && heard[0].strength < 1.0);
    }

    #[test]
    fn strength_falls_off_with_distance_and_age() {
        let stimulus = Stimulus::new(StimulusKind::Sound, at(0.0, 10.0), 40.0, 1.0, Tick::ZERO);
        let near = stimulus.strength_at(Vec3::ZERO, Tick::ZERO);
        let far = stimulus.strength_at(at(0.0, 30.0), Tick::ZERO);
        let old = stimulus.strength_at(Vec3::ZERO, Tick::new(30));
        assert!(near > far, "closer must be stronger: {near} vs {far}");
        assert!(near > old, "older must be weaker: {near} vs {old}");
    }

    #[test]
    fn an_expired_stimulus_is_neither_live_nor_perceived() {
        let mut buffer = buffer(8);
        buffer.push(
            Stimulus::new(StimulusKind::Sight, at(0.0, 5.0), 30.0, 1.0, Tick::ZERO).with_ttl(10),
        );
        let later = Tick::new(11);
        assert!(!buffer.iter().next().expect("record").is_live(later));
        let seen = perceive(
            &buffer,
            Vec3::ZERO,
            Vec3::Z,
            later,
            &PerceptionQuery::new(),
            &OpenAir,
        );
        assert!(seen.is_empty(), "expired stimuli are invisible");
        assert_eq!(buffer.retain_live(later), 1);
        assert!(buffer.is_empty());
    }

    #[test]
    fn a_wall_stops_sight_but_not_sound() {
        let mut buffer = buffer(8);
        buffer.push(Stimulus::new(
            StimulusKind::Sight,
            at(10.0, 0.0),
            30.0,
            1.0,
            Tick::ZERO,
        ));
        buffer.push(Stimulus::new(
            StimulusKind::Sound,
            at(10.0, 0.0),
            30.0,
            1.0,
            Tick::ZERO,
        ));
        let seen = perceive(
            &buffer,
            Vec3::ZERO,
            Vec3::X,
            Tick::ZERO,
            &PerceptionQuery::new(),
            &HalfWall,
        );
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].kind, StimulusKind::Sound);
    }

    #[test]
    fn the_strongest_stimuli_win_the_limited_slots() {
        let mut buffer = buffer(8);
        buffer.push(Stimulus::new(
            StimulusKind::Sight,
            at(0.0, 25.0),
            30.0,
            1.0,
            Tick::ZERO,
        ));
        buffer.push(Stimulus::new(
            StimulusKind::Sight,
            at(0.0, 5.0),
            30.0,
            1.0,
            Tick::ZERO,
        ));
        buffer.push(Stimulus::new(
            StimulusKind::Sight,
            at(0.0, 15.0),
            30.0,
            1.0,
            Tick::ZERO,
        ));
        let query = PerceptionQuery::new().with_max_results(2);
        let seen = perceive(&buffer, Vec3::ZERO, Vec3::Z, Tick::ZERO, &query, &OpenAir);
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0].distance, 5.0);
        assert_eq!(seen[1].distance, 15.0);
    }

    #[test]
    fn a_full_buffer_overwrites_the_oldest_and_never_grows() {
        let mut buffer = buffer(2);
        assert!(!buffer.push(Stimulus::new(
            StimulusKind::Sound,
            at(1.0, 0.0),
            5.0,
            1.0,
            Tick::ZERO
        )));
        assert!(!buffer.push(Stimulus::new(
            StimulusKind::Sound,
            at(2.0, 0.0),
            5.0,
            1.0,
            Tick::ZERO
        )));
        assert!(buffer.push(Stimulus::new(
            StimulusKind::Sound,
            at(3.0, 0.0),
            5.0,
            1.0,
            Tick::ZERO
        )));
        assert_eq!(buffer.len(), 2);
        let positions: Vec<f32> = buffer.iter().map(|item| item.position.x).collect();
        assert_eq!(positions, vec![3.0, 2.0]);
    }

    #[test]
    fn perception_is_deterministic() {
        let mut buffer = buffer(16);
        for index in 0..16 {
            #[allow(clippy::cast_precision_loss)]
            let x = index as f32 * 0.5;
            buffer.push(Stimulus::new(
                StimulusKind::Sight,
                at(x, 4.0),
                30.0,
                1.0,
                Tick::ZERO,
            ));
        }
        let run = || {
            perceive(
                &buffer,
                Vec3::ZERO,
                Vec3::Z,
                Tick::ZERO,
                &PerceptionQuery::new().with_max_results(5),
                &OpenAir,
            )
            .iter()
            .map(|seen| (seen.position.x.to_bits(), seen.strength.to_bits()))
            .collect::<Vec<_>>()
        };
        assert_eq!(run(), run());
    }

    #[test]
    fn a_zero_capacity_buffer_is_refused() {
        assert!(matches!(StimulusBuffer::new(0), Err(StimulusError::Empty)));
    }
}
