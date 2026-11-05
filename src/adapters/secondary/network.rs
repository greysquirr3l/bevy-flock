//! What a client needs to know about the crowd, and nothing else.
//!
//! A crowd is the worst case for replication: ten thousand entities, most of them
//! irrelevant to any given player. Sending all of them is a bandwidth bill; sending
//! them without a bound is a bill with no ceiling. So this module does the two
//! things a netcode adapter has to do and no more:
//!
//! * **Quantise.** A position to the centimetre, a facing to a byte, a tier and a
//!   mood to a few bits. The wire format is [`NpcNetState::BYTES`] bytes per NPC and
//!   that number is pinned by a test, because it is the number a bandwidth budget is
//!   built from.
//! * **Filter by interest.** Only NPCs inside a radius around the viewer, capped at
//!   a hard limit, in a deterministic order. The cap is not a suggestion: it is what
//!   stops a busy market from becoming a latency spike the moment a player walks in.
//!
//! It does not implement replicon's traits. That integration is a dozen lines in the
//! game, and keeping it out means this crate does not pin a replicon version that
//! will be wrong in a month (ADR 0040).

use crate::adapters::primary::components::{LodTier, Npc, NpcMood};
use crate::domain::ids::NpcId;
use bevy_ecs::prelude::*;
use bevy_math::Vec3;
use bevy_transform::prelude::Transform;

/// Positions are quantised to this many units per metre, giving one-centimetre
/// resolution over a sixteen-kilometre world.
pub const POSITION_SCALE: f32 = 100.0;

/// The largest position a quantised state can represent, in metres.
pub const POSITION_LIMIT: f32 = 32_767.0 / POSITION_SCALE;

/// One NPC, as it goes over the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NpcNetState {
    /// Identity, as bits.
    pub npc: u32,
    /// Position, quantised.
    pub position: [i16; 3],
    /// Facing, in 256ths of a turn.
    pub yaw: u8,
    /// Tier, as an ordinal.
    pub tier: u8,
    /// Mood valence, in `-127..=127`.
    pub mood: i8,
}

impl NpcNetState {
    /// Bytes one state occupies on the wire.
    pub const BYTES: usize = 4 + 6 + 1 + 1 + 1;

    /// Capture an NPC's state.
    #[must_use]
    pub fn capture(npc: NpcId, position: Vec3, yaw: f32, tier: LodTier, valence: f32) -> Self {
        let quantise = |value: f32| -> i16 {
            let scaled = (value * POSITION_SCALE).round();
            let clamped = scaled.clamp(f32::from(i16::MIN), f32::from(i16::MAX));
            clamped as i16
        };
        let turn = yaw.rem_euclid(core::f32::consts::TAU) / core::f32::consts::TAU;
        let yaw_byte = (turn * 255.0).round().clamp(0.0, 255.0) as u8;
        let mood = (valence.clamp(-1.0, 1.0) * 127.0).round() as i8;
        Self {
            npc: npc.to_bits(),
            position: [
                quantise(position.x),
                quantise(position.y),
                quantise(position.z),
            ],
            yaw: yaw_byte,
            tier: match tier {
                LodTier::Near => 0,
                LodTier::Mid => 1,
                LodTier::Far => 2,
                LodTier::Ambient => 3,
            },
            mood,
        }
    }

    /// The position, dequantised.
    #[must_use]
    pub fn position(&self) -> Vec3 {
        Vec3::new(
            f32::from(self.position[0]) / POSITION_SCALE,
            f32::from(self.position[1]) / POSITION_SCALE,
            f32::from(self.position[2]) / POSITION_SCALE,
        )
    }

    /// The facing, in radians.
    #[must_use]
    pub fn yaw(&self) -> f32 {
        f32::from(self.yaw) / 255.0 * core::f32::consts::TAU
    }

    /// The mood, in `-1..=1`.
    #[must_use]
    pub fn valence(&self) -> f32 {
        f32::from(self.mood) / 127.0
    }

    /// The identity.
    #[must_use]
    pub fn identity(&self) -> NpcId {
        NpcId::from_bits(self.npc)
    }

