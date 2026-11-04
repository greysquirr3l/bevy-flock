//! Opaque identifiers.
//!
//! Every identifier in this crate is a newtype rather than a bare integer, and
//! several of them cannot be constructed except through a named constructor.
//! That is what stops `world.despawn(npc_id_as_u32)` from compiling when the
//! value in hand was a slot index.

use core::fmt;

/// One bit of [`NpcId`] is reserved to distinguish the two population modes, so
/// a recycled NPC can never collide with a unique one even if a game mixes the
/// modes in the same world.
const RECYCLED_BIT: u32 = 1 << 31;

/// Identifier of a single NPC.
///
/// `NpcId` is stable for the lifetime of an NPC and is the key used by the RNG
/// stream assignment, the spatial index tie-break and the snapshot canonical
/// ordering. Two NPCs with the same `NpcId` in one world is an engine bug, not a
/// game bug, and the population bookkeeping exists to make it unrepresentable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct NpcId(u32);

impl NpcId {
    /// An identifier for an NPC in a unique population.
    #[must_use]
    pub const fn unique(index: u32) -> Self {
        Self(index & !RECYCLED_BIT)
    }

    /// An identifier for an NPC occupying a recycled pool slot.
    #[must_use]
    pub const fn recycled(slot: u32) -> Self {
        Self((slot & !RECYCLED_BIT) | RECYCLED_BIT)
    }

    /// The index within its population mode.
    #[must_use]
    pub const fn index(self) -> u32 {
        self.0 & !RECYCLED_BIT
    }

    /// Whether this identifier belongs to a recycled pool slot.
    #[must_use]
    pub const fn is_recycled(self) -> bool {
        self.0 & RECYCLED_BIT != 0
    }

    /// The underlying bit pattern, for storage in snapshot formats.
    #[must_use]
    pub const fn to_bits(self) -> u32 {
        self.0
    }

    /// Rebuild an identifier from [`NpcId::to_bits`].
    #[must_use]
    pub const fn from_bits(bits: u32) -> Self {
        Self(bits)
    }
}

impl fmt::Display for NpcId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let tag = if self.is_recycled() { 'r' } else { 'u' };
        write!(f, "npc:{tag}{}", self.index())
    }
}

/// Identifier of a slot in a recycled population pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SlotId(u32);

impl SlotId {
    /// Wrap a raw slot index.
    #[must_use]
    pub const fn new(index: u32) -> Self {
        Self(index)
    }

    /// The raw slot index.
    #[must_use]
    pub const fn index(self) -> u32 {
        self.0
    }

    /// The NPC identifier that occupies this slot.
    #[must_use]
    pub const fn npc_id(self) -> NpcId {
        NpcId::recycled(self.0)
    }
}

impl fmt::Display for SlotId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "slot:{}", self.0)
    }
}

/// Identifier of a group of NPCs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GroupId(u32);

impl GroupId {
    /// Wrap a raw group index.
    #[must_use]
    pub const fn new(index: u32) -> Self {
        Self(index)
    }

    /// The raw group index.
    #[must_use]
    pub const fn index(self) -> u32 {
        self.0
    }
}

impl fmt::Display for GroupId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "group:{}", self.0)
    }
}

/// The seed a simulation was started from.
///
/// The seed is the only input that decides which individuals exist, how they
/// look and how they behave when nothing else changes, which is why it is a
/// first-class type rather than a `u64` floating around the API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SimSeed(u64);

impl SimSeed {
    /// Wrap a raw seed.
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self(seed)
    }

    /// The raw seed value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for SimSeed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "seed:0x{:016x}", self.0)
    }
}

impl Default for SimSeed {
    /// A fixed default seed.
    ///
    /// Deliberately not random: a test that forgets to set a seed should still
    /// be reproducible when it fails.
    fn default() -> Self {
        Self(0x5EED_5EED_5EED_5EED)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_and_recycled_ids_never_collide() {
        let unique = NpcId::unique(7);
        let recycled = NpcId::recycled(7);
        assert_ne!(unique, recycled);
        assert_eq!(unique.index(), 7);
        assert_eq!(recycled.index(), 7);
        assert!(!unique.is_recycled());
        assert!(recycled.is_recycled());
    }

    #[test]
    fn index_is_masked_so_a_high_bit_cannot_leak_into_the_index() {
        let id = NpcId::unique(RECYCLED_BIT | 3);
        assert_eq!(id.index(), 3);
        assert!(!id.is_recycled());
    }

    #[test]
    fn bits_round_trip() {
        for raw in [0_u32, 1, 42, u32::MAX] {
            let id = NpcId::from_bits(raw);
            assert_eq!(id.to_bits(), raw);
        }
    }

    #[test]
    fn slot_maps_to_its_own_npc_id() {
        assert_eq!(SlotId::new(9).npc_id(), NpcId::recycled(9));
    }
}
