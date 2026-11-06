//! Social memory: what an NPC remembers about a subject, and how cheaply.
//!
//! A crowd of ten thousand cannot each keep a `Vec` of grudges, and a crowd of a
//! million cannot keep a grudge at all. So memory is layered by *eligibility*
//! rather than by population:
//!
//! | Tier | Storage | Sized by | Purpose |
//! | --- | --- | --- | --- |
//! | Hot | [`MemoryStore`]: flat 16-byte slots | enrolled NPCs × subjects | the NPCs near enough to care |
//! | Cold | [`MemoryArchive`]: one record per NPC | a bounded capacity | grudges that outlive eligibility |
//! | Ambient | aggregate per cell, not here | cells | "the district remembers" |
//!
//! The ambient tier deliberately has no representation in this module: it is a
//! property of a cell, and giving a million inhabitants individual memories would
//! be the mistake this design exists to avoid.
//!
//! # Why a flat array
//!
//! The obvious shape — `Vec<Memory>` per NPC — costs one allocation per NPC, a
//! pointer chase per lookup, and a cache miss per stranger. A flat array indexed
//! by `memory_index * subjects + subject` costs one allocation for the whole
//! crowd, iterates in address order, and makes "wipe the crowd's memory" a
//! `fill`. [`MemorySlot`] is exactly sixteen bytes; the size is asserted at
//! compile time because it is the whole cost model.
//!
//! # Why integers
//!
//! Everything here is `u16`/`i16`/`u32` and ticks. Decay is computed when memory
//! is *read*, from `now - last_seen`, so nothing sweeps ten thousand records per
//! frame and no float ever enters a decision that a replay has to reproduce.
//!
//! # Why not embeddings
//!
//! Semantic memory ("this one reminds me of that one") needs a vector index, a
//! real dependency, and inference that is bit-identical across platforms to stay
//! replayable. Social memory does not need any of it: the value is a small
//! relationship table plus *promotion* of the few relationships worth authoring
//! into names, scars and dialogue. Store integers; promote deliberately.
//!
//! # Retention is the caller's decision
//!
//! When an NPC leaves the near tier its memory can die ([`MemoryStore::release`]
//! and drop), be parked ([`MemoryArchive::insert`]), or be handed to a save file.
//! The store provides the mechanism and the archive provides a bounded, evicting
//! cache; which of the three a game wants is a policy the plugin exposes, not a
//! decision this module makes.

use std::collections::BTreeMap;

use smallvec::SmallVec;

use crate::domain::ids::NpcId;
use crate::domain::tick::Tick;

/// The size of one memory slot, in bytes.
///
/// Asserted at compile time: the store's cost is `capacity × subjects × this`, and
/// a field added carelessly would double it.
pub const SLOT_BYTES: usize = 16;

/// The most subjects one store can track.
///
/// Subjects are dense identifiers — one per player, pet or rival NPC a game wants
/// remembered. The slot array is indexed by subject, so this multiplies the store's
/// memory: a store costs `capacity × subjects × 16` bytes for its slots alone.
///
/// Sixty-four is the ceiling (ADR 0016): a player, a party, a faction and its
/// officers fit without a second store. It is a ceiling, not a stride —
/// [`MemoryStore::new`] sizes to the subjects a game asks for, so a game using four
/// pays for four whether this number is sixteen or sixty-four.
pub const MAX_SUBJECTS: u16 = 64;

/// Identifies something an NPC can remember.
///
/// A player, a pet, another NPC — the store does not care which, and does not need
/// to: guards remembering each other is the same mechanism as guards remembering
/// the player, which is why this is not called `PlayerId`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SubjectId(u16);

impl SubjectId {
    /// A subject identifier. Callers assign dense identifiers, usually by
    /// registering subjects with the plugin in a fixed order.
    #[must_use]
    pub const fn new(raw: u16) -> Self {
        Self(raw)
    }

    /// The raw identifier.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

/// A handle to an NPC's row in a [`MemoryStore`].
///
/// Handed out by [`MemoryStore::enrol`] and stored on the entity, the same way
/// [`NpcId`] is. The store itself keeps no map from [`NpcId`], because a map lookup
/// per remembered NPC per frame is exactly the kind of cost this design avoids.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct MemoryIndex(u32);

impl MemoryIndex {
    /// A handle from a raw index.
    ///
    /// For restoring a row that was persisted, or for a test that wants a specific
    /// one. It is not a capability: the store checks every handle against its own
    /// ownership table ([`MemoryStore::is_enrolled`], [`MemoryStore::owner`]), so a
    /// forged index addresses nothing rather than someone else's memory.
    #[must_use]
    pub const fn new(raw: u32) -> Self {
        Self(raw)
    }

    /// The raw index, for storage in a component.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// What happened, as a bit so it survives in one `u16`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum EncounterKind {
    /// They talked.
    Spoke,
    /// The subject gave something.
    Helped,
    /// The subject hurt this NPC.
    Attacked,
    /// The subject took something.
    Robbed,
    /// This NPC saw the subject do something to someone else.
    Witnessed,
    /// They exchanged goods.
    Traded,
}

impl EncounterKind {
    /// Every kind, in a fixed order.
    pub const ALL: [Self; 6] = [
        Self::Spoke,
        Self::Helped,
        Self::Attacked,
        Self::Robbed,
        Self::Witnessed,
        Self::Traded,
    ];

    /// The bit this kind occupies in [`MemorySlot::flags`].
    #[must_use]
    pub const fn flag(self) -> u16 {
        1 << (self as u16)
    }

    /// How much familiarity one encounter adds.
    #[must_use]
    pub const fn weight(self) -> u16 {
        match self {
            Self::Spoke => 8,
            Self::Traded => 16,
            Self::Witnessed => 24,
            Self::Helped => 48,
            Self::Robbed => 64,
            Self::Attacked => 96,
        }
    }

    /// How the encounter colours the relationship, before decay.
    #[must_use]
    pub const fn valence(self) -> i16 {
        match self {
            Self::Spoke => 24,
            Self::Traded => 40,
            Self::Witnessed => -48,
            Self::Helped => 160,
            Self::Robbed => -180,
            Self::Attacked => -260,
        }
    }

    /// A lowercase name for trace output and save files.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Spoke => "spoke",
            Self::Helped => "helped",
            Self::Attacked => "attacked",
            Self::Robbed => "robbed",
            Self::Witnessed => "witnessed",
            Self::Traded => "traded",
        }
    }
}

/// One thing that happened, ready to be remembered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Encounter {
    /// When it happened.
    pub tick: Tick,
    /// What happened.
    pub kind: EncounterKind,
    /// How it felt, overriding the kind's default when the game wants nuance.
    pub valence: i16,
}

impl Encounter {
    /// An encounter using the kind's default emotional weight.
    #[must_use]
    pub const fn new(kind: EncounterKind, tick: Tick) -> Self {
        Self {
            tick,
            kind,
            valence: kind.valence(),
        }
    }

