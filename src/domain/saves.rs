//! The relationship save format (ADR 0012).
//!
//! A grudge that does not survive a save is a mood, so memory needs a file. This
//! module is the file: a little-endian binary record, one version, records sorted
//! by identity, times stored as *ages*.
//!
//! ```text
//! header (24)   magic BFLK, version, flags, half-life, subjects, count, crc
//! records       npc, origin, slot_count, then (subject, 16-byte slot) each
//! trailer (8)   crc over everything above, reserved
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
//! # What is not in the file
//!
//! Indices, free lists, enrolment, tiers, weights and tags are derived and
//! recomputed on load. So are names, archetypes and promoted prose, which belong
//! to the game rather than to the store. A loader that trusted a file for any of
//! that would be a loader that a hostile file could steer.
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

/// The only format version this build writes.
pub const FORMAT_VERSION: u16 = 1;

/// Header size in bytes.
const HEADER_BYTES: usize = 24;

/// Trailer size in bytes.
const TRAILER_BYTES: usize = 8;

/// Bytes per slot on disk: sixteen, the same as in memory, written field by field.
const SLOT_BYTES: usize = 16;

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
    /// The file ends before the header, a record or the trailer.
    Truncated {
        /// Byte offset where the reader ran out.
        offset: usize,
    },
    /// A checksum disagreed.
    ChecksumMismatch {
        /// Which part of the file: `header` or `file`.
        section: &'static str,
    },
    /// Records are not in strictly ascending identity order.
    NotCanonical {
        /// Byte offset of the offending record.
        offset: usize,
    },
    /// The file carries a record count or slot count that cannot fit.
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
    /// A subject in the file is beyond the store's stride.
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
            Self::NotCanonical { offset } => write!(f, "records are not sorted at byte {offset}"),
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
        }
    }
}

impl std::error::Error for SaveError {}

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
    const fn default_zero() -> Self {
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
    /// Records, ascending by identity.
    pub records: Vec<SaveRecord>,
}

/// What a restore did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RestoreReport {
    /// Rows re-enrolled into the hot store.
    pub store_records: u32,
    /// Records parked in the archive.
    pub archive_records: u32,
    /// Slots written, across both tiers.
    pub slots: u32,
}