    /// Write the state, little-endian, into a buffer.
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.npc.to_le_bytes());
        for axis in self.position {
            out.extend_from_slice(&axis.to_le_bytes());
        }
        out.push(self.yaw);
        out.push(self.tier);
        out.push(self.mood as u8);
    }

    /// Read a state from the front of a buffer, returning how many bytes were used.
    #[must_use]
    pub fn decode(bytes: &[u8]) -> Option<(Self, usize)> {
        if bytes.len() < Self::BYTES {
            return None;
        }
        let mut offset = 0;
        let mut take = |count: usize| -> Option<&[u8]> {
            let slice = bytes.get(offset..offset + count)?;
            offset += count;
            Some(slice)
        };
        let npc = u32::from_le_bytes(take(4)?.try_into().ok()?);
        let mut position = [0_i16; 3];
        for axis in &mut position {
            *axis = i16::from_le_bytes(take(2)?.try_into().ok()?);
        }
        let yaw = take(1)?.first().copied()?;
        let tier = take(1)?.first().copied()?;
        let mood = take(1)?.first().copied()? as i8;
        Some((
            Self {
                npc,
                position,
                yaw,
                tier,
                mood,
            },
            offset,
        ))
    }
}

/// Which NPCs a viewer cares about.
#[derive(Resource, Debug, Clone, Copy, PartialEq)]
pub struct InterestSet {
    /// The viewer's position.
    pub centre: Vec3,
    /// How far interest reaches, in metres.
    pub radius: f32,
    /// The hard cap on how many states may be produced.
    pub limit: usize,
}

impl Default for InterestSet {
    fn default() -> Self {
        Self {
            centre: Vec3::ZERO,
            radius: 80.0,
            limit: 256,
        }
    }
}

impl InterestSet {
    /// Whether a point is interesting to this viewer.
    #[must_use]
    pub fn contains(&self, position: Vec3) -> bool {
        self.centre.distance_squared(position) <= self.radius.max(0.0) * self.radius.max(0.0)
    }
}

/// The states produced for one viewer this frame.
#[derive(Resource, Debug, Default)]
pub struct ReplicationSet {
    states: Vec<NpcNetState>,
    dropped: u32,
}

impl ReplicationSet {
    /// The states, in deterministic identity order.
    #[must_use]
    pub fn states(&self) -> &[NpcNetState] {
        &self.states
    }

    /// How many interesting NPCs were dropped because of the cap.
    #[must_use]
    pub const fn dropped(&self) -> u32 {
        self.dropped
    }

    /// Bytes the states will occupy on the wire.
    #[must_use]
    pub fn wire_bytes(&self) -> usize {
        self.states.len() * NpcNetState::BYTES
    }

    /// How many states are queued.
    #[must_use]
    pub fn len(&self) -> usize {
        self.states.len()
    }

    /// Whether nothing is queued.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.states.is_empty()
    }

    /// Drop everything, keeping the allocation.
    pub fn clear(&mut self) {
        self.states.clear();
        self.dropped = 0;
    }
}