    /// An encounter with an explicit emotional weight.
    #[must_use]
    pub const fn with_valence(mut self, valence: i16) -> Self {
        self.valence = valence;
        self
    }
}

/// One NPC's memory of one subject.
///
/// Sixteen bytes, `Copy`, no pointers, no allocation: a crowd's worth of these is
/// a contiguous array and can be snapshotted with a `copy_from_slice`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct MemorySlot {
    familiarity: u16,
    valence: i16,
    encounters: u16,
    flags: u16,
    first_seen: u32,
    last_seen: u32,
}

// The cost model, asserted rather than documented and hoped for.
const _: () = assert!(core::mem::size_of::<MemorySlot>() == SLOT_BYTES);

impl MemorySlot {
    /// An empty slot: nobody remembers anybody.
    pub const EMPTY: Self = Self {
        familiarity: 0,
        valence: 0,
        encounters: 0,
        flags: 0,
        first_seen: 0,
        last_seen: 0,
    };

    /// Build a slot from its parts.
    ///
    /// The inverse of the accessors, used by the save format (ADR 0012) to rebuild a
    /// slot at a new clock. Callers that are not a loader should record encounters
    /// through [`MemoryStore::remember`] instead.
    #[must_use]
    pub const fn from_parts(
        familiarity: u16,
        valence: i16,
        encounters: u16,
        flags: u16,
        first_seen: Tick,
        last_seen: Tick,
    ) -> Self {
        Self {
            familiarity,
            valence,
            encounters,
            flags,
            first_seen: first_seen.get() as u32,
            last_seen: last_seen.get() as u32,
        }
    }

    /// Whether anything is remembered here.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.encounters == 0 && self.familiarity == 0
    }

    /// Raw familiarity, before decay.
    #[must_use]
    pub const fn familiarity(self) -> u16 {
        self.familiarity
    }

    /// Raw emotional colouring, before decay. Negative is hostile.
    #[must_use]
    pub const fn valence(self) -> i16 {
        self.valence
    }

    /// How many encounters are remembered, saturating at 65,535.
    #[must_use]
    pub const fn encounters(self) -> u16 {
        self.encounters
    }

    /// Which kinds of encounter have ever happened.
    #[must_use]
    pub const fn flags(self) -> u16 {
        self.flags
    }

    /// Whether a given kind has ever happened.
    #[must_use]
    pub const fn has(self, kind: EncounterKind) -> bool {
        self.flags & kind.flag() != 0
    }

    /// The tick of the most recent encounter.
    #[must_use]
    pub const fn last_seen(self) -> Tick {
        Tick::new(self.last_seen as u64)
    }

    /// The tick of the first encounter.
    #[must_use]
    pub const fn first_seen(self) -> Tick {
        Tick::new(self.first_seen as u64)
    }

    /// Whether this NPC has needed a guard since.
    ///
    /// The question a behaviour tree actually asks, answered by one `AND`: a
    /// negative memory that has not faded is a grudge that still has teeth.
    #[must_use]
    pub const fn is_grudge(self) -> bool {
        self.valence < 0
            && self.flags
                & (EncounterKind::Attacked.flag()
                    | EncounterKind::Robbed.flag()
                    | EncounterKind::Witnessed.flag())
                != 0
    }

    fn record(&mut self, encounter: &Encounter) {
        let stamp = stamp_of(encounter.tick);
        if self.encounters == 0 {
            self.first_seen = stamp;
        }
        self.encounters = self.encounters.saturating_add(1);
        self.familiarity = self.familiarity.saturating_add(encounter.kind.weight());
        self.flags |= encounter.kind.flag();
        // A running opinion, three parts history to one part news. Integer
        // arithmetic, so every platform agrees to the bit; an emphasis on the most
        // recent encounter would let one bad afternoon erase a friendship.
        self.valence = blend(self.valence, encounter.valence);
        self.last_seen = stamp;
    }
}

/// Three parts history to one part news, in integers.
///
/// Truncating toward zero rather than toward negative infinity keeps the curve
/// symmetric: a grudge fades at the same rate as a fondness.
fn blend(current: i16, event: i16) -> i16 {
    let mixed = (i32::from(current) * 3 + i32::from(event)) / 4;
    i16::try_from(mixed).unwrap_or(if mixed < 0 { i16::MIN } else { i16::MAX })
}

/// Halve a signed value by shifting its magnitude.
///
/// `>>` on a negative `i16` is arithmetic: it floors, and it converges to -1 rather
/// than to zero, so a grudge would last forever at one point of hostility. Shifting
/// the magnitude instead reaches zero, and truncating toward zero keeps the curve
/// symmetric with [`blend`].
fn decay_signed(value: i16, shifts: u32) -> i16 {
    let magnitude = i16::try_from(value.unsigned_abs() >> shifts).unwrap_or(0);
    if value < 0 { -magnitude } else { magnitude }
}

fn stamp_of(tick: Tick) -> u32 {
    // Ticks are `u64`; slots are 16 bytes. A `u32` stamp wraps after 828 days at
    // 60 Hz, and decay treats a wrapped age as very old, which is the safe
    // direction for a value that should fade anyway.
    tick.get() as u32
}

/// How fast memory fades, in ticks.
///
/// Decay is a shift, so the curve is a staircase rather than a smooth exponential:
/// deterministic, branch-free, and impossible to get subtly wrong per platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecayPolicy {
    /// Ticks per halving. A grudge with `half_life_ticks: 3_600` loses half its
    /// intensity every sim-minute.
    pub half_life_ticks: u32,
}

impl DecayPolicy {
    /// Memory that never fades, for a game where a grudge is forever.
    pub const NONE: Self = Self { half_life_ticks: 0 };

    /// A grudge that halves every sim-minute at 60 Hz.
    pub const MINUTE: Self = Self {
        half_life_ticks: 3_600,
    };

    /// How many halvings have elapsed.
    fn shifts(self, age: u32) -> u32 {
        if self.half_life_ticks == 0 {
            return 0;
        }
        // Fifteen shifts take any i16 to zero, so anything older is the same as
        // forgotten and further work would be wasted.
        (age / self.half_life_ticks).min(15)
    }

    /// Familiarity now.
    #[must_use]
    pub fn familiarity_at(self, slot: MemorySlot, now: Tick) -> u16 {
        let age = stamp_of(now).wrapping_sub(slot.last_seen);
        slot.familiarity >> self.shifts(age)
    }

    /// Emotional colouring now. Negative is hostile.
    #[must_use]
    pub fn valence_at(self, slot: MemorySlot, now: Tick) -> i16 {
        let age = stamp_of(now).wrapping_sub(slot.last_seen);
        decay_signed(slot.valence, self.shifts(age))
    }
}

/// The ordering [`select_enrolment`] uses: nearest first, then by identity.
///
/// Exposed so a caller holding more than a pair — the ECS bridge needs the entity as
/// well as the id and the distance — can sort by the same rule instead of re-deriving
/// it and drifting.
#[must_use]
pub fn enrolment_order(left: (NpcId, f32), right: (NpcId, f32)) -> core::cmp::Ordering {
    left.1
        .total_cmp(&right.1)
        .then_with(|| left.0.cmp(&right.0))
}

