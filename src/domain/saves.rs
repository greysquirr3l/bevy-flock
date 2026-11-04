//! The relationship save format (ADR 0012, ADR 0013).
//!
//! A grudge that does not survive a save is a mood, so memory needs a file. This
//! module is the file: a little-endian, sectioned binary container, records sorted
//! by identity, times stored as *ages*, and — since version 2 — a subject registry
//! so a save cannot silently attach a memory to the wrong subject.
//!
//! ```text
//! header (24)        magic BFLK, version, flags, half-life, subjects, records,
//!                    section count, crc
//! section table      count × { id u16, reserved u16, offset u32, len u32, crc u32 }
//! section 1          subject registry: count, fingerprint, then key/kind/label
//! section 2          records: npc, origin, slot_count, then (subject, 16-byte slot)
//! trailer (8)        crc over everything above, reserved
//! ```
//!
//! # Times are ages
//!
//! `MemorySlot` stores tick stamps, and a tick stamp means nothing in another
//! session. Saving `saved_at - stamp` instead makes a save/load round trip
//! lossless: a relationship that was a hundred ticks old reloads a hundred ticks
//! old, whatever the new clock says. Decay is therefore *frozen* across sessions —
//! the world is paused while the game is off — and a game that wants the world to
//! have moved on calls [`MemorySnapshot::rebase_offline`], which is a deliberate act
//! rather than a side effect of how bytes were written.
//!
//! # Subjects are matched by key, never by position
//!
//! A dense `SubjectId` is a position in the game's subject list, and positions move:
//! a game that adds a companion, drops a rival, or reorders its cast between
//! releases would otherwise re-point every stored relationship at a different
//! person, silently. Version 2 therefore records a [`SubjectKey`] — a stable
//! identifier the game chooses — for every subject, and the loader translates old
//! positions to current ones by matching keys. Anything the current registry does
//! not know is dropped and *counted*, which is visible in [`LoadReport`] instead of
//! being discovered as a bug report about a stranger who hates the player.
//!
//! # What is not in the file
//!
//! Indices, free lists, enrolment, tiers, weights and tags are derived and
//! recomputed on load. So are names, archetypes and promoted prose, which belong
//! to the game rather than to the store — a label in the registry is for the
//! developer's eyes and for mods, and is never used to match subjects.
//!
//! # The crate produces bytes, not files
//!
//! [`MemorySnapshot::to_bytes`] and [`MemorySnapshot::from_bytes`] are the whole
//! interface. Durability — write a temporary file, fsync, rename — is the game's
//! job, because a simulation crate that opens paths has stopped being one.

use std::fmt;

use smallvec::SmallVec;

use crate::domain::ids::NpcId;
use crate::domain::memory::{
    MemoryArchive, MemoryIndex, MemorySlot, MemoryStore, NpcMemories, SubjectId,
};
use crate::domain::tick::Tick;

/// The four bytes every save starts with.
pub const MAGIC: [u8; 4] = *b"BFLK";

/// The format version this build writes.
pub const FORMAT_VERSION: u16 = 2;

/// The version that wrote a registry-less file. Still readable.
pub const LEGACY_VERSION: u16 = 1;

/// Header size in bytes.
const HEADER_BYTES: usize = 24;

/// Size of one section-table entry.
const TABLE_ENTRY_BYTES: usize = 16;

/// Trailer size in bytes.
const TRAILER_BYTES: usize = 8;

/// Bytes per slot on disk: sixteen, the same as in memory, written field by field.
const SLOT_BYTES: usize = 16;

/// Section identifier: the subject registry.
const SECTION_REGISTRY: u16 = 1;

/// Section identifier: the relationship records.
const SECTION_RECORDS: u16 = 2;

/// Why a save could not be read, or could not be restored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveError {
    /// The file does not begin with [`MAGIC`].
    BadMagic {
        /// The four bytes found instead.
        found: [u8; 4],
    },
    /// The file is a format this build does not know.
    UnsupportedVersion {
        /// The version in the header.
        found: u16,
    },
    /// The file ends before the header, a section, a record or the trailer.
    Truncated {
        /// Byte offset where the reader ran out.
        offset: usize,
    },
    /// A checksum disagreed.
    ChecksumMismatch {
        /// Which part of the file: `header`, `file`, or a section id.
        section: &'static str,
    },
    /// A section's table entry points outside the file.
    SectionOutOfBounds {
        /// The section identifier.
        id: u16,
    },
    /// The same section identifier appears twice in the table.
    DuplicateSection {
        /// The section identifier.
        id: u16,
    },
    /// A section's contents do not parse.
    MalformedSection {
        /// The section identifier.
        id: u16,
    },
    /// Records or subjects are not in strictly ascending order.
    NotCanonical {
        /// Byte offset of the offending entry.
        offset: usize,
    },
    /// The file carries a count that cannot fit in the space it has.
    ImplausibleCount {
        /// The count that could not fit.
        count: u32,
    },
    /// The store cannot hold every record the file describes.
    Capacity {
        /// Rows needed.
        needed: u32,
        /// Rows available.
        available: u32,
    },
    /// A subject in the file has no counterpart in the current registry.
    SubjectOutOfRange {
        /// The subject found.
        subject: u16,
        /// Subjects the store supports.
        subjects: u16,
    },
    /// A record in the file repeats an identity.
    DuplicateIdentity {
        /// The identity that appeared twice.
        npc: NpcId,
    },
    /// A version 1 file was loaded without a policy for its missing registry.
    MissingSubjectRegistry {
        /// The file's format version.
        version: u16,
    },
}

impl fmt::Display for SaveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadMagic { found } => write!(f, "not a flock save: magic {found:02x?}"),
            Self::UnsupportedVersion { found } => {
                write!(f, "save format version {found} is not supported")
            }
            Self::Truncated { offset } => write!(f, "save ends unexpectedly at byte {offset}"),
            Self::ChecksumMismatch { section } => write!(f, "{section} checksum does not match"),
            Self::SectionOutOfBounds { id } => write!(f, "section {id} points outside the file"),
            Self::DuplicateSection { id } => write!(f, "section {id} appears more than once"),
            Self::MalformedSection { id } => write!(f, "section {id} does not parse"),
            Self::NotCanonical { offset } => write!(f, "entries are not sorted at byte {offset}"),
            Self::ImplausibleCount { count } => {
                write!(f, "count {count} cannot fit in the remaining bytes")
            }
            Self::Capacity { needed, available } => {
                write!(
                    f,
                    "save needs {needed} rows but only {available} are available"
                )
            }
            Self::SubjectOutOfRange { subject, subjects } => {
                write!(
                    f,
                    "subject {subject} is beyond the store's {subjects} subjects"
                )
            }
            Self::DuplicateIdentity { npc } => write!(f, "identity {npc:?} appears more than once"),
            Self::MissingSubjectRegistry { version } => write!(
                f,
                "a version {version} save carries no subject registry, so its subjects cannot be verified"
            ),
        }
    }
}

impl std::error::Error for SaveError {}

/// A stable identifier for a subject, chosen by the game.
///
/// This is the thing that survives a reordering, an insertion or a deletion in the
/// game's cast list. It must be stable across saves and across releases: derive it
/// from authored content (a character id, a row id in a table) rather than from an
/// index, and never reuse it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SubjectKey(u64);

impl SubjectKey {
    /// A key. The game owns the namespace; zero is not special.
    #[must_use]
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    /// The raw value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// What kind of thing a subject is.
///
/// Recorded because a save that says "these five subjects were players" is easier
/// to trust than one that only says "five". It is part of the registry fingerprint,
/// so reclassifying a subject is a mapping change rather than a silent edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum SubjectKind {
    /// A player character.
    Player,
    /// A non-player character.
    Npc,
    /// Anything else: a pet, a vehicle, a faction avatar.
    Other,
}