impl MemorySnapshot {
    /// Capture every relationship in a store and an archive.
    ///
    /// `now` is the tick the save is taken at; every stamp becomes an age relative
    /// to it.
    #[must_use]
    pub fn capture(
        store: &MemoryStore,
        archive: &MemoryArchive,
        now: Tick,
        half_life_ticks: u32,
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

    /// Encode to bytes.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.encoded_len());
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
        out.extend_from_slice(&1_u16.to_le_bytes());
        out.extend_from_slice(&self.half_life_ticks.to_le_bytes());
        let subjects = self
            .records
            .iter()
            .flat_map(|record| record.slots.iter().map(|(subject, _)| subject.get()))
            .max()
            .map_or(0_u16, |highest| highest.saturating_add(1));
        out.extend_from_slice(&subjects.to_le_bytes());
        out.extend_from_slice(&(self.records.len() as u32).to_le_bytes());
        out.extend_from_slice(&0_u16.to_le_bytes());
        let crc = crc32(&out);
        out.extend_from_slice(&crc.to_le_bytes());

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

        let crc = crc32(&out);
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&0_u32.to_le_bytes());
        out
    }

    /// Decode from bytes, validating magic, version, counts, ordering and checksums.
    ///
    /// # Errors
    ///
    /// Returns a [`SaveError`] naming the reason and, where useful, the byte
    /// offset. A loader never guesses: an unknown version is a refusal, not a
    /// best-effort parse.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, SaveError> {
        let mut reader = Reader::new(bytes);
        let header = reader.take(HEADER_BYTES)?;

        let magic: [u8; 4] = header
            .get(0..4)
            .and_then(|slice| slice.try_into().ok())
            .ok_or(SaveError::Truncated { offset: 0 })?;
        if magic != MAGIC {
            return Err(SaveError::BadMagic { found: magic });
        }

        let header_crc = read_u32(header, 20).ok_or(SaveError::Truncated { offset: 20 })?;
        let body = header
            .get(0..20)
            .ok_or(SaveError::Truncated { offset: 0 })?;
        if crc32(body) != header_crc {
            return Err(SaveError::ChecksumMismatch { section: "header" });
        }

        let version = read_u16(header, 4).ok_or(SaveError::Truncated { offset: 4 })?;
        if version != FORMAT_VERSION {
            return Err(SaveError::UnsupportedVersion { found: version });
        }
        let flags = read_u16(header, 6).ok_or(SaveError::Truncated { offset: 6 })?;
        if flags & 1 == 0 {
            // v1 only defines age-encoded times; a file without that bit is not
            // one this build wrote, and guessing would re-date every relationship.
            return Err(SaveError::UnsupportedVersion { found: version });
        }
        let half_life_ticks = read_u32(header, 8).ok_or(SaveError::Truncated { offset: 8 })?;
        let record_count = read_u32(header, 14).ok_or(SaveError::Truncated { offset: 14 })?;

        // Before allocating, refuse a count that cannot possibly fit: the smallest
        // record is seven bytes, and the trailer must still be there.
        let remaining = reader.remaining();
        let smallest_records = remaining.saturating_sub(TRAILER_BYTES) / 7;
        if record_count as usize > smallest_records {
            return Err(SaveError::ImplausibleCount {
                count: record_count,
            });
        }

        let mut records = Vec::with_capacity(record_count as usize);
        let mut previous: Option<NpcId> = None;
        for _ in 0..record_count {
            let offset = reader.position();
            let npc_raw = reader.read_u32()?;
            let origin_code = reader.read_u8()?;
            let slot_count = reader.read_u16()?;
            let origin =
                Origin::from_code(origin_code).ok_or(SaveError::NotCanonical { offset })?;
            let npc = NpcId::from_bits(npc_raw);
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

            if (slot_count as usize) > reader.remaining() / 18 {
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
                let slot = SavedSlot {
                    familiarity: reader.read_u16()?,
                    valence: reader.read_i16()?,
                    encounters: reader.read_u16()?,
                    flags: reader.read_u16()?,
                    first_age: reader.read_u32()?,
                    last_age: reader.read_u32()?,
                };
                slots.push((subject, slot));
            }
            records.push(SaveRecord { npc, origin, slots });
        }

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
            records,
        })
    }

    /// Move every age forward by `elapsed` ticks, as if the world had kept running.
    ///
    /// Decay is frozen across sessions by default (ADR 0012): a grudge does not
    /// fade because the player went to bed. A game that wants the world to have
    /// moved on calls this once, with however much simulated time should have
    /// passed, and is then explicit about the choice.
    pub fn rebase_offline(&mut self, elapsed: u32) {
        for record in &mut self.records {
            for (_, slot) in &mut record.slots {
                slot.first_age = slot.first_age.saturating_add(elapsed);
                slot.last_age = slot.last_age.saturating_add(elapsed);
            }
        }
    }

    /// Put every record back, at `now`, without partially mutating on failure.
    ///
    /// # Errors
    ///
    /// Returns [`SaveError::SubjectOutOfRange`] or [`SaveError::Capacity`] if the
    /// store cannot hold the file. Nothing is written in that case.
    pub fn restore(
        self,
        store: &mut MemoryStore,
        archive: &mut MemoryArchive,
        now: Tick,
    ) -> Result<RestoreReport, SaveError> {
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
        for record in &self.records {
            for (subject, _) in &record.slots {
                if subject.get() >= store.subjects() {
                    return Err(SaveError::SubjectOutOfRange {
                        subject: subject.get(),
                        subjects: store.subjects(),
                    });
                }
            }
        }

        let mut report = RestoreReport::default();
        for record in self.records {
            let slots = record.slots.len() as u32;
            match record.origin {
                Origin::Store => {
                    let Some(index) = store.enrol(record.npc) else {
                        // Capacity was checked above, so this means the store was
                        // mutated between the check and here; treat it as capacity
                        // rather than silently dropping a relationship.
                        return Err(SaveError::Capacity { needed, available });
                    };
                    for (subject, slot) in &record.slots {
                        store.implant(index, *subject, slot.to_slot(now));
                    }
                    report.store_records += 1;
                }
                Origin::Archive => {
                    let memories = NpcMemories {
                        npc: record.npc,
                        slots: record
                            .slots
                            .iter()
                            .map(|(subject, slot)| (*subject, slot.to_slot(now)))
                            .collect(),
                    };
                    archive.insert(memories);
                    report.archive_records += 1;
                }
            }
            report.slots += slots;
        }
        Ok(report)
    }

    /// Bytes this snapshot will occupy once encoded.
    #[must_use]
    pub fn encoded_len(&self) -> usize {
        HEADER_BYTES
            + TRAILER_BYTES
            + self
                .records
                .iter()
                .map(|record| 7 + record.slots.len() * (2 + SLOT_BYTES))
                .sum::<usize>()
    }

    /// Whether the snapshot holds nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
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
        let slice = self.take(1)?;
        slice.first().copied().ok_or(SaveError::Truncated {
            offset: self.position,
        })
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
}

fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    let slice = bytes.get(offset..offset + 2)?;
    Some(u16::from_le_bytes(slice.try_into().ok()?))
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    let slice = bytes.get(offset..offset + 4)?;
    Some(u32::from_le_bytes(slice.try_into().ok()?))
}

/// CRC-32 (IEEE 802.3), the checksum the trailer and header carry.
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
        let snapshot = MemorySnapshot::capture(&store, &archive, Tick::new(200), 3_600);
        let bytes = snapshot.to_bytes();
        assert_eq!(bytes.len(), snapshot.encoded_len());

        let decoded = MemorySnapshot::from_bytes(&bytes).expect("valid");
        assert_eq!(decoded, snapshot);

        let mut fresh = MemoryStore::new(16, 4);
        let mut fresh_archive = MemoryArchive::new(4);
        let report = decoded
            .restore(&mut fresh, &mut fresh_archive, Tick::new(200))
            .expect("restores");
        assert_eq!(report.store_records, 2);
        assert_eq!(report.archive_records, 1);
        assert_eq!(report.slots, 4);

        for npc in [7_u32, 9] {
            let index = fresh
                .enrolled()
                .find(|(_, id)| *id == NpcId::unique(npc))
                .map(|(index, _)| index)
                .expect("enrolled");
            for raw in 0..4_u16 {
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
    fn ages_survive_a_clock_that_restarts() {
        let mut store = MemoryStore::new(4, 2);
        let index = store.enrol(NpcId::unique(1)).expect("room");
        store.remember(
            index,
            SUBJECT,
            &Encounter::new(EncounterKind::Attacked, Tick::new(900)),
        );
        let snapshot =
            MemorySnapshot::capture(&store, &MemoryArchive::new(1), Tick::new(1_000), 100);
        let bytes = snapshot.to_bytes();

        // Saved at tick 1,000 with a memory 100 ticks old; reloaded into a session
        // whose clock is somewhere else entirely.
        let decoded = MemorySnapshot::from_bytes(&bytes).expect("valid");
        let mut fresh = MemoryStore::new(4, 2);
        let mut archive = MemoryArchive::new(1);
        let now = Tick::new(5_000_000);
        decoded
            .restore(&mut fresh, &mut archive, now)
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
        let a = MemorySnapshot::capture(&store, &archive, Tick::new(200), 3_600).to_bytes();
        let b = MemorySnapshot::capture(&store, &archive, Tick::new(200), 3_600).to_bytes();
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
    fn an_empty_world_saves_to_a_header_and_a_trailer() {
        let snapshot = MemorySnapshot::capture(&store(), &MemoryArchive::new(1), Tick::ZERO, 0);
        let bytes = snapshot.to_bytes();
        assert_eq!(bytes.len(), HEADER_BYTES + TRAILER_BYTES);
        assert_eq!(MemorySnapshot::from_bytes(&bytes).expect("valid"), snapshot);
    }

    #[test]
    fn a_bad_magic_is_refused_by_name() {
        let mut bytes =
            MemorySnapshot::capture(&store(), &MemoryArchive::new(1), Tick::ZERO, 0).to_bytes();
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
        let mut snapshot = MemorySnapshot::capture(&store(), &MemoryArchive::new(1), Tick::ZERO, 0);
        snapshot.half_life_ticks = 0;
        let mut bytes = snapshot.to_bytes();
        // Bump the version and repair the header checksum, as a future writer would.
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
        let bytes = MemorySnapshot::capture(&store, &archive, Tick::new(200), 3_600).to_bytes();
        for cut in [0, 3, 20, bytes.len() - 6, bytes.len() - 1] {
            let short = bytes.get(0..cut).expect("prefix");
            assert!(
                MemorySnapshot::from_bytes(short).is_err(),
                "a {cut}-byte prefix is not a save"
            );
        }
    }

    #[test]
    fn a_flipped_byte_is_caught_by_the_checksum() {
        let (store, archive) = populated();
        let mut bytes = MemorySnapshot::capture(&store, &archive, Tick::new(200), 3_600).to_bytes();
        let middle = bytes.len() / 2;
        if let Some(byte) = bytes.get_mut(middle) {
            *byte ^= 0xFF;
        }
        assert_eq!(
            MemorySnapshot::from_bytes(&bytes),
            Err(SaveError::ChecksumMismatch { section: "file" })
        );
    }

    #[test]
    fn unsorted_or_duplicated_identities_are_refused() {
        let mut snapshot = MemorySnapshot::capture(
            &store_with_memory(),
            &MemoryArchive::new(1),
            Tick::new(100),
            0,
        );
        let record = snapshot.records.first().expect("one").clone();
        snapshot.records.push(record);
        let mut bytes = snapshot.to_bytes();
        // Repair the count and checksums so only the ordering is wrong.
        bytes
            .get_mut(14..18)
            .expect("header")
            .copy_from_slice(&2_u32.to_le_bytes());
        let crc = crc32(bytes.get(0..20).expect("header"));
        bytes
            .get_mut(20..24)
            .expect("header")
            .copy_from_slice(&crc.to_le_bytes());
        let body_end = bytes.len() - TRAILER_BYTES;
        let crc = crc32(bytes.get(0..body_end).expect("body"));
        bytes
            .get_mut(body_end..body_end + 4)
            .expect("trailer")
            .copy_from_slice(&crc.to_le_bytes());
        assert_eq!(
            MemorySnapshot::from_bytes(&bytes),
            Err(SaveError::DuplicateIdentity {
                npc: NpcId::unique(1)
            })
        );
    }

    #[test]
    fn a_store_too_small_refuses_the_file_without_partial_writes() {
        let mut store = MemoryStore::new(4, 4);
        for npc in 0..4 {
            store.enrol(NpcId::unique(npc)).expect("room");
        }
        let snapshot = MemorySnapshot::capture(
            &store_with_memory(),
            &MemoryArchive::new(1),
            Tick::new(100),
            0,
        );
        let mut target = MemoryStore::new(4, 4);
        for npc in 0..4 {
            target.enrol(NpcId::unique(100 + npc)).expect("room");
        }
        let mut archive = MemoryArchive::new(1);
        assert_eq!(
            snapshot.restore(&mut target, &mut archive, Tick::ZERO),
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
        let snapshot = MemorySnapshot::capture(&store, &MemoryArchive::new(1), Tick::new(100), 0);
        let mut narrow = MemoryStore::new(4, 2);
        assert_eq!(
            snapshot.restore(&mut narrow, &mut MemoryArchive::new(1), Tick::ZERO),
            Err(SaveError::SubjectOutOfRange {
                subject: 3,
                subjects: 2
            })
        );
        assert_eq!(narrow.len(), 0, "nothing was written");
    }

    #[test]
    fn the_size_is_the_advertised_size() {
        let (store, archive) = populated();
        let snapshot = MemorySnapshot::capture(&store, &archive, Tick::new(200), 0);
        let slots = snapshot.slot_count() as usize;
        assert_eq!(
            snapshot.encoded_len(),
            32 + snapshot.records.len() * 7 + slots * 18,
            "header, trailer, records, slots"
        );

        // The ADR claims 430 KB for 10,000 NPCs with two relationships each.
        let bulk = MemorySnapshot {
            half_life_ticks: 0,
            records: (0..10_000_u32)
                .map(|npc| SaveRecord {
                    npc: NpcId::unique(npc),
                    origin: Origin::Store,
                    slots: SmallVec::from_slice(&[
                        (SubjectId::new(0), SavedSlot::default_zero()),
                        (SubjectId::new(1), SavedSlot::default_zero()),
                    ]),
                })
                .collect(),
        };
        assert_eq!(bulk.encoded_len(), 430_032);
    }

    #[test]
    fn the_header_records_the_decay_policy_in_force() {
        let snapshot = MemorySnapshot::capture(
            &store_with_memory(),
            &MemoryArchive::new(1),
            Tick::new(100),
            3_600,
        );
        let decoded = MemorySnapshot::from_bytes(&snapshot.to_bytes()).expect("valid");
        assert_eq!(
            decoded.half_life_ticks, 3_600,
            "a save knows how fast it fades"
        );
    }
}