/// Who gets a memory row when the player walks into a market.
///
/// The store is a fixed resource — a row capacity and a subject ceiling — so something
/// has to choose which NPCs are worth remembering the player, and it has to choose the
/// same way on every run. The rule is:
///
/// 1. nearest first, because proximity to the player is the only reason these rows
///    exist at all: memory is granted to the NPCs the player can actually meet;
/// 2. ties broken by identity, so two people at the same distance resolve the same way
///    in a replay;
/// 3. truncated to `limit`, the per-tick enrolment budget, so a player teleporting
///    into a packed square cannot enrol a thousand rows in one frame.
///
/// Returns the sorted prefix that fits the budget. The slice is sorted in place so a
/// caller can reuse it for a diagnostic without sorting twice.
pub fn select_enrolment(candidates: &mut [(NpcId, f32)], limit: usize) -> &[(NpcId, f32)] {
    candidates.sort_unstable_by(|left, right| enrolment_order(*left, *right));
    let kept = limit.min(candidates.len());
    // `kept` is derived from `len()`, so the range is in bounds by construction.
    candidates.get(..kept).unwrap_or(candidates)
}

/// Everything one NPC remembers, lifted out of the store.
///
/// Returned by [`MemoryStore::release`] so the caller can drop it, park it in a
/// [`MemoryArchive`], or write it to a save file — the three retention strategies
/// a game chooses between.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct NpcMemories {
    /// Whose memory this is.
    pub npc: NpcId,
    /// The non-empty slots, in subject order.
    pub slots: SmallVec<[(SubjectId, MemorySlot); 8]>,
}

impl NpcMemories {
    /// The strongest sentiment in this record, signed, before decay.
    #[must_use]
    pub fn strongest(&self) -> i16 {
        self.slots
            .iter()
            .map(|(_, slot)| slot.valence)
            .max_by_key(|value| value.unsigned_abs())
            .unwrap_or(0)
    }

    /// Whether anything is worth keeping.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }
}

/// The hot tier: flat slots for the NPCs close enough to matter.
///
/// Capacity is a hard bound, allocated once. Enrolling past it fails rather than
/// growing, because a crowd simulation that allocates under load is a crowd
/// simulation that hitches.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "memory", derive(bevy_ecs::prelude::Resource))]
pub struct MemoryStore {
    subjects: u16,
    capacity: u32,
    slots: Vec<MemorySlot>,
    owners: Vec<Option<NpcId>>,
    free: Vec<u32>,
    high_water: u32,
}

impl MemoryStore {
    /// A store for `capacity` NPCs, each able to remember `subjects` subjects.
    ///
    /// Costs `capacity × subjects × 16` bytes, allocated once: 16,384 NPCs
    /// remembering eight subjects is 2 MB.
    #[must_use]
    pub fn new(capacity: u32, subjects: u16) -> Self {
        let subjects = subjects.clamp(1, MAX_SUBJECTS);
        let cells = (capacity as usize).saturating_mul(usize::from(subjects));
        Self {
            subjects,
            capacity,
            slots: vec![MemorySlot::EMPTY; cells],
            owners: vec![None; capacity as usize],
            free: Vec::new(),
            high_water: 0,
        }
    }

    /// How many NPCs can be enrolled.
    #[must_use]
    pub const fn capacity(&self) -> u32 {
        self.capacity
    }

    /// Bytes this store holds, whatever it has used.
    ///
    /// The slot array is the whole story: `capacity × subjects × 16`, whether or not a
    /// single relationship exists. Reported rather than estimated, so a game can size
    /// a budget before it spawns and so ADR 0016's claims are checked by a test rather
    /// than by arithmetic in a document.
    #[must_use]
    pub fn footprint(&self) -> usize {
        self.slots.capacity() * core::mem::size_of::<MemorySlot>()
            + self.owners.capacity() * core::mem::size_of::<Option<NpcId>>()
            + self.free.capacity() * core::mem::size_of::<u32>()
    }

    /// How many subjects each NPC can remember.
    #[must_use]
    pub const fn subjects(&self) -> u16 {
        self.subjects
    }

    /// How many NPCs are enrolled now.
    #[must_use]
    pub fn len(&self) -> u32 {
        self.count_enrolled()
    }

    /// Whether nobody is enrolled.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Bytes of slot storage.
    #[must_use]
    pub fn bytes(&self) -> usize {
        self.slots.len() * SLOT_BYTES
    }

    /// Give an NPC a row. Fails when the store is full.
    ///
    /// Reusing a released index is deliberate: the free list is LIFO, so a crowd
    /// that churns eligibility reuses rows in a deterministic order and the store
    /// never fragments.
    pub fn enrol(&mut self, npc: NpcId) -> Option<MemoryIndex> {
        let index = if let Some(index) = self.free.pop() {
            index
        } else {
            if self.high_water >= self.capacity {
                return None;
            }
            let index = self.high_water;
            self.high_water += 1;
            index
        };
        let row = self.row_range(MemoryIndex(index))?;
        if let Some(slots) = self.slots.get_mut(row) {
            slots.fill(MemorySlot::EMPTY);
        }
        if let Some(owner) = self.owners.get_mut(index as usize) {
            *owner = Some(npc);
        }
        Some(MemoryIndex(index))
    }

    /// Enrol the nearest candidates, up to `limit` new rows.
    ///
    /// Returns how many rows were created. A candidate already enrolled is skipped
    /// rather than counted, so a tick that sees the same crowd twice does not report
    /// work it did not do; a full store stops early, and the returned count is how the
    /// caller knows the crowd outgrew the budget.
    pub fn enrol_nearest(&mut self, candidates: &[(NpcId, f32)], limit: usize) -> u32 {
        let mut enrolled = 0;
        for (npc, _) in candidates.iter().take(limit) {
            if self.enrolled().any(|(_, owner)| owner == *npc) {
                continue;
            }
            if self.enrol(*npc).is_some() {
                enrolled += 1;
            } else {
                break;
            }
        }
        enrolled
    }

    /// Whether an index still belongs to the NPC it was issued to.
    #[must_use]
    pub fn is_enrolled(&self, index: MemoryIndex) -> bool {
        self.owners
            .get(index.0 as usize)
            .is_some_and(Option::is_some)
    }