impl SubjectKind {
    const fn code(self) -> u8 {
        match self {
            Self::Player => 0,
            Self::Npc => 1,
            Self::Other => 2,
        }
    }

    const fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Self::Player),
            1 => Some(Self::Npc),
            2 => Some(Self::Other),
            _ => None,
        }
    }
}

/// One registered subject: what it is called in the game, for matching and for
/// reading a save by hand.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SubjectEntry {
    /// The stable identifier that matching uses.
    pub key: SubjectKey,
    /// What kind of thing it is.
    pub kind: SubjectKind,
    /// A human-readable label. Never used to match; empty is fine.
    pub label: String,
}

impl SubjectEntry {
    /// An entry with no label.
    #[must_use]
    pub fn new(key: SubjectKey, kind: SubjectKind) -> Self {
        Self {
            key,
            kind,
            label: String::new(),
        }
    }

    /// An entry with a label.
    #[must_use]
    pub fn labelled(key: SubjectKey, kind: SubjectKind, label: impl Into<String>) -> Self {
        Self {
            key,
            kind,
            label: label.into(),
        }
    }
}

/// The subjects a game knows about, in dense identifier order.
///
/// Position *is* the [`SubjectId`]: entry zero is subject zero. That is the whole
/// reason this exists — position is convenient at runtime and meaningless in a
/// file, so the file carries the keys and the loader translates.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SubjectRegistry {
    entries: Vec<SubjectEntry>,
}

impl SubjectRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a subject, returning its dense identifier.
    pub fn register(&mut self, entry: SubjectEntry) -> SubjectId {
        self.entries.push(entry);
        SubjectId::new((self.entries.len() - 1) as u16)
    }

    /// How many subjects are registered.
    #[must_use]
    pub fn len(&self) -> u16 {
        self.entries.len() as u16
    }

    /// Whether no subject is registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The entry at a dense identifier.
    #[must_use]
    pub fn get(&self, subject: SubjectId) -> Option<&SubjectEntry> {
        self.entries.get(usize::from(subject.get()))
    }

    /// Every entry, in identifier order.
    #[must_use]
    pub fn entries(&self) -> &[SubjectEntry] {
        &self.entries
    }

    /// The current identifier for a key, if the registry knows it.
    #[must_use]
    pub fn id_of(&self, key: SubjectKey) -> Option<SubjectId> {
        self.entries
            .iter()
            .position(|entry| entry.key == key)
            .map(|index| SubjectId::new(index as u16))
    }

    /// A checksum over every `(key, kind)` pair in identifier order.
    ///
    /// Two registries with the same fingerprint map identifiers identically, so the
    /// loader can skip translation entirely. This is FNV-1a rather than CRC-32
    /// because it runs over microseconds of data and is not protecting a file
    /// against corruption — the section checksum does that.
    #[must_use]
    pub fn fingerprint(&self) -> u32 {
        let mut hash = 0x811C_9DC5_u32;
        for entry in &self.entries {
            for byte in entry.key.get().to_le_bytes() {
                hash ^= u32::from(byte);
                hash = hash.wrapping_mul(0x0100_0193);
            }
            hash ^= u32::from(entry.kind.code());
            hash = hash.wrapping_mul(0x0100_0193);
        }
        hash
    }
}

impl FromIterator<SubjectEntry> for SubjectRegistry {
    fn from_iter<T: IntoIterator<Item = SubjectEntry>>(iter: T) -> Self {
        let mut registry = Self::new();
        for entry in iter {
            registry.register(entry);
        }
        registry
    }
}

/// What to do with a version 1 save, which has no registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LegacySubjectPolicy {
    /// Refuse the file, because its subjects cannot be verified.
    #[default]
    Refuse,
    /// Trust the file's dense identifiers, as version 1 required. The load report
    /// records that the mapping was not verified, so the decision is visible.
    AssumeDense,
}

/// Which tier a record came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// A row in the hot [`MemoryStore`].
    Store,
    /// An entry in the cold [`MemoryArchive`].
    Archive,
}

impl Origin {
    const fn code(self) -> u8 {
        match self {
            Self::Store => 0,
            Self::Archive => 1,
        }
    }

    const fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Self::Store),
            1 => Some(Self::Archive),
            _ => None,
        }
    }
}

/// One slot as stored: ages rather than stamps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SavedSlot {
    /// Raw familiarity.
    pub familiarity: u16,
    /// Raw valence.
    pub valence: i16,
    /// Encounter count.
    pub encounters: u16,
    /// Encounter kind flags.
    pub flags: u16,
    /// Age of the first encounter, in ticks before the save.
    pub first_age: u32,
    /// Age of the most recent encounter, in ticks before the save.
    pub last_age: u32,
}

impl SavedSlot {
    /// A zeroed slot, for fixtures that care about shape rather than content.
    #[cfg(test)]
    #[must_use]
    const fn zeroed() -> Self {
        Self {
            familiarity: 0,
            valence: 0,
            encounters: 0,
            flags: 0,
            first_age: 0,
            last_age: 0,
        }
    }

    /// Convert a live slot into an age-based one.
    #[must_use]
    pub fn from_slot(slot: MemorySlot, now: Tick) -> Self {
        let now = now.get() as u32;
        Self {
            familiarity: slot.familiarity(),
            valence: slot.valence(),
            encounters: slot.encounters(),
            flags: slot.flags(),
            first_age: now.wrapping_sub(slot.first_seen().get() as u32),
            last_age: now.wrapping_sub(slot.last_seen().get() as u32),
        }
    }

    /// Convert an age-based slot back into a stamp-based one at `now`.
    #[must_use]
    pub fn to_slot(self, now: Tick) -> MemorySlot {
        let now = now.get() as u32;
        MemorySlot::from_parts(
            self.familiarity,
            self.valence,
            self.encounters,
            self.flags,
            Tick::new(u64::from(now.wrapping_sub(self.first_age))),
            Tick::new(u64::from(now.wrapping_sub(self.last_age))),
        )
    }
}

/// One NPC's relationships, ready to write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveRecord {
    /// Whose memory this is.
    pub npc: NpcId,
    /// Which tier it belongs to.
    pub origin: Origin,
    /// Non-empty slots, ascending by subject.
    pub slots: SmallVec<[(SubjectId, SavedSlot); 8]>,
}

/// Every relationship in a world, as bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemorySnapshot {
    /// Half-life of memory decay in ticks; `0` means memory never fades.
    pub half_life_ticks: u32,
    /// The registry the save was written against, when the file carries one.
    ///
    /// `None` means a version 1 file, which has none; see [`LegacySubjectPolicy`].
    pub registry: Option<SubjectRegistry>,
    /// Records, ascending by identity.
    pub records: Vec<SaveRecord>,
}

/// What a restore did, including what it could not do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LoadReport {
    /// Rows re-enrolled into the hot store.
    pub store_records: u32,
    /// Records parked in the archive.
    pub archive_records: u32,
    /// Slots written, across both tiers.
    pub slots: u32,
    /// Slots discarded because their subject is gone from the current registry.
    pub dropped_slots: u32,
    /// Whether subject identifiers had to be translated, because the registry moved.
    pub translated: bool,
    /// Whether the file's subjects were taken on trust (a version 1 file under
    /// [`LegacySubjectPolicy::AssumeDense`]).
    pub unverified_mapping: bool,
}