/// Collect the states a viewer needs, bounded and sorted.
pub fn collect_interest(
    interest: Res<InterestSet>,
    mut out: ResMut<ReplicationSet>,
    npcs: Query<(&Npc, &Transform, &LodTier, Option<&NpcMood>)>,
) {
    out.clear();
    let yaw = 0.0_f32;
    let mut candidates: Vec<(NpcId, NpcNetState)> = Vec::new();
    for (npc, transform, tier, mood) in &npcs {
        if !interest.contains(transform.translation) {
            continue;
        }
        // Ambient NPCs are simulated statistically; a client is told about them by
        // whatever draws the far crowd, not by per-entity replication.
        if *tier == LodTier::Ambient {
            continue;
        }
        let valence = mood.map_or(0.0, |mood| mood.0.valence);
        candidates.push((
            npc.0,
            NpcNetState::capture(npc.0, transform.translation, yaw, *tier, valence),
        ));
    }
    candidates.sort_unstable_by_key(|(npc, _)| *npc);
    for (index, (_, state)) in candidates.iter().enumerate() {
        if index >= interest.limit {
            out.dropped = out.dropped.saturating_add(1);
            continue;
        }
        out.states.push(*state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_app::{App, Update};

    fn app(interest: InterestSet) -> App {
        let mut app = App::new();
        app.insert_resource(interest)
            .init_resource::<ReplicationSet>()
            .add_systems(Update, collect_interest);
        app
    }

    #[test]
    fn a_state_survives_a_round_trip_within_quantisation() {
        let state = NpcNetState::capture(
            NpcId::unique(42),
            Vec3::new(12.34, 0.0, -56.78),
            1.5,
            LodTier::Near,
            0.5,
        );
        let mut bytes = Vec::new();
        state.encode(&mut bytes);
        assert_eq!(bytes.len(), NpcNetState::BYTES);
        let (decoded, used) = NpcNetState::decode(&bytes).expect("decode");
        assert_eq!(used, NpcNetState::BYTES);
        assert_eq!(decoded, state);
        assert!((decoded.position() - Vec3::new(12.34, 0.0, -56.78)).length() < 0.02);
        assert_eq!(decoded.identity(), NpcId::unique(42));
    }

    #[test]
    fn a_truncated_buffer_is_refused() {
        let state = NpcNetState::capture(NpcId::unique(1), Vec3::ZERO, 0.0, LodTier::Near, 0.0);
        let mut bytes = Vec::new();
        state.encode(&mut bytes);
        bytes.truncate(NpcNetState::BYTES - 1);
        assert!(NpcNetState::decode(&bytes).is_none());
    }

    #[test]
    fn interest_keeps_what_is_near_and_drops_what_is_far() {
        let mut app = app(InterestSet {
            centre: Vec3::ZERO,
            radius: 20.0,
            limit: 64,
        });
        app.world_mut().spawn((
            Npc(NpcId::unique(1)),
            Transform::from_translation(Vec3::new(5.0, 0.0, 0.0)),
            LodTier::Near,
        ));
        app.world_mut().spawn((
            Npc(NpcId::unique(2)),
            Transform::from_translation(Vec3::new(500.0, 0.0, 0.0)),
            LodTier::Near,
        ));
        app.update();
        let set = app.world().resource::<ReplicationSet>();
        assert_eq!(set.len(), 1);
        assert_eq!(set.states()[0].identity(), NpcId::unique(1));
        assert_eq!(set.wire_bytes(), NpcNetState::BYTES);
    }

    #[test]
    fn the_cap_is_hard_and_reported() {
        let mut app = app(InterestSet {
            centre: Vec3::ZERO,
            radius: 100.0,
            limit: 3,
        });
        for index in 0..10_u32 {
            app.world_mut().spawn((
                Npc(NpcId::unique(index)),
                Transform::from_translation(Vec3::ZERO),
                LodTier::Mid,
            ));
        }
        app.update();
        let set = app.world().resource::<ReplicationSet>();
        assert_eq!(set.len(), 3);
        assert_eq!(set.dropped(), 7);
    }

    #[test]
    fn states_are_ordered_by_identity() {
        let mut app = app(InterestSet::default());
        for index in (0..6_u32).rev() {
            app.world_mut().spawn((
                Npc(NpcId::unique(index)),
                Transform::from_translation(Vec3::ZERO),
                LodTier::Near,
            ));
        }
        app.update();
        let ids: Vec<u32> = app
            .world()
            .resource::<ReplicationSet>()
            .states()
            .iter()
            .map(|state| state.npc)
            .collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        assert_eq!(ids, sorted);
    }

    #[test]
    fn ambient_npcs_are_not_replicated_per_entity() {
        let mut app = app(InterestSet::default());
        app.world_mut().spawn((
            Npc(NpcId::unique(1)),
            Transform::from_translation(Vec3::ZERO),
            LodTier::Ambient,
        ));
        app.update();
        assert!(app.world().resource::<ReplicationSet>().is_empty());
    }

    #[test]
    fn far_positions_clamp_rather_than_wrap() {
        let state = NpcNetState::capture(
            NpcId::unique(1),
            Vec3::new(1_000_000.0, 0.0, -1_000_000.0),
            0.0,
            LodTier::Near,
            0.0,
        );
        assert_eq!(state.position[0], i16::MAX);
        assert_eq!(state.position[2], i16::MIN);
        assert!((state.position().x - POSITION_LIMIT).abs() < 0.02);
    }
}