    /// Every enrolled row, in index order.
    ///
    /// Deliberately an iterator over live rows rather than the whole capacity: the
    /// save format only writes what exists, and a crowd of hermits should save to a
    /// header.
    pub fn enrolled(&self) -> impl Iterator<Item = (MemoryIndex, NpcId)> + '_ {
        self.owners
            .iter()
            .enumerate()
            .filter_map(|(index, owner)| owner.map(|npc| (MemoryIndex(index as u32), npc)))
    }

    /// The NPC a row belongs to.
    #[must_use]
    pub fn owner(&self, index: MemoryIndex) -> Option<NpcId> {
        self.owners.get(index.0 as usize).copied().flatten()
    }

    /// Record an encounter.
    ///
    /// Returns `false` if the index is not enrolled or the subject is out of
    /// range, which is a caller bug rather than a simulation outcome: the store
    /// reports it instead of writing into the wrong row.
    pub fn remember(
        &mut self,
        index: MemoryIndex,
        subject: SubjectId,
        encounter: &Encounter,
    ) -> bool {
        if !self.is_enrolled(index) || subject.get() >= self.subjects {
            return false;
        }
        let cell = self.cell(index, subject);
        cell.and_then(|cell| self.slots.get_mut(cell))
            .is_some_and(|slot| {
                slot.record(encounter);
                true
            })
    }

    /// Put an exact memory into a row.
    ///
    /// The primitive behind [`MemoryArchive::restore`]: a parked grudge comes back
    /// as it was, not as an approximation rebuilt from an encounter log. Replaying
    /// events to reconstruct a slot would decay it a second time and lose the
    /// original kind.
    pub fn implant(&mut self, index: MemoryIndex, subject: SubjectId, slot: MemorySlot) -> bool {
        let Some(cell) = self.cell(index, subject) else {
            return false;
        };
        self.slots.get_mut(cell).is_some_and(|target| {
            *target = slot;
            true
        })
    }

    /// What this NPC remembers about a subject, before decay.
    #[must_use]
    pub fn recall(&self, index: MemoryIndex, subject: SubjectId) -> Option<MemorySlot> {
        let cell = self.cell(index, subject)?;
        self.slots
            .get(cell)
            .copied()
            .filter(|slot| !slot.is_empty())
    }

    /// The single strongest memory this NPC holds about anyone, decayed to `now`.
    ///
    /// This is the read a behaviour tree makes: not "what do you remember about
    /// subject 3" but "who matters most to you right now". It is O(subjects), which
    /// is a fixed bound rather than a crowd-sized one, and it is called only for the
    /// minds close enough to care (see `MemoryConfig::publish_radius`).
    ///
    /// Ties break to the lower subject, so two people remembered equally well always
    /// resolve the same way and a replay agrees with the run it replays.
    #[must_use]
    pub fn strongest_recall(
        &self,
        index: MemoryIndex,
        now: Tick,
        decay: DecayPolicy,
    ) -> Option<(SubjectId, i16, u16, u16)> {
        if !self.is_enrolled(index) {
            return None;
        }
        let mut best: Option<(SubjectId, i16, u16, u16)> = None;
        for raw in 0..self.subjects {
            let subject = SubjectId::new(raw);
            let Some(slot) = self.recall(index, subject) else {
                continue;
            };
            let familiarity = decay.familiarity_at(slot, now);
            if familiarity == 0 {
                // Faded to nothing: the answer a tree wants is "no memory", not a
                // slot with a zero in it.
                continue;
            }
            let valence = decay.valence_at(slot, now);
            let candidate = (subject, valence, familiarity, slot.flags);
            best = match best {
                Some(current) if current.2 >= familiarity => Some(current),
                _ => Some(candidate),
            };
        }
        best
    }
    /// What this NPC remembers, decayed to `now`.
    ///
    /// Returns `None` when the memory has faded to nothing, which is the answer a
    /// behaviour tree wants: not "a faint memory" but "no memory".
    #[must_use]
    pub fn recall_decayed(
        &self,
        index: MemoryIndex,
        subject: SubjectId,
        now: Tick,
        decay: DecayPolicy,
    ) -> Option<(i16, u16)> {
        let slot = self.recall(index, subject)?;
        let valence = decay.valence_at(slot, now);
        let familiarity = decay.familiarity_at(slot, now);
        (familiarity > 0).then_some((valence, familiarity))
    }

    /// Erase one memory.
    pub fn forget(&mut self, index: MemoryIndex, subject: SubjectId) -> bool {
        let Some(cell) = self.cell(index, subject) else {
            return false;
        };
        self.slots.get_mut(cell).is_some_and(|slot| {
            let had = !slot.is_empty();
            *slot = MemorySlot::EMPTY;
            had
        })
    }

    /// Lift an NPC's memory out of the store and free the row.
    ///
    /// The three retention strategies look like this:
    ///
    /// - forget: `let _ = store.release(index);`
    /// - park: `archive.insert(store.release(index)?);`
    /// - persist: write the returned record into a save file.
    pub fn release(&mut self, index: MemoryIndex) -> Option<NpcMemories> {
        let npc = self.owner(index)?;
        let subjects = self.subjects;
        let mut slots = SmallVec::<[(SubjectId, MemorySlot); 8]>::new();
        for raw in 0..subjects {
            let subject = SubjectId::new(raw);
            if let Some(slot) = self.recall(index, subject) {
                slots.push((subject, slot));
            }
        }
        self.release_row(index);
        Some(NpcMemories { npc, slots })
    }

    /// Free every row whose strongest memory has decayed below `min_familiarity`.
    ///
    /// Meant to be called on a slow cadence — a few times a second, not per frame —
    /// and deterministic: rows are visited in index order and the comparison is
    /// integer.
    pub fn sweep(&mut self, now: Tick, decay: DecayPolicy, min_familiarity: u16) -> u32 {
        let mut freed = 0;
        for index in 0..self.high_water {
            if self.owners.get(index as usize).is_none_or(Option::is_none) {
                continue;
            }
            let strongest = self.strongest_familiarity(MemoryIndex(index), now, decay);
            if strongest.is_some_and(|value| value >= min_familiarity) {
                continue;
            }
            self.release_row(MemoryIndex(index));
            freed += 1;
        }
        freed
    }

    /// Free every row whose strongest memory has decayed below `min_familiarity`,
    /// **except** rows the caller vouches for.
    ///
    /// This is the difference between a memory system and a leak with a half-life. Decay
    /// itself is already independent of simulation — [`crate::domain::memory::DecayPolicy`]
    /// evaluates it from `now - last_seen` at read time — but *reclamation* is not: a row
    /// is only freed when somebody sweeps it. An NPC nobody simulates, because it is
    /// folded into the continuum (ADR 0018) or simply far away, is never swept by
    /// per-entity code, so its row and its subject slots sit there until the store is
    /// full.
    ///
    /// The `keep` predicate is the anchor seam. A row it protects is never freed, however
    /// long the subject has been away: the *warmth* of the memory decays to nothing
    /// through [`MemoryStore::recall_decayed`], while the *record* — the fact that this
    /// NPC has met this subject, and what kind of meeting it was — survives. That is the
    /// difference between "I have forgotten you" and "I know exactly who you are, and I
    /// am not pleased".
    pub fn sweep_keeping(
        &mut self,
        now: Tick,
        decay: DecayPolicy,
        min_familiarity: u16,
        keep: &dyn Fn(NpcId) -> bool,
    ) -> u32 {
        let mut freed = 0;
        for index in 0..self.high_water {
            let Some(Some(owner)) = self.owners.get(index as usize).copied() else {
                continue;
            };
            if keep(owner) {
                continue;
            }
            let strongest = self.strongest_familiarity(MemoryIndex(index), now, decay);
            if strongest.is_some_and(|value| value >= min_familiarity) {
                continue;
            }
            self.release_row(MemoryIndex(index));
            freed += 1;
        }
        freed
    }

    /// Sweep on a cadence, exempting anchored owners.
    ///
    /// `cadence` is in ticks: a sweep every 600 ticks is ten seconds at 60 Hz and is the
    /// only rate at which this needs to run. Call it every frame if you like — it is
    /// idempotent — but the point of the cadence is that a game with ten thousand absent
    /// NPCs pays for them once every ten seconds rather than once a frame.
    pub fn age(
        &mut self,
        now: Tick,
        decay: DecayPolicy,
        policy: AgingPolicy,
        keep: &dyn Fn(NpcId) -> bool,
    ) -> AgingReport {
        let mut report = AgingReport::default();
        if policy.cadence == 0 || !now.get().is_multiple_of(policy.cadence) {
            return report;
        }
        report.swept = true;
        report.freed = self.sweep_keeping(now, decay, policy.min_familiarity, keep);
        report.retained = self
            .owners
            .iter()
            .filter(|owner| owner.is_some_and(&keep))
            .count() as u32;
        report
    }

    /// The strongest decayed familiarity this NPC still holds, if any.
    #[must_use]
    pub fn strongest_familiarity(
        &self,
        index: MemoryIndex,
        now: Tick,
        decay: DecayPolicy,
    ) -> Option<u16> {
        let row = self.row_range(index)?;
        self.slots
            .get(row)?
            .iter()
            .filter(|slot| !slot.is_empty())
            .map(|slot| decay.familiarity_at(*slot, now))
            .max()
    }

    fn cell(&self, index: MemoryIndex, subject: SubjectId) -> Option<usize> {
        if !self.is_enrolled(index) || subject.get() >= self.subjects {
            return None;
        }
        let base = (index.0 as usize).checked_mul(usize::from(self.subjects))?;
        base.checked_add(usize::from(subject.get()))
    }

    fn row_range(&self, index: MemoryIndex) -> Option<core::ops::Range<usize>> {
        let base = (index.0 as usize).checked_mul(usize::from(self.subjects))?;
        let end = base.checked_add(usize::from(self.subjects))?;
        (end <= self.slots.len()).then_some(base..end)
    }

    fn release_row(&mut self, index: MemoryIndex) {
        if let Some(row) = self.row_range(index)
            && let Some(slots) = self.slots.get_mut(row)
        {
            slots.fill(MemorySlot::EMPTY);
        }
        // `take()` is the guard: a row released twice finds no owner the second
        // time, so the free list cannot hold the same row twice. Testing
        // `free.contains(...)` instead would have made sweeping a full store
        // quadratic — 24 ms for 16k rows, which the benchmark caught.
        let released = self
            .owners
            .get_mut(index.0 as usize)
            .is_some_and(|owner| owner.take().is_some());
        if released {
            self.free.push(index.0);
        }
    }

    fn count_enrolled(&self) -> u32 {
        self.owners.iter().filter(|owner| owner.is_some()).count() as u32
    }
}