impl MemorySnapshot {
    /// Capture every relationship in a store and an archive, against a registry.
    ///
    /// `now` is the tick the save is taken at; every stamp becomes an age relative
    /// to it. The registry is written into the file so a future load can tell
    /// whether the subjects still mean what they meant.
    #[must_use]
    pub fn capture(
        store: &MemoryStore,
        archive: &MemoryArchive,
        now: Tick,
        half_life_ticks: u32,
        registry: &SubjectRegistry,
    ) -> Self {
        let mut records = Vec::new();
        for (index, npc) in store.enrolled() {
            if let Some(record) = Self::record_from_store(store, index, npc, now) {
                records.push(record);
            }
        }
        for (npc, memories) in archive.iter() {
            if memories.is_empty() {
                continue;
            }
            let slots = memories
                .slots
                .iter()
                .map(|(subject, slot)| (*subject, SavedSlot::from_slot(*slot, now)))
                .collect();
            records.push(SaveRecord {
                npc: *npc,
                origin: Origin::Archive,
                slots,
            });
        }
        records.sort_by_key(|record| record.npc);
        Self {
            half_life_ticks,
            registry: Some(registry.clone()),
            records,
        }
    }

    fn record_from_store(
        store: &MemoryStore,
        index: MemoryIndex,
        npc: NpcId,
        now: Tick,
    ) -> Option<SaveRecord> {
        let mut slots = SmallVec::<[(SubjectId, SavedSlot); 8]>::new();
        for raw in 0..store.subjects() {
            let subject = SubjectId::new(raw);
            if let Some(slot) = store.recall(index, subject) {
                slots.push((subject, SavedSlot::from_slot(slot, now)));
            }
        }
        (!slots.is_empty()).then_some(SaveRecord {
            npc,
            origin: Origin::Store,
            slots,
        })
    }

    /// How many relationships this save holds.
    #[must_use]
    pub fn slot_count(&self) -> u32 {
        self.records
            .iter()
            .map(|record| record.slots.len() as u32)
            .sum()
    }

    /// The registry written into the file, if any.
    #[must_use]
    pub fn registry(&self) -> Option<&SubjectRegistry> {
        self.registry.as_ref()
    }

    /// Encode to bytes, as format version 2.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let registry_payload = self.registry.as_ref().map(encode_registry);
        let records_payload = self.encode_records();
        let sections: Vec<(u16, Vec<u8>)> = [
            registry_payload.map(|payload| (SECTION_REGISTRY, payload)),
            Some((SECTION_RECORDS, records_payload)),
        ]
        .into_iter()
        .flatten()
        .collect();

        let table_bytes = HEADER_BYTES + sections.len() * TABLE_ENTRY_BYTES;
        let mut out = Vec::with_capacity(
            table_bytes
                + sections
                    .iter()
                    .map(|(_, payload)| payload.len())
                    .sum::<usize>()
                + TRAILER_BYTES,
        );

        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
        out.extend_from_slice(&1_u16.to_le_bytes());
        out.extend_from_slice(&self.half_life_ticks.to_le_bytes());
        let subjects = self.registry.as_ref().map_or(0, SubjectRegistry::len);
        out.extend_from_slice(&subjects.to_le_bytes());
        out.extend_from_slice(&(self.records.len() as u32).to_le_bytes());
        out.extend_from_slice(&(sections.len() as u16).to_le_bytes());
        let crc = crc32(&out);
        out.extend_from_slice(&crc.to_le_bytes());

        let mut offset = table_bytes;
        for (id, payload) in &sections {
            out.extend_from_slice(&id.to_le_bytes());
            out.extend_from_slice(&0_u16.to_le_bytes());
            out.extend_from_slice(&(offset as u32).to_le_bytes());
            out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            out.extend_from_slice(&crc32(payload).to_le_bytes());
            offset += payload.len();
        }
        for (_, payload) in &sections {
            out.extend_from_slice(payload);
        }