/// The cold tier: a bounded cache of records for NPCs that left the hot tier.
///
/// A `BTreeMap` rather than a `HashMap` because iteration order is part of the
/// eviction rule, and eviction order decides which grudge survives a busy night in
/// the tavern. Deterministic beats fast here; this is a slow path.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct MemoryArchive {
    capacity: usize,
    entries: BTreeMap<NpcId, NpcMemories>,
}

impl MemoryArchive {
    /// An archive holding at most `capacity` records.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            entries: BTreeMap::new(),
        }
    }

    /// How many records the archive holds.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// How many records are stored.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the archive is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Store a record, evicting the least-remembered NPC if the archive is full.
    ///
    /// Eviction picks the lowest total familiarity, breaking ties by lowest
    /// [`NpcId`]. Both are stable values, so two replays evict the same NPC — which
    /// matters, because the alternative is a crowd that forgets differently on
    /// every machine.
    pub fn insert(&mut self, memories: NpcMemories) -> Option<NpcMemories> {
        if self.capacity == 0 || memories.is_empty() {
            return None;
        }
        let evicted =
            if self.entries.len() >= self.capacity && !self.entries.contains_key(&memories.npc) {
                let victim = self
                    .entries
                    .iter()
                    .map(|(npc, record)| (total_familiarity(record), *npc))
                    .min()
                    .map(|(_, npc)| npc)?;
                self.entries.remove(&victim)
            } else {
                None
            };
        self.entries.insert(memories.npc, memories);
        evicted
    }

    /// Recall a parked record.
    #[must_use]
    pub fn get(&self, npc: NpcId) -> Option<&NpcMemories> {
        self.entries.get(&npc)
    }

    /// Take a record back out, as when an NPC returns to the near tier.
    pub fn take(&mut self, npc: NpcId) -> Option<NpcMemories> {
        self.entries.remove(&npc)
    }

    /// Whether a record is stored.
    #[must_use]
    pub fn contains(&self, npc: NpcId) -> bool {
        self.entries.contains_key(&npc)
    }

    /// Iterate records in identifier order.
    pub fn iter(&self) -> impl Iterator<Item = (&NpcId, &NpcMemories)> {
        self.entries.iter()
    }

    /// Put a record back into a store, enrolling the NPC again.
    ///
    /// The inverse of [`MemoryStore::release`], so a returning NPC resumes the
    /// grudge it left behind rather than starting as a stranger.
    pub fn restore(
        &mut self,
        npc: NpcId,
        store: &mut MemoryStore,
    ) -> Option<(MemoryIndex, NpcMemories)> {
        let memories = self.entries.remove(&npc)?;
        let index = store.enrol(npc)?;
        for (subject, slot) in &memories.slots {
            store.implant(index, *subject, *slot);
        }
        Some((index, memories))
    }
}

fn total_familiarity(memories: &NpcMemories) -> u32 {
    memories
        .slots
        .iter()
        .map(|(_, slot)| u32::from(slot.familiarity))
        .sum()
}

/// How often absent NPCs are swept, and how faded a memory has to be to be freed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgingPolicy {
    /// Ticks between sweeps. Zero disables sweeping entirely.
    pub cadence: u64,
    /// Familiarity at or above this keeps a row alive. Zero means "fade to nothing
    /// before you are forgotten", which is almost never what a game wants.
    pub min_familiarity: u16,
}

impl AgingPolicy {
    /// Ten seconds at 60 Hz, with a floor of one point of familiarity.
    pub const DEFAULT: Self = Self {
        cadence: 600,
        min_familiarity: 1,
    };

    /// Sweep every `cadence` ticks with the default floor.
    #[must_use]
    pub const fn every(cadence: u64) -> Self {
        Self {
            cadence,
            min_familiarity: 1,
        }
    }
}

impl Default for AgingPolicy {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// What an [`MemoryStore::age`] pass did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AgingReport {
    /// Whether the cadence came due this tick.
    pub swept: bool,
    /// Rows reclaimed.
    pub freed: u32,
    /// Rows kept because their owner was anchored.
    pub retained: u32,
}

#[cfg(test)]
mod tests {
    use crate::testkit::{GetOrDie, OrDie};

    #[test]
    fn an_absent_npc_is_reclaimed_and_its_slot_reused() {
        // The player leaves for an hour of game time. The NPC is never simulated, so no
        // per-entity code ever touches its row: the cadence sweep is the only thing that
        // can reclaim it, and if it cannot, this is a leak with a half-life.
        let mut store = MemoryStore::new(8, 4);
        let absent = NpcId::unique(1);
        let index = store.enrol(absent).or_die();
        let subject = SubjectId::new(0);
        store.remember(
            index,
            subject,
            &Encounter::new(EncounterKind::Spoke, Tick::new(0)),
        );
        assert_eq!(store.len(), 1);

        let policy = AgingPolicy::every(600);
        let far_future = Tick::new(216_000);

        // Before the cadence comes due, nothing happens: that is the point of a cadence.
        let off_beat = store.age(Tick::new(216_001), DecayPolicy::MINUTE, policy, &|_| false);
        assert!(!off_beat.swept, "off cadence is not a sweep");

        let report = store.age(far_future, DecayPolicy::MINUTE, policy, &|_| false);
        assert!(report.swept);
        assert_eq!(report.freed, 1, "a hundred half-lives is not a memory");
        assert_eq!(store.len(), 0, "the row is gone");

        // And the slot is genuinely reusable, which is what makes this a fix rather than
        // a smaller leak.
        let newcomer = NpcId::unique(2);
        let second = store.enrol(newcomer).or_die();
        assert!(store.remember(
            second,
            subject,
            &Encounter::new(EncounterKind::Traded, Tick::new(216_000))
        ));
    }

    #[test]
    fn an_anchored_npc_is_kept_and_its_grudge_survives_its_warmth() {
        // An hour away, anchored: the record survives, the feeling does not.
        let mut store = MemoryStore::new(8, 4);
        let anchored = NpcId::unique(7);
        let index = store.enrol(anchored).or_die();
        let subject = SubjectId::new(1);
        store.remember(
            index,
            subject,
            &Encounter::new(EncounterKind::Attacked, Tick::new(0)),
        );

        let policy = AgingPolicy::every(600);
        let now = Tick::new(216_000);
        let keep = |owner: NpcId| owner == anchored;
        let report = store.age(now, DecayPolicy::MINUTE, policy, &keep);

        assert_eq!(report.freed, 0, "an anchor is not swept");
        assert_eq!(report.retained, 1);
        assert_eq!(store.len(), 1, "the row is still there");

        // Warmth is gone; the fact of the attack is not.
        let decayed = store.recall_decayed(index, subject, now, DecayPolicy::MINUTE);
        assert!(
            decayed.is_none_or(|(valence, familiarity)| familiarity == 0 && valence == 0),
            "after a hundred half-lives nothing is felt: {decayed:?}"
        );
        let raw = store.recall(index, subject).or_die();
        assert!(raw.is_grudge(), "and it is still a grudge");
        assert_eq!(raw.last_seen(), Tick::new(0), "the date is unchanged");
        assert!(
            raw.encounters() == 1,
            "the count is history, not a feeling, so it does not decay"
        );
    }

    #[test]
    fn a_long_absence_does_not_grow_the_store() {
        // Five thousand arrivals and departures through a store with room for sixteen
        // rows: only reclamation keeps this bounded, and absence must not defeat it.
        let mut store = MemoryStore::new(16, 2);
        let policy = AgingPolicy::every(60);
        let mut tick = 0_u64;
        for round in 0..5_000_u64 {
            let npc = NpcId::unique(u32::try_from(round).or_die());
            let Some(index) = store.enrol(npc) else {
                unreachable!("enrol refused at round {round}: {} rows held", store.len());
            };
            store.remember(
                index,
                SubjectId::new(0),
                &Encounter::new(EncounterKind::Spoke, Tick::new(tick)),
            );
            // Eight half-lives between arrivals: the previous visitor has faded to
            // nothing, so the row is reclaimable. A shorter gap would test that the store
            // fills, which is a different assertion.
            tick += 28_800;
            store.age(Tick::new(tick), DecayPolicy::MINUTE, policy, &|_| false);
            assert!(
                store.len() <= 16,
                "the store grew to {} rows at round {round}",
                store.len()
            );
        }
    }
    use super::*;

    const SUBJECT: SubjectId = SubjectId::new(0);
    const OTHER: SubjectId = SubjectId::new(1);

    fn store() -> MemoryStore {
        MemoryStore::new(64, 4)
    }

    #[test]
    fn the_footprint_is_the_stride_times_the_capacity() {
        // ADR 0016's claim, checked rather than calculated in prose: the slot array is
        // `capacity x subjects x 16` bytes, with a row-bookkeeping constant on top that
        // does not scale with subjects.
        for subjects in [1_u16, 4, 16, 32, 64] {
            let rows = 10_000_u32;
            let store = MemoryStore::new(rows, subjects);
            let slots = rows as usize * usize::from(subjects) * SLOT_BYTES;
            let overhead = store.footprint() - slots;
            assert_eq!(
                store.subjects(),
                subjects,
                "a store sizes to what it is asked for, not to the ceiling"
            );
            assert!(
                overhead <= rows as usize * 24,
                "{subjects} subjects: {overhead} bytes of bookkeeping exceeds the model"
            );
            eprintln!(
                "{subjects:>2} subjects x {rows} rows: {} bytes ({slots} slots + {overhead} bookkeeping)",
                store.footprint()
            );
        }
    }

    #[test]
    fn sixty_four_subjects_costs_four_times_sixteen() {
        let sixteen = MemoryStore::new(10_000, 16).footprint();
        let sixty_four = MemoryStore::new(10_000, 64).footprint();
        let ratio = sixty_four as f64 / sixteen as f64;
        assert!(
            (3.9..4.1).contains(&ratio),
            "the stride quadruples while the bookkeeping does not: {ratio}"
        );
    }

    fn slot_size_is_pinned() {
        assert_eq!(core::mem::size_of::<MemorySlot>(), SLOT_BYTES);
    }

    #[test]
    fn a_slot_is_sixteen_bytes() {
        slot_size_is_pinned();
    }

    #[test]
    fn enrolling_returns_a_reusable_row_and_fails_when_full() {
        let mut store = MemoryStore::new(2, 2);
        let first = store.enrol(NpcId::unique(1)).or_die();
        let second = store.enrol(NpcId::unique(2)).or_die();
        assert!(
            store.enrol(NpcId::unique(3)).is_none(),
            "capacity is a bound"
        );
        store.release(first);
        let third = store.enrol(NpcId::unique(4)).or_die();
        assert_eq!(
            third, first,
            "the free list is LIFO, so reuse is deterministic"
        );
        assert_eq!(store.len(), 2);
        assert_eq!(store.owner(second), Some(NpcId::unique(2)));
    }