        let crc = crc32(&out);
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&0_u32.to_le_bytes());
        out
    }

    fn encode_records(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(
            self.records
                .iter()
                .map(|record| 7 + record.slots.len() * (2 + SLOT_BYTES))
                .sum(),
        );
        for record in &self.records {
            out.extend_from_slice(&record.npc.to_bits().to_le_bytes());
            out.push(record.origin.code());
            out.extend_from_slice(&(record.slots.len() as u16).to_le_bytes());
            for (subject, slot) in &record.slots {
                out.extend_from_slice(&subject.get().to_le_bytes());
                out.extend_from_slice(&slot.familiarity.to_le_bytes());
                out.extend_from_slice(&slot.valence.to_le_bytes());
                out.extend_from_slice(&slot.encounters.to_le_bytes());
                out.extend_from_slice(&slot.flags.to_le_bytes());
                out.extend_from_slice(&slot.first_age.to_le_bytes());
                out.extend_from_slice(&slot.last_age.to_le_bytes());
            }
        }
        out
    }

    /// Decode from bytes, validating magic, version, sections, counts, ordering and
    /// checksums.
    ///
    /// Version 1 files are accepted and produce a snapshot with no registry; the
    /// decision about what to do with such a file is made by
    /// [`MemorySnapshot::restore`], not here.
    ///
    /// # Errors
    ///
    /// Returns a [`SaveError`] naming the reason and, where useful, the byte
    /// offset. A loader never guesses: an unknown version is a refusal, not a
    /// best-effort parse.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, SaveError> {
        let version = read_u16(bytes, 4).ok_or(SaveError::Truncated { offset: 4 })?;
        match version {
            LEGACY_VERSION => Self::from_v1_bytes(bytes),
            FORMAT_VERSION => Self::from_v2_bytes(bytes),
            found => Err(SaveError::UnsupportedVersion { found }),
        }
    }

    fn from_v1_bytes(bytes: &[u8]) -> Result<Self, SaveError> {
        let mut reader = Reader::new(bytes);
        let header = reader.take(HEADER_BYTES)?;
        let header_crc = read_u32(header, 20).ok_or(SaveError::Truncated { offset: 20 })?;
        let body = header
            .get(0..20)
            .ok_or(SaveError::Truncated { offset: 0 })?;
        if crc32(body) != header_crc {
            return Err(SaveError::ChecksumMismatch { section: "header" });
        }
        let half_life_ticks = read_u32(header, 8).ok_or(SaveError::Truncated { offset: 8 })?;
        let record_count = read_u32(header, 14).ok_or(SaveError::Truncated { offset: 14 })?;
        let remaining = reader.remaining();
        if record_count as usize > remaining.saturating_sub(TRAILER_BYTES) / 7 {
            return Err(SaveError::ImplausibleCount {
                count: record_count,
            });
        }
        let records = decode_records(&mut reader, record_count, SECTION_RECORDS)?;
        let trailer_offset = reader.position();
        let file_crc = reader.read_u32()?;
        let _reserved = reader.read_u32()?;
        let covered = bytes.get(0..trailer_offset).ok_or(SaveError::Truncated {
            offset: trailer_offset,
        })?;
        if crc32(covered) != file_crc {
            return Err(SaveError::ChecksumMismatch { section: "file" });
        }
        if reader.remaining() != 0 {
            return Err(SaveError::Truncated {
                offset: reader.position(),
            });
        }
        Ok(Self {
            half_life_ticks,
            registry: None,
            records,
        })
    }

    fn from_v2_bytes(bytes: &[u8]) -> Result<Self, SaveError> {
        let header = bytes
            .get(0..HEADER_BYTES)
            .ok_or(SaveError::Truncated { offset: 0 })?;
        let magic: [u8; 4] = header
            .get(0..4)
            .and_then(|slice| slice.try_into().ok())
            .ok_or(SaveError::Truncated { offset: 0 })?;
        if magic != MAGIC {
            return Err(SaveError::BadMagic { found: magic });
        }
        let header_crc = read_u32(header, 20).ok_or(SaveError::Truncated { offset: 20 })?;
        if crc32(
            header
                .get(0..20)
                .ok_or(SaveError::Truncated { offset: 0 })?,
        ) != header_crc
        {
            return Err(SaveError::ChecksumMismatch { section: "header" });
        }
        let half_life_ticks = read_u32(header, 8).ok_or(SaveError::Truncated { offset: 8 })?;
        let record_count = read_u32(header, 14).ok_or(SaveError::Truncated { offset: 14 })?;
        let section_count = read_u16(header, 18).ok_or(SaveError::Truncated { offset: 18 })?;

        let table_bytes = usize::from(section_count) * TABLE_ENTRY_BYTES;
        let table =
            bytes
                .get(HEADER_BYTES..HEADER_BYTES + table_bytes)
                .ok_or(SaveError::Truncated {
                    offset: HEADER_BYTES,
                })?;
        let body_start = HEADER_BYTES + table_bytes;
        let body_end = bytes
            .len()
            .checked_sub(TRAILER_BYTES)
            .ok_or(SaveError::Truncated {
                offset: bytes.len(),
            })?;

        let sections =
            parse_section_table(table, usize::from(section_count), body_start, body_end)?;

        let mut registry = None;
        let mut records = Vec::new();
        let mut saw_records = false;
        for section in &sections {
            let end = section.offset.saturating_add(section.len);
            let payload = bytes.get(section.offset..end).ok_or(SaveError::Truncated {
                offset: section.offset,
            })?;
            if crc32(payload) != section.checksum {
                return Err(SaveError::ChecksumMismatch {
                    section: match section.id {
                        SECTION_REGISTRY => "registry",
                        SECTION_RECORDS => "records",
                        _ => "unknown",
                    },
                });
            }
            match section.id {
                SECTION_REGISTRY => {
                    registry = Some(decode_registry(payload)?);
                }
                SECTION_RECORDS => {
                    let mut reader = Reader::new(payload);
                    records = decode_records(&mut reader, record_count, SECTION_RECORDS)?;
                    saw_records = true;
                }
                // Unknown sections are skipped on purpose: a newer writer may add
                // them, and refusing to read a save because it contains something
                // this build does not use would make the format impossible to grow.
                _ => {}
            }
        }
        if !saw_records {
            return Err(SaveError::MalformedSection {
                id: SECTION_RECORDS,
            });
        }

        let trailer_crc =
            read_u32(bytes, body_end).ok_or(SaveError::Truncated { offset: body_end })?;
        let covered = bytes
            .get(0..body_end)
            .ok_or(SaveError::Truncated { offset: body_end })?;
        if crc32(covered) != trailer_crc {
            return Err(SaveError::ChecksumMismatch { section: "file" });
        }

        Ok(Self {
            half_life_ticks,
            registry,
            records,
        })
    }

    /// Move every age forward by `elapsed` ticks, as if the world had kept running.
    ///
    /// Decay is frozen across sessions by default: a grudge does not fade because
    /// the player went to bed. A game that wants the world to have moved on calls
    /// this once, with however much simulated time should have passed, and is then
    /// explicit about the choice.
    pub fn rebase_offline(&mut self, elapsed: u32) {
        for record in &mut self.records {
            for (_, slot) in &mut record.slots {
                slot.first_age = slot.first_age.saturating_add(elapsed);
                slot.last_age = slot.last_age.saturating_add(elapsed);
            }
        }
    }

    /// Translate this save's subjects into a current registry.
    ///
    /// Returns one entry per stored subject: the current identifier, or `None` when
    /// the subject no longer exists. A version 1 save has nothing to translate, so
    /// it is entirely trusted or entirely refused, according to `policy`.
    ///
    /// # Errors
    ///
    /// Returns [`SaveError::MissingSubjectRegistry`] for a version 1 file under
    /// [`LegacySubjectPolicy::Refuse`].
    pub fn subject_mapping(
        &self,
        current: &SubjectRegistry,
        policy: LegacySubjectPolicy,
    ) -> Result<(Vec<Option<SubjectId>>, bool), SaveError> {
        let subjects = self
            .records
            .iter()
            .flat_map(|record| record.slots.iter().map(|(subject, _)| subject.get()))
            .max()
            .map_or(0, |highest| usize::from(highest) + 1);

        let Some(written) = self.registry.as_ref() else {
            return match policy {
                LegacySubjectPolicy::Refuse => Err(SaveError::MissingSubjectRegistry {
                    version: LEGACY_VERSION,
                }),
                LegacySubjectPolicy::AssumeDense => Ok((
                    (0..subjects)
                        .map(|raw| Some(SubjectId::new(raw as u16)))
                        .collect(),
                    true,
                )),
            };
        };

        if written.fingerprint() == current.fingerprint() {
            // Same registry, same positions: no translation, no drops, no doubt.
            return Ok((
                (0..subjects)
                    .map(|raw| Some(SubjectId::new(raw as u16)))
                    .collect(),
                false,
            ));
        }

        let mapping = (0..subjects)
            .map(|raw| {
                written
                    .entries()
                    .get(raw)
                    .and_then(|entry| current.id_of(entry.key))
            })
            .collect();
        Ok((mapping, true))
    }

    /// Put every record back, at `now`, translating subjects into `current`.
    ///
    /// # Errors
    ///
    /// Returns a [`SaveError`] if the store cannot hold the file or the file's
    /// subjects cannot be verified. Nothing is written in that case.
    pub fn restore(
        self,
        store: &mut MemoryStore,
        archive: &mut MemoryArchive,
        now: Tick,
        current: &SubjectRegistry,
        policy: LegacySubjectPolicy,
    ) -> Result<LoadReport, SaveError> {
        let (mapping, translated) = self.subject_mapping(current, policy)?;

        let needed = u32::try_from(
            self.records
                .iter()
                .filter(|record| record.origin == Origin::Store)
                .count(),
        )
        .unwrap_or(u32::MAX);
        let available = store.capacity().saturating_sub(store.len());
        if needed > available {
            return Err(SaveError::Capacity { needed, available });
        }
        for (subject, _) in self.records.iter().flat_map(|record| record.slots.iter()) {
            if let Some(Some(target)) = mapping.get(usize::from(subject.get()))
                && target.get() >= store.subjects()
            {
                return Err(SaveError::SubjectOutOfRange {
                    subject: target.get(),
                    subjects: store.subjects(),
                });
            }
        }

        let mut report = LoadReport {
            translated,
            unverified_mapping: self.registry.is_none() && translated,
            ..LoadReport::default()
        };

        for record in self.records {
            let mut slots = SmallVec::<[(SubjectId, MemorySlot); 8]>::new();
            for (subject, slot) in &record.slots {
                match mapping.get(usize::from(subject.get())).copied().flatten() {
                    Some(target) => slots.push((target, slot.to_slot(now))),
                    None => report.dropped_slots += 1,
                }
            }
            if slots.is_empty() {
                // Every subject this NPC knew is gone; writing an empty row would
                // occupy capacity to remember nobody.
                continue;
            }
            match record.origin {
                Origin::Store => {
                    let Some(index) = store.enrol(record.npc) else {
                        return Err(SaveError::Capacity { needed, available });
                    };
                    for (subject, slot) in &slots {
                        store.implant(index, *subject, *slot);
                    }
                    report.store_records += 1;
                }
                Origin::Archive => {
                    archive.insert(NpcMemories {
                        npc: record.npc,
                        slots: slots.iter().copied().collect(),
                    });
                    report.archive_records += 1;
                }
            }
            report.slots += slots.len() as u32;
        }
        Ok(report)
    }

    /// Bytes this snapshot will occupy once encoded.
    ///
    /// Encoding is deterministic, so asking the encoder is both simpler and less
    /// likely to drift than keeping a second size formula in step with it.
    #[must_use]
    pub fn encoded_len(&self) -> usize {
        self.to_bytes().len()
    }

    /// Whether the snapshot holds nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
}