    #[test]
    fn remembering_blends_history_with_news() {
        let mut store = store();
        let index = store.enrol(NpcId::unique(1)).or_die();
        assert!(store.remember(
            index,
            SUBJECT,
            &Encounter::new(EncounterKind::Helped, Tick::ZERO)
        ));

        let slot = store.recall(index, SUBJECT).or_die();
        assert_eq!(slot.valence(), 40, "160 blended into nothing is 40");
        assert_eq!(slot.encounters(), 1);
        assert!(slot.has(EncounterKind::Helped));
        assert!(!slot.has(EncounterKind::Attacked));

        assert!(store.remember(
            index,
            SUBJECT,
            &Encounter::new(EncounterKind::Attacked, Tick::new(10))
        ));
        let slot = store.recall(index, SUBJECT).or_die();
        // 40 * 3 + (-260) = -140, divided by 4 = -35.
        assert_eq!(
            slot.valence(),
            -35,
            "one bad afternoon does not erase a friendship"
        );
        assert_eq!(slot.encounters(), 2);
        assert!(slot.is_grudge());
    }

    #[test]
    fn counts_saturate_instead_of_wrapping() {
        let mut store = store();
        let index = store.enrol(NpcId::unique(1)).or_die();
        for _ in 0..70_000 {
            store.remember(
                index,
                SUBJECT,
                &Encounter::new(EncounterKind::Spoke, Tick::ZERO),
            );
        }
        let slot = store.recall(index, SUBJECT).or_die();
        assert_eq!(slot.encounters(), u16::MAX);
        assert_eq!(slot.familiarity(), u16::MAX);
    }

    #[test]
    fn decay_halves_per_period_and_reads_lazily() {
        let mut store = store();
        let index = store.enrol(NpcId::unique(1)).or_die();
        store.remember(
            index,
            SUBJECT,
            &Encounter::new(EncounterKind::Attacked, Tick::ZERO),
        );
        let slot = store.recall(index, SUBJECT).or_die();
        let decay = DecayPolicy {
            half_life_ticks: 100,
        };
        assert_eq!(decay.valence_at(slot, Tick::new(0)), -65);
        assert_eq!(decay.valence_at(slot, Tick::new(99)), -65, "no halving yet");
        assert_eq!(decay.valence_at(slot, Tick::new(100)), -32, "one halving");
        assert_eq!(decay.valence_at(slot, Tick::new(300)), -8, "three halvings");
        assert_eq!(decay.valence_at(slot, Tick::new(100_000)), 0, "forgotten");
        assert_eq!(
            DecayPolicy::NONE.valence_at(slot, Tick::new(100_000)),
            -65,
            "a game can decide a grudge is forever"
        );
    }

    #[test]
    fn faded_memory_recalls_as_absent() {
        let mut store = store();
        let index = store.enrol(NpcId::unique(1)).or_die();
        store.remember(
            index,
            SUBJECT,
            &Encounter::new(EncounterKind::Spoke, Tick::ZERO),
        );
        let decay = DecayPolicy {
            half_life_ticks: 10,
        };
        assert!(
            store
                .recall_decayed(index, SUBJECT, Tick::new(0), decay)
                .is_some()
        );
        assert!(
            store
                .recall_decayed(index, SUBJECT, Tick::new(10_000), decay)
                .is_none(),
            "a faded memory is absent, not faint"
        );
    }

    #[test]
    fn an_unknown_index_or_subject_is_refused_not_written() {
        let mut store = store();
        let index = store.enrol(NpcId::unique(1)).or_die();
        assert!(!store.remember(
            index,
            SubjectId::new(9),
            &Encounter::new(EncounterKind::Spoke, Tick::ZERO)
        ));
        assert!(store.recall(index, SubjectId::new(9)).is_none());
        let bogus = MemoryIndex(999);
        assert!(!store.remember(
            bogus,
            SUBJECT,
            &Encounter::new(EncounterKind::Spoke, Tick::ZERO)
        ));
        assert!(store.recall(bogus, SUBJECT).is_none());
    }

    #[test]
    fn release_returns_only_what_was_remembered() {
        let mut store = store();
        let index = store.enrol(NpcId::unique(7)).or_die();
        store.remember(
            index,
            SUBJECT,
            &Encounter::new(EncounterKind::Robbed, Tick::new(5)),
        );
        let memories = store.release(index).or_die();
        assert_eq!(memories.npc, NpcId::unique(7));
        assert_eq!(memories.slots.len(), 1);
        assert_eq!(
            memories.slots.first().map(|(subject, _)| *subject),
            Some(SUBJECT)
        );
        assert_eq!(store.len(), 0);
        assert!(store.recall(index, SUBJECT).is_none());
    }

    #[test]
    fn sweep_frees_only_what_has_faded() {
        let mut store = store();
        let stale = store.enrol(NpcId::unique(1)).or_die();
        let fresh = store.enrol(NpcId::unique(2)).or_die();
        store.remember(
            stale,
            SUBJECT,
            &Encounter::new(EncounterKind::Spoke, Tick::ZERO),
        );
        store.remember(
            fresh,
            SUBJECT,
            &Encounter::new(EncounterKind::Attacked, Tick::new(950)),
        );

        let decay = DecayPolicy {
            half_life_ticks: 100,
        };
        // At tick 1,000 the stale memory has faded to nothing and the fresh one is
        // still half a grudge.
        let freed = store.sweep(Tick::new(1_000), decay, 1);
        assert_eq!(freed, 1);
        assert_eq!(store.len(), 1);
        assert_eq!(store.owner(fresh), Some(NpcId::unique(2)));
        assert!(store.owner(stale).is_none());
    }

    #[test]
    fn archive_evicts_the_least_remembered_deterministically() {
        let mut archive = MemoryArchive::new(2);
        let record = |npc: u32, familiarity: u16| NpcMemories {
            npc: NpcId::unique(npc),
            slots: SmallVec::from_slice(&[(
                SUBJECT,
                MemorySlot {
                    familiarity,
                    ..MemorySlot::EMPTY
                },
            )]),
        };
        assert!(archive.insert(record(1, 10)).is_none());
        assert!(archive.insert(record(2, 50)).is_none());
        let evicted = archive.insert(record(3, 30)).or_die();
        assert_eq!(evicted.npc, NpcId::unique(1), "the faintest memory goes");
        assert!(archive.contains(NpcId::unique(2)));
        assert!(archive.contains(NpcId::unique(3)));
        assert_eq!(archive.len(), 2);
    }

    #[test]
    fn archive_restore_round_trips_a_grudge() {
        let mut store = store();
        let index = store.enrol(NpcId::unique(5)).or_die();
        store.remember(
            index,
            SUBJECT,
            &Encounter::new(EncounterKind::Attacked, Tick::new(40)),
        );
        let mut archive = MemoryArchive::new(4);
        let stored = store.release(index).or_die();
        let evicted = archive.insert(stored);
        assert!(evicted.is_none(), "an empty archive evicts nobody");
        assert!(archive.contains(NpcId::unique(5)));

        let (restored, memories) = archive.restore(NpcId::unique(5), &mut store).or_die();
        assert_eq!(memories.slots.len(), 1);
        let slot = store.recall(restored, SUBJECT).or_die();
        assert_eq!(slot.encounters(), 1);
        assert_eq!(slot.valence(), -65, "the grudge came back at full strength");
        assert!(slot.is_grudge());
        assert!(archive.is_empty());
    }

    #[test]
    fn the_store_is_sized_by_eligibility_not_population() {
        let store = MemoryStore::new(16_384, 8);
        assert_eq!(store.bytes(), 16_384 * 8 * SLOT_BYTES);
        assert_eq!(
            store.bytes(),
            2 * 1024 * 1024,
            "2 MB for 16k NPCs and 8 subjects"
        );
    }

    #[test]
    fn two_stores_fed_identical_events_agree_exactly() {
        let mut left = MemoryStore::new(8, 2);
        let mut right = MemoryStore::new(8, 2);
        let mut left_rows = Vec::new();
        let mut right_rows = Vec::new();
        for npc in 0..4 {
            let a = left.enrol(NpcId::unique(npc)).or_die();
            let b = right.enrol(NpcId::unique(npc)).or_die();
            assert_eq!(a, b, "enrolment order is deterministic");
            left_rows.push(a);
            right_rows.push(b);
        }
        for step in 0..64_u32 {
            let position = (step % 4) as usize;
            let kind = EncounterKind::ALL.at(position % EncounterKind::ALL.len());
            let encounter = Encounter::new(kind, Tick::new(u64::from(step)));
            assert!(left.remember(left_rows.at(position), SUBJECT, &encounter));
            assert!(right.remember(right_rows.at(position), SUBJECT, &encounter));
        }
        let decay = DecayPolicy { half_life_ticks: 5 };
        for raw in 0..4 {
            let index = MemoryIndex(raw);
            let a = left.recall_decayed(index, SUBJECT, Tick::new(500), decay);
            let b = right.recall_decayed(index, SUBJECT, Tick::new(500), decay);
            assert_eq!(a, b);
        }
    }

    #[test]
    fn subject_count_is_clamped_to_the_ceiling() {
        let store = MemoryStore::new(4, 999);
        assert_eq!(store.subjects(), MAX_SUBJECTS);
        let zero = MemoryStore::new(4, 0);
        assert_eq!(
            zero.subjects(),
            1,
            "a store with no subjects would be useless"
        );
    }

    #[test]
    fn enrolment_takes_the_nearest_first_and_breaks_ties_by_identity() {
        let mut candidates = [
            (NpcId::unique(7), 12.0_f32),
            (NpcId::unique(3), 2.0_f32),
            (NpcId::unique(9), 2.0_f32),
            (NpcId::unique(1), 40.0_f32),
        ];
        let kept = select_enrolment(&mut candidates, 3);
        // Two people at 2 m: the lower identity goes first, so a replay agrees.
        assert_eq!(
            kept.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            vec![NpcId::unique(3), NpcId::unique(9), NpcId::unique(7)]
        );
        assert_eq!(select_enrolment(&mut candidates, 0).len(), 0);
        let all = select_enrolment(&mut candidates, 99);
        assert_eq!(all.len(), 4);
        assert_eq!(all.last().map(|(id, _)| *id), Some(NpcId::unique(1)));
    }

    #[test]
    fn enrol_nearest_skips_rows_that_already_exist() {
        let mut store = MemoryStore::new(8, 4);
        let candidates = [(NpcId::unique(1), 1.0_f32), (NpcId::unique(2), 2.0)];
        assert_eq!(store.enrol_nearest(&candidates, 8), 2);
        // A second pass adds nobody: the same crowd seen again is not new work.
        assert_eq!(store.enrol_nearest(&candidates, 8), 0);
        assert_eq!(store.len(), 2);
    }

    #[test]
    fn enrol_nearest_stops_at_the_row_capacity() {
        let mut store = MemoryStore::new(2, 4);
        let candidates = [
            (NpcId::unique(1), 1.0_f32),
            (NpcId::unique(2), 2.0),
            (NpcId::unique(3), 3.0),
        ];
        // Two rows exist, so the third candidate is left out rather than dropped in
        // silence: the count is what tells the caller the crowd outgrew the budget.
        assert_eq!(store.enrol_nearest(&candidates, 8), 2);
        assert_eq!(store.len(), 2);
    }

    #[test]
    fn the_strongest_recall_is_the_familiar_one_and_not_the_merely_recent_one() {
        let mut store = MemoryStore::new(4, 8);
        let index = store.enrol(NpcId::unique(1)).or_die();
        let stranger = SubjectId::new(0);
        let friend = SubjectId::new(3);
        for _ in 0..4 {
            store.remember(
                index,
                stranger,
                &Encounter::new(EncounterKind::Spoke, Tick::new(10)),
            );
        }
        for _ in 0..40 {
            store.remember(
                index,
                friend,
                &Encounter::new(EncounterKind::Traded, Tick::new(10)).with_valence(400),
            );
        }
        let now = Tick::new(20);
        let (subject, valence, familiarity, _flags) = store
            .strongest_recall(index, now, DecayPolicy::MINUTE)
            .or_die();
        assert_eq!(subject, friend);
        assert!(
            familiarity > 30,
            "the friend should dominate: {familiarity}"
        );
        assert!(valence > 0);

        // With no decay allowance at all the answer does not change, but a memory that
        // has faded to nothing disappears rather than reporting a zero.
        let later = Tick::new(20 + 3_600 * 20);
        assert!(
            store
                .strongest_recall(index, later, DecayPolicy::MINUTE)
                .is_none()
        );
    }

    #[test]
    fn strongest_recall_is_none_for_a_row_that_was_never_enrolled() {
        let store = MemoryStore::new(4, 8);
        assert!(
            store
                .strongest_recall(MemoryIndex(0), Tick::new(1), DecayPolicy::NONE)
                .is_none()
        );
    }

    #[test]
    fn remembering_another_subject_does_not_touch_the_first() {
        let mut store = store();
        let index = store.enrol(NpcId::unique(1)).or_die();
        store.remember(
            index,
            SUBJECT,
            &Encounter::new(EncounterKind::Helped, Tick::new(1)),
        );
        store.remember(
            index,
            OTHER,
            &Encounter::new(EncounterKind::Robbed, Tick::new(2)),
        );
        assert_eq!(
            store.recall(index, SUBJECT).map(MemorySlot::valence),
            Some(40)
        );
        assert_eq!(
            store.recall(index, OTHER).map(MemorySlot::valence),
            Some(-45)
        );
        store.forget(index, OTHER);
        assert!(store.recall(index, OTHER).is_none());
        assert!(
            store.recall(index, SUBJECT).is_some(),
            "forgetting one is not forgetting all"
        );
    }
}