/// One entry of the section table, already bounds-checked against the file.
struct Section {
    id: u16,
    offset: usize,
    len: usize,
    checksum: u32,
}

/// Parse and validate the section table.
///
/// Uniform structure: identifiers ascending, no duplicates, every payload inside the
/// body and none of them overlapping the table or the trailer. A table that says
/// anything else is a malformed file rather than something to interpret generously.
fn parse_section_table(
    table: &[u8],
    count: usize,
    body_start: usize,
    body_end: usize,
) -> Result<Vec<Section>, SaveError> {
    let mut sections = Vec::with_capacity(count);
    let mut previous: Option<u16> = None;
    for index in 0..count {
        let base = index * TABLE_ENTRY_BYTES;
        let id = read_u16(table, base).ok_or(SaveError::Truncated { offset: base })?;
        let offset = read_u32(table, base + 4).ok_or(SaveError::Truncated { offset: base })?;
        let len = read_u32(table, base + 8).ok_or(SaveError::Truncated { offset: base })?;
        let checksum = read_u32(table, base + 12).ok_or(SaveError::Truncated { offset: base })?;
        if previous.is_some_and(|last| id <= last) {
            return Err(if previous == Some(id) {
                SaveError::DuplicateSection { id }
            } else {
                SaveError::NotCanonical { offset: base }
            });
        }
        previous = Some(id);
        let offset = offset as usize;
        let len = len as usize;
        let end = offset
            .checked_add(len)
            .ok_or(SaveError::SectionOutOfBounds { id })?;
        if offset < body_start || end > body_end {
            return Err(SaveError::SectionOutOfBounds { id });
        }
        sections.push(Section {
            id,
            offset,
            len,
            checksum,
        });
    }
    Ok(sections)
}

fn encode_registry(registry: &SubjectRegistry) -> Vec<u8> {
    let mut out = Vec::with_capacity(6 + registry.entries().len() * 11);
    out.extend_from_slice(&registry.len().to_le_bytes());
    out.extend_from_slice(&registry.fingerprint().to_le_bytes());
    out.extend_from_slice(&0_u16.to_le_bytes());
    for entry in registry.entries() {
        out.extend_from_slice(&entry.key.get().to_le_bytes());
        out.push(entry.kind.code());
        let label = entry.label.as_bytes();
        let len = u16::try_from(label.len()).unwrap_or(u16::MAX);
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(label.get(0..usize::from(len)).unwrap_or_default());
    }
    out
}

fn decode_registry(payload: &[u8]) -> Result<SubjectRegistry, SaveError> {
    let mut reader = Reader::new(payload);
    let count = reader.read_u16()?;
    let fingerprint = reader.read_u32()?;
    let _reserved = reader.read_u16()?;
    if usize::from(count) > reader.remaining() / 11 {
        return Err(SaveError::MalformedSection {
            id: SECTION_REGISTRY,
        });
    }
    let mut entries = Vec::with_capacity(usize::from(count));
    for _ in 0..count {
        let key = SubjectKey::new(reader.read_u64()?);
        let kind =
            SubjectKind::from_code(reader.read_u8()?).ok_or(SaveError::MalformedSection {
                id: SECTION_REGISTRY,
            })?;
        let label_len = reader.read_u16()?;
        let label_bytes = reader.take(usize::from(label_len))?;
        let label = std::str::from_utf8(label_bytes)
            .map_err(|_| SaveError::MalformedSection {
                id: SECTION_REGISTRY,
            })?
            .to_owned();
        entries.push(SubjectEntry { key, kind, label });
    }
    let registry = entries.into_iter().collect::<SubjectRegistry>();
    if registry.fingerprint() != fingerprint {
        return Err(SaveError::ChecksumMismatch {
            section: "registry",
        });
    }
    Ok(registry)
}

fn decode_records(
    reader: &mut Reader<'_>,
    count: u32,
    section: u16,
) -> Result<Vec<SaveRecord>, SaveError> {
    if count as usize > reader.remaining() / 7 {
        return Err(SaveError::ImplausibleCount { count });
    }
    let mut records = Vec::with_capacity(count as usize);
    let mut previous: Option<NpcId> = None;
    for _ in 0..count {
        let offset = reader.position();
        let npc = NpcId::from_bits(reader.read_u32()?);
        let origin = Origin::from_code(reader.read_u8()?)
            .ok_or(SaveError::MalformedSection { id: section })?;
        let slot_count = reader.read_u16()?;
        if let Some(last) = previous
            && npc <= last
        {
            return Err(if npc == last {
                SaveError::DuplicateIdentity { npc }
            } else {
                SaveError::NotCanonical { offset }
            });
        }
        previous = Some(npc);
        if usize::from(slot_count) > reader.remaining() / 18 {
            return Err(SaveError::ImplausibleCount {
                count: u32::from(slot_count),
            });
        }
        let mut slots = SmallVec::<[(SubjectId, SavedSlot); 8]>::new();
        let mut last_subject: Option<SubjectId> = None;
        for _ in 0..slot_count {
            let subject = SubjectId::new(reader.read_u16()?);
            if last_subject.is_some_and(|previous| subject <= previous) {
                return Err(SaveError::NotCanonical { offset });
            }
            last_subject = Some(subject);
            slots.push((
                subject,
                SavedSlot {
                    familiarity: reader.read_u16()?,
                    valence: reader.read_i16()?,
                    encounters: reader.read_u16()?,
                    flags: reader.read_u16()?,
                    first_age: reader.read_u32()?,
                    last_age: reader.read_u32()?,
                },
            ));
        }
        records.push(SaveRecord { npc, origin, slots });
    }
    Ok(records)
}

struct Reader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    const fn position(&self) -> usize {
        self.position
    }

    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.position)
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], SaveError> {
        let end = self
            .position
            .checked_add(count)
            .ok_or(SaveError::Truncated {
                offset: self.position,
            })?;
        let slice = self
            .bytes
            .get(self.position..end)
            .ok_or(SaveError::Truncated {
                offset: self.position,
            })?;
        self.position = end;
        Ok(slice)
    }

    fn read_u8(&mut self) -> Result<u8, SaveError> {
        let offset = self.position;
        let slice = self.take(1)?;
        slice
            .first()
            .copied()
            .ok_or(SaveError::Truncated { offset })
    }

    fn read_u16(&mut self) -> Result<u16, SaveError> {
        let offset = self.position;
        let slice = self.take(2)?;
        read_u16(slice, 0).ok_or(SaveError::Truncated { offset })
    }

    fn read_i16(&mut self) -> Result<i16, SaveError> {
        self.read_u16().map(|value| value as i16)
    }

    fn read_u32(&mut self) -> Result<u32, SaveError> {
        let offset = self.position;
        let slice = self.take(4)?;
        read_u32(slice, 0).ok_or(SaveError::Truncated { offset })
    }

    fn read_u64(&mut self) -> Result<u64, SaveError> {
        let offset = self.position;
        let slice = self.take(8)?;
        let bytes: [u8; 8] = slice
            .try_into()
            .map_err(|_| SaveError::Truncated { offset })?;
        Ok(u64::from_le_bytes(bytes))
    }
}

fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    let slice = bytes.get(offset..offset + 2)?;
    Some(u16::from_le_bytes(slice.try_into().ok()?))
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    let slice = bytes.get(offset..offset + 4)?;
    Some(u32::from_le_bytes(slice.try_into().ok()?))
}

/// CRC-32 (IEEE 802.3), the checksum the sections, header and trailer carry.
#[expect(
    clippy::indexing_slicing,
    reason = "the index is masked to 0..=255 and the table has 256 entries"
)]
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFF_u32;
    for byte in bytes {
        let index = ((crc ^ u32::from(*byte)) & 0xFF) as usize;
        crc = (crc >> 8) ^ CRC_TABLE[index];
    }
    !crc
}

/// The standard CRC-32 table, built at compile time so no dependency is needed.
#[expect(
    clippy::indexing_slicing,
    reason = "the index counts 0..256 and the table has 256 entries"
)]
const CRC_TABLE: [u32; 256] = {
    let mut table = [0_u32; 256];
    let mut index = 0;
    while index < 256 {
        let mut value = index as u32;
        let mut bit = 0;
        while bit < 8 {
            value = if value & 1 == 1 {
                (value >> 1) ^ 0xEDB8_8320
            } else {
                value >> 1
            };
            bit += 1;
        }
        table[index] = value;
        index += 1;
    }
    table
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::memory::{DecayPolicy, Encounter, EncounterKind};

    const SUBJECT: SubjectId = SubjectId::new(0);

    fn registry() -> SubjectRegistry {
        SubjectRegistry::new()
            .tap_register(SubjectEntry::labelled(
                SubjectKey::new(0xAAAA),
                SubjectKind::Player,
                "the player",
            ))
            .tap_register(SubjectEntry::labelled(
                SubjectKey::new(0xBBBB),
                SubjectKind::Npc,
                "the captain",
            ))
    }

    /// Small helper so fixtures read top to bottom.
    trait Tap {
        fn tap_register(self, entry: SubjectEntry) -> Self;
    }

    impl Tap for SubjectRegistry {
        fn tap_register(mut self, entry: SubjectEntry) -> Self {
            self.register(entry);
            self
        }
    }

    fn store() -> MemoryStore {
        MemoryStore::new(16, 4)
    }

    fn populated() -> (MemoryStore, MemoryArchive) {
        let mut store = store();
        let index = store.enrol(NpcId::unique(7)).expect("room");
        store.remember(
            index,
            SUBJECT,
            &Encounter::new(EncounterKind::Attacked, Tick::new(100)),
        );
        store.remember(
            index,
            SubjectId::new(1),
            &Encounter::new(EncounterKind::Helped, Tick::new(120)),
        );
        let lonely = store.enrol(NpcId::unique(9)).expect("room");
        store.remember(
            lonely,
            SUBJECT,
            &Encounter::new(EncounterKind::Spoke, Tick::new(90)),
        );
        let mut archive = MemoryArchive::new(4);
        let parked = store.enrol(NpcId::unique(3)).expect("room");
        store.remember(
            parked,
            SUBJECT,
            &Encounter::new(EncounterKind::Robbed, Tick::new(50)),
        );
        archive.insert(store.release(parked).expect("enrolled"));
        (store, archive)
    }

    #[test]
    fn a_round_trip_preserves_every_relationship_exactly() {
        let (store, archive) = populated();
        let registry = registry();
        let snapshot = MemorySnapshot::capture(&store, &archive, Tick::new(200), 3_600, &registry);
        let bytes = snapshot.to_bytes();
        assert_eq!(bytes.len(), snapshot.encoded_len());

        let decoded = MemorySnapshot::from_bytes(&bytes).expect("valid");
        assert_eq!(decoded, snapshot);

        let mut fresh = MemoryStore::new(16, 4);
        let mut fresh_archive = MemoryArchive::new(4);
        let report = decoded
            .restore(
                &mut fresh,
                &mut fresh_archive,
                Tick::new(200),
                &registry,
                LegacySubjectPolicy::Refuse,
            )
            .expect("restores");
        assert_eq!(report.store_records, 2);
        assert_eq!(report.archive_records, 1);
        assert_eq!(report.slots, 4);
        assert_eq!(report.dropped_slots, 0);
        assert!(!report.translated, "the registry did not move");

        for npc in [7_u32, 9] {
            let index = fresh
                .enrolled()
                .find(|(_, id)| *id == NpcId::unique(npc))
                .map(|(index, _)| index)
                .expect("enrolled");
            for raw in 0..2_u16 {
                let subject = SubjectId::new(raw);
                assert_eq!(
                    fresh.recall(index, subject),
                    store
                        .enrolled()
                        .find(|(_, id)| *id == NpcId::unique(npc))
                        .and_then(|(index, _)| store.recall(index, subject)),
                    "npc {npc} subject {raw}"
                );
            }
        }
        assert!(fresh_archive.contains(NpcId::unique(3)));
    }

    #[test]
    fn a_reordered_registry_carries_memories_to_the_right_people() {
        let (store, archive) = populated();
        let written = registry();
        let snapshot = MemorySnapshot::capture(&store, &archive, Tick::new(200), 0, &written);
        let bytes = snapshot.to_bytes();

        // The game dropped the captain and added a stranger, which shifts every
        // position: subject 1 in the file is now the player, and subject 0 is gone.
        let mut current = SubjectRegistry::new();
        let player = current.register(SubjectEntry::new(
            SubjectKey::new(0xAAAA),
            SubjectKind::Player,
        ));
        let stranger =
            current.register(SubjectEntry::new(SubjectKey::new(0xCCCC), SubjectKind::Npc));
        assert_eq!(player, SubjectId::new(0));

        let decoded = MemorySnapshot::from_bytes(&bytes).expect("valid");
        let mut fresh = MemoryStore::new(16, 4);
        let mut fresh_archive = MemoryArchive::new(8);
        let report = decoded
            .restore(
                &mut fresh,
                &mut fresh_archive,
                Tick::new(200),
                &current,
                LegacySubjectPolicy::Refuse,
            )
            .expect("restores");
        assert!(
            report.translated,
            "positions moved, so translation happened"
        );
        assert_eq!(
            report.dropped_slots, 1,
            "the one memory of the captain is dropped, not re-pointed at the stranger"
        );

        // NPC 7 remembered the player hostile (-65) and the captain friendly (40).
        // After translation the player's memory must still be the hostile one.
        let index = fresh
            .enrolled()
            .find(|(_, id)| *id == NpcId::unique(7))
            .map(|(index, _)| index)
            .expect("enrolled");
        let slot = fresh
            .recall(index, player)
            .expect("the player is still remembered");
        assert_eq!(
            slot.valence(),
            -65,
            "the grudge followed the player, not the position"
        );
        assert!(
            fresh.recall(index, stranger).is_none(),
            "the stranger inherits nothing"
        );
    }

    #[test]
    fn an_identical_registry_fingerprint_skips_translation() {
        let (store, archive) = populated();
        let registry = registry();
        let bytes =
            MemorySnapshot::capture(&store, &archive, Tick::new(200), 0, &registry).to_bytes();
        let decoded = MemorySnapshot::from_bytes(&bytes).expect("valid");
        let (mapping, translated) = decoded
            .subject_mapping(&registry, LegacySubjectPolicy::Refuse)
            .expect("mapping");
        assert!(!translated);
        assert_eq!(
            mapping,
            vec![Some(SubjectId::new(0)), Some(SubjectId::new(1))]
        );
    }

    #[test]
    fn a_version_one_file_is_refused_by_default() {
        let (store, archive) = populated();
        let bytes = v1_bytes(&store, &archive, Tick::new(200), 3_600);
        let decoded = MemorySnapshot::from_bytes(&bytes).expect("v1 still reads");
        assert!(decoded.registry().is_none());
        let mut fresh = MemoryStore::new(16, 4);
        assert_eq!(
            decoded.restore(
                &mut fresh,
                &mut MemoryArchive::new(4),
                Tick::new(200),
                &registry(),
                LegacySubjectPolicy::Refuse,
            ),
            Err(SaveError::MissingSubjectRegistry {
                version: LEGACY_VERSION
            })
        );
        assert_eq!(fresh.len(), 0, "nothing was written");
    }

    #[test]
    fn a_version_one_file_loads_on_request_and_says_it_was_not_verified() {
        let (store, archive) = populated();
        let bytes = v1_bytes(&store, &archive, Tick::new(200), 3_600);
        let decoded = MemorySnapshot::from_bytes(&bytes).expect("v1 still reads");
        let mut fresh = MemoryStore::new(16, 4);
        let report = decoded
            .restore(
                &mut fresh,
                &mut MemoryArchive::new(4),
                Tick::new(200),
                &registry(),
                LegacySubjectPolicy::AssumeDense,
            )
            .expect("trusted");
        assert!(report.unverified_mapping, "the caller was told");
        assert_eq!(report.slots, 4);
    }

    #[test]
    fn ages_survive_a_clock_that_restarts() {
        let mut store = MemoryStore::new(4, 2);
        let index = store.enrol(NpcId::unique(1)).expect("room");
        store.remember(
            index,
            SUBJECT,
            &Encounter::new(EncounterKind::Attacked, Tick::new(900)),
        );
        let snapshot = MemorySnapshot::capture(
            &store,
            &MemoryArchive::new(1),
            Tick::new(1_000),
            100,
            &registry(),
        );
        let bytes = snapshot.to_bytes();

        let decoded = MemorySnapshot::from_bytes(&bytes).expect("valid");
        let mut fresh = MemoryStore::new(4, 2);
        let now = Tick::new(5_000_000);
        decoded
            .restore(
                &mut fresh,
                &mut MemoryArchive::new(1),
                now,
                &registry(),
                LegacySubjectPolicy::Refuse,
            )
            .expect("restores");
        let (fresh_index, _) = fresh.enrolled().next().expect("one row");
        let slot = fresh.recall(fresh_index, SUBJECT).expect("remembered");
        assert_eq!(
            slot.last_seen(),
            Tick::new(5_000_000 - 100),
            "the age is what survived"
        );
        assert_eq!(
            DecayPolicy::MINUTE.valence_at(slot, now),
            DecayPolicy::MINUTE.valence_at(
                store.recall(index, SUBJECT).expect("original"),
                Tick::new(1_000)
            ),
            "a save and load does not age the grudge"
        );
    }

    #[test]
    fn offline_time_is_an_explicit_choice() {
        let mut snapshot = MemorySnapshot::capture(
            &store_with_memory(),
            &MemoryArchive::new(1),
            Tick::new(100),
            100,
            &registry(),
        );
        assert_eq!(
            snapshot
                .records
                .first()
                .expect("one")
                .slots
                .first()
                .expect("one")
                .1
                .last_age,
            50
        );
        snapshot.rebase_offline(1_000);
        assert_eq!(
            snapshot
                .records
                .first()
                .expect("one")
                .slots
                .first()
                .expect("one")
                .1
                .last_age,
            1_050,
            "the player was away for a while and the world noticed"
        );
    }

    fn store_with_memory() -> MemoryStore {
        let mut store = MemoryStore::new(4, 2);
        let index = store.enrol(NpcId::unique(1)).expect("room");
        store.remember(
            index,
            SUBJECT,
            &Encounter::new(EncounterKind::Spoke, Tick::new(50)),
        );
        store
    }

    #[test]
    fn encoding_is_canonical_and_repeatable() {
        let (store, archive) = populated();
        let registry = registry();
        let a =
            MemorySnapshot::capture(&store, &archive, Tick::new(200), 3_600, &registry).to_bytes();
        let b =
            MemorySnapshot::capture(&store, &archive, Tick::new(200), 3_600, &registry).to_bytes();
        assert_eq!(a, b, "the same world produces the same bytes");
        let decoded = MemorySnapshot::from_bytes(&a).expect("valid");
        let npcs: Vec<u32> = decoded
            .records
            .iter()
            .map(|record| record.npc.to_bits())
            .collect();
        assert_eq!(
            npcs,
            vec![3, 7, 9],
            "records are sorted by identity, not enrolment"
        );
    }

    #[test]
    fn an_empty_world_saves_to_a_header_a_table_and_a_trailer() {
        let snapshot =
            MemorySnapshot::capture(&store(), &MemoryArchive::new(1), Tick::ZERO, 0, &registry());
        let bytes = snapshot.to_bytes();
        assert_eq!(bytes.len(), snapshot.encoded_len());
        assert_eq!(MemorySnapshot::from_bytes(&bytes).expect("valid"), snapshot);
    }

    #[test]
    fn a_bad_magic_is_refused_by_name() {
        let mut bytes =
            MemorySnapshot::capture(&store(), &MemoryArchive::new(1), Tick::ZERO, 0, &registry())
                .to_bytes();
        if let Some(byte) = bytes.first_mut() {
            *byte = b'X';
        }
        assert_eq!(
            MemorySnapshot::from_bytes(&bytes),
            Err(SaveError::BadMagic { found: *b"XFLK" })
        );
    }

    #[test]
    fn an_unknown_version_is_refused_rather_than_guessed() {
        let mut bytes =
            MemorySnapshot::capture(&store(), &MemoryArchive::new(1), Tick::ZERO, 0, &registry())
                .to_bytes();
        bytes
            .get_mut(4..6)
            .expect("header")
            .copy_from_slice(&9_u16.to_le_bytes());
        let crc = crc32(bytes.get(0..20).expect("header"));
        bytes
            .get_mut(20..24)
            .expect("header")
            .copy_from_slice(&crc.to_le_bytes());
        assert_eq!(
            MemorySnapshot::from_bytes(&bytes),
            Err(SaveError::UnsupportedVersion { found: 9 })
        );
    }

    #[test]
    fn a_truncated_file_is_reported_not_parsed() {
        let (store, archive) = populated();
        let bytes = MemorySnapshot::capture(&store, &archive, Tick::new(200), 3_600, &registry())
            .to_bytes();
        for cut in [0, 3, 20, 40, bytes.len() - 6, bytes.len() - 1] {
            let short = bytes.get(0..cut).expect("prefix");
            assert!(
                MemorySnapshot::from_bytes(short).is_err(),
                "a {cut}-byte prefix is not a save"
            );
        }
    }

    #[test]
    fn a_flipped_byte_is_caught_by_a_section_checksum() {
        let (store, archive) = populated();
        let mut bytes =
            MemorySnapshot::capture(&store, &archive, Tick::new(200), 3_600, &registry())
                .to_bytes();
        // Inside the records payload, which ends eight bytes before the file does.
        let middle = bytes.len() - 40;
        if let Some(byte) = bytes.get_mut(middle) {
            *byte ^= 0xFF;
        }
        match MemorySnapshot::from_bytes(&bytes) {
            Err(SaveError::ChecksumMismatch { section }) => {
                assert!(section == "records" || section == "file", "{section}");
            }
            other => panic!("expected a checksum failure, got {other:?}"),
        }
    }

    #[test]
    fn an_unknown_section_is_skipped_not_fatal() {
        let (store, archive) = populated();
        let snapshot = MemorySnapshot::capture(&store, &archive, Tick::new(200), 0, &registry());
        let extended = bytes_with_extra_section(&snapshot, 200, b"a future writer was here");
        let decoded = MemorySnapshot::from_bytes(&extended).expect("forward compatible");
        assert_eq!(decoded.records, snapshot.records);
    }

    #[test]
    fn unsorted_or_duplicated_identities_are_refused() {
        let mut snapshot = MemorySnapshot::capture(
            &store_with_memory(),
            &MemoryArchive::new(1),
            Tick::new(100),
            0,
            &registry(),
        );
        let record = snapshot.records.first().expect("one").clone();
        snapshot.records.push(record);
        let bytes = snapshot.to_bytes();
        // The writer emits the count from the vector, so the duplicate is inside the
        // records section and the loader must catch it structurally.
        assert_eq!(
            MemorySnapshot::from_bytes(&bytes),
            Err(SaveError::DuplicateIdentity {
                npc: NpcId::unique(1)
            })
        );
    }

    #[test]
    fn a_store_too_small_refuses_the_file_without_partial_writes() {
        let mut target = MemoryStore::new(4, 4);
        for npc in 0..4 {
            target.enrol(NpcId::unique(100 + npc)).expect("room");
        }
        let snapshot = MemorySnapshot::capture(
            &store_with_memory(),
            &MemoryArchive::new(1),
            Tick::new(100),
            0,
            &registry(),
        );
        assert_eq!(
            snapshot.restore(
                &mut target,
                &mut MemoryArchive::new(1),
                Tick::ZERO,
                &registry(),
                LegacySubjectPolicy::Refuse,
            ),
            Err(SaveError::Capacity {
                needed: 1,
                available: 0
            })
        );
        assert_eq!(target.len(), 4, "nothing was written");
    }

    #[test]
    fn a_subject_beyond_the_store_is_refused() {
        let mut store = MemoryStore::new(4, 4);
        let index = store.enrol(NpcId::unique(1)).expect("room");
        store.remember(
            index,
            SubjectId::new(3),
            &Encounter::new(EncounterKind::Spoke, Tick::new(10)),
        );
        let snapshot = MemorySnapshot::capture(
            &store,
            &MemoryArchive::new(1),
            Tick::new(100),
            0,
            &registry(),
        );
        let mut narrow = MemoryStore::new(4, 2);
        assert_eq!(
            snapshot.restore(
                &mut narrow,
                &mut MemoryArchive::new(1),
                Tick::ZERO,
                &registry(),
                LegacySubjectPolicy::Refuse,
            ),
            Err(SaveError::SubjectOutOfRange {
                subject: 3,
                subjects: 2
            })
        );
        assert_eq!(narrow.len(), 0, "nothing was written");
    }

    #[test]
    fn the_header_records_the_decay_policy_in_force() {
        let snapshot = MemorySnapshot::capture(
            &store_with_memory(),
            &MemoryArchive::new(1),
            Tick::new(100),
            3_600,
            &registry(),
        );
        let decoded = MemorySnapshot::from_bytes(&snapshot.to_bytes()).expect("valid");
        assert_eq!(
            decoded.half_life_ticks, 3_600,
            "a save knows how fast it fades"
        );
    }

    #[test]
    fn the_size_is_the_advertised_size() {
        let (store, archive) = populated();
        let snapshot = MemorySnapshot::capture(&store, &archive, Tick::new(200), 0, &registry());
        let bytes = snapshot.to_bytes();
        assert_eq!(bytes.len(), snapshot.encoded_len());

        // ADR 0013 claims 430,094 bytes for 10,000 NPCs with two relationships each
        // and a two-subject registry.
        let bulk = MemorySnapshot {
            half_life_ticks: 0,
            registry: Some(two_subject_registry()),
            records: (0..10_000_u32)
                .map(|npc| SaveRecord {
                    npc: NpcId::unique(npc),
                    origin: Origin::Store,
                    slots: SmallVec::from_slice(&[
                        (SubjectId::new(0), SavedSlot::zeroed()),
                        (SubjectId::new(1), SavedSlot::zeroed()),
                    ]),
                })
                .collect(),
        };
        // 24 header + 32 table + 30 registry + 430,000 records + 8 trailer.
        assert_eq!(bulk.to_bytes().len(), 430_094);
    }

    fn two_subject_registry() -> SubjectRegistry {
        [
            SubjectEntry::new(SubjectKey::new(1), SubjectKind::Player),
            SubjectEntry::new(SubjectKey::new(2), SubjectKind::Npc),
        ]
        .into_iter()
        .collect()
    }

    /// A version 1 file, for the compatibility tests.
    fn v1_bytes(
        store: &MemoryStore,
        archive: &MemoryArchive,
        now: Tick,
        half_life: u32,
    ) -> Vec<u8> {
        let snapshot = MemorySnapshot::capture(store, archive, now, half_life, &registry());
        let mut out = Vec::new();
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&LEGACY_VERSION.to_le_bytes());
        out.extend_from_slice(&1_u16.to_le_bytes());
        out.extend_from_slice(&half_life.to_le_bytes());
        out.extend_from_slice(&registry().len().to_le_bytes());
        out.extend_from_slice(&(snapshot.records.len() as u32).to_le_bytes());
        out.extend_from_slice(&0_u16.to_le_bytes());
        let crc = crc32(&out);
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&snapshot.encode_records());
        let crc = crc32(&out);
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&0_u32.to_le_bytes());
        out
    }

    /// Build a v2 file with an extra, unknown section appended.
    ///
    /// Used to prove that a reader tolerates a section a future writer added, which
    /// is the whole reason the format has a section table at all.
    fn bytes_with_extra_section(snapshot: &MemorySnapshot, id: u16, payload: &[u8]) -> Vec<u8> {
        let registry = snapshot.registry.as_ref().map(encode_registry);
        let records = snapshot.encode_records();
        let mut sections: Vec<(u16, &[u8])> = Vec::new();
        if let Some(registry) = &registry {
            sections.push((SECTION_REGISTRY, registry.as_slice()));
        }
        sections.push((SECTION_RECORDS, records.as_slice()));
        sections.push((id, payload));
        sections.sort_by_key(|(id, _)| *id);

        let table_bytes = HEADER_BYTES + sections.len() * TABLE_ENTRY_BYTES;
        let mut out = Vec::new();
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
        out.extend_from_slice(&1_u16.to_le_bytes());
        out.extend_from_slice(&snapshot.half_life_ticks.to_le_bytes());
        out.extend_from_slice(
            &snapshot
                .registry
                .as_ref()
                .map_or(0_u16, SubjectRegistry::len)
                .to_le_bytes(),
        );
        out.extend_from_slice(&(snapshot.records.len() as u32).to_le_bytes());
        out.extend_from_slice(&(sections.len() as u16).to_le_bytes());
        let header_crc = crc32(&out);
        out.extend_from_slice(&header_crc.to_le_bytes());

        let mut offset = table_bytes;
        for (section_id, body) in &sections {
            out.extend_from_slice(&section_id.to_le_bytes());
            out.extend_from_slice(&0_u16.to_le_bytes());
            out.extend_from_slice(&(offset as u32).to_le_bytes());
            out.extend_from_slice(&(body.len() as u32).to_le_bytes());
            out.extend_from_slice(&crc32(body).to_le_bytes());
            offset += body.len();
        }
        for (_, body) in &sections {
            out.extend_from_slice(body);
        }
        let file_crc = crc32(&out);
        out.extend_from_slice(&file_crc.to_le_bytes());
        out.extend_from_slice(&0_u32.to_le_bytes());
        out
    }
}
