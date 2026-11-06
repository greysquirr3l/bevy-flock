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
use core::cmp::Ordering;
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
    /// The axis-aligned box around every live record's *sphere*, maintained on push and swept on
    /// `retain_live`. Min and max are exact operations, so this can never exclude a record that is
    /// visible: it is a conservative prune, not an approximation.
    bounds: Option<(Vec3, Vec3)>,
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
            bounds: None,
        })
    }

    /// How many records the buffer can hold.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// The record at a ring slot, for an index that stores slots rather than copies.
    #[must_use]
    pub fn get(&self, slot: usize) -> Option<&Stimulus> {
        self.items.get(slot)
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
        self.grow_bounds(stimulus);
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
        // Recomputed rather than shrunk: a box that only grows would keep pruning correct but
        // would slowly stop pruning anything, and the sweep is already O(records).
        let mut bounds: Option<(Vec3, Vec3)> = None;
        for stimulus in &self.items {
            let reach = Vec3::splat(stimulus.radius.max(0.0));
            let (low, high) = (stimulus.position - reach, stimulus.position + reach);
            bounds = Some(match bounds {
                Some((min, max)) => (min.min(low), max.max(high)),
                None => (low, high),
            });
        }
        self.bounds = bounds;
        before - self.items.len()
    }

    /// The box around every live record's sphere, if there is a record at all.
    ///
    /// An observer outside this box expanded by its own reach cannot perceive anything, which is
    /// the cheapest prune in the module: one comparison per observer for a crowd where the action
    /// is in one corner of the level.
    #[must_use]
    pub const fn bounds(&self) -> Option<(Vec3, Vec3)> {
        self.bounds
    }

    fn grow_bounds(&mut self, stimulus: Stimulus) {
        let reach = Vec3::splat(stimulus.radius.max(0.0));
        let low = stimulus.position - reach;
        let high = stimulus.position + reach;
        self.bounds = Some(match self.bounds {
            Some((min, max)) => (min.min(low), max.max(high)),
            None => (low, high),
        });
    }

    /// Forget everything.
    pub fn clear(&mut self) {
        self.items.clear();
        self.cursor = 0;
        self.bounds = None;
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

    /// The largest distance any record can be perceived from under this query.
    ///
    /// A record's own radius can only shorten its reach, never lengthen it, so this is the window
    /// an index must cover to miss nothing.
    #[must_use]
    pub fn max_limit(&self) -> f32 {
        if self.sight_radius.is_finite() && self.hearing_radius.is_finite() {
            self.sight_radius.max(self.hearing_radius).max(0.0)
        } else {
            0.0
        }
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

/// What one observer perceives, scanning the whole buffer.
///
/// The simplest correct implementation, and the reference the indexed path is compared against.
/// It also prunes in O(1): an observer outside the buffer's bounding box expanded by its own reach
/// cannot perceive anything, and in a level where the action is in one corner that is most of the
/// crowd.
///
/// # Panics
///
/// Test one record against one observer and keep it if it wins a slot.
///
/// A macro rather than a function, so the linear and the indexed path share *one* predicate set
/// without a call between them: this runs once per record per observer, two million times in the
/// dense benchmark, and as a function it measured 30% slower because the optimiser declined to
/// inline it at that size. A macro cannot drift the way a copy can, and it needs no
/// `#[inline(always)]` to be free.
///
/// Every predicate lives here and only here: an index decides which records reach it, never what
/// it answers.
macro_rules! consider {
    ($stimulus:expr, $observer:expr, $forward:expr, $now:expr, $query:expr, $occluder:expr, $found:expr) => {{
        'record: {

        if !$stimulus.is_live($now) || $query.max_results == 0 {
                break 'record;
        }
        let limit = match $stimulus.kind {
            StimulusKind::Sight => $query.sight_radius.min($stimulus.radius),
            _ => $query.hearing_radius.min($stimulus.radius),
        };
        // Squared first: most records in a full buffer are out of range, and a comparison saves the
        // square root for the ones that are not.
        let distance_squared = ($stimulus.position - $observer).length_squared();
        if distance_squared > limit * limit {
                break 'record;
        }
        let distance = distance_squared.sqrt();
        if $stimulus.kind.uses_field_of_view() && $query.field_of_view > -1.0 {
            // The cone test without a normalize: comparing the dot against `cos * |offset|` is the
            // same predicate, and saves a square root and a divide per record per $observer.
            let offset = $stimulus.position - $observer;
            if $forward.dot(offset) < $query.field_of_view * distance {
                    break 'record;
            }
        }
        if $stimulus.kind.is_occluded() && $occluder.blocks($observer, $stimulus.position) {
                break 'record;
        }
        let strength = $stimulus.strength_at_distance(distance, $now);
        if strength <= 0.0 {
                break 'record;
        }
        let candidate = Perceived {
            kind: $stimulus.kind,
            position: $stimulus.position,
            distance,
            strength,
        };
        // Bounded insertion rather than collect-then-sort: `max_results` is six by default, and a
        // dense buffer can put thousands of records inside the hearing radius. Keeping the best few
        // as we go costs one comparison per slot and no allocation, where sorting everything cost a
        // sort per $observer and a growth of the result vector the caller never sees.
        let slot = $found.partition_point(|existing| ranks(existing, &candidate) == Ordering::Less);
        if slot >= $query.max_results {
                break 'record;
        }
        if $found.len() == $query.max_results {
            $found.pop();
        }
        $found.insert(slot, candidate);
        }
    }};
}

/// # Panics
///
/// Never: the result is bounded by `query.max_results` and every slot is written through a
/// checked path.
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
    if !buffer_reaches(buffer, observer, query.max_limit()) {
        return found;
    }
    let forward = forward.normalize_or_zero();
    for stimulus in buffer.iter() {
        consider!(
            stimulus, observer, forward, now, query, occluder, &mut found
        );
    }
    found
}

/// What one observer perceives, reading only the records the index says are in range.
///
/// Identical output to [`perceive`> by construction: the index is a candidate filter whose
/// candidate set is a superset of the records that can pass the exact predicates, and the kept
/// slots are chosen by a total order over the records themselves, so the order candidates are
/// visited in cannot change the answer. `the_index_agrees_with_the_linear_scan` holds it to that
/// over a randomised buffer.
#[must_use]
pub fn perceive_indexed(
    buffer: &StimulusBuffer,
    index: &StimulusIndex,
    observer: Vec3,
    forward: Vec3,
    now: Tick,
    query: &PerceptionQuery,
    occluder: &dyn Occluder,
) -> SmallVec<[Perceived; 8]> {
    // When the observer's window covers the grid there is nothing to prune, and the measured
    // answer is that scanning the buffer is 34% faster than walking copied entries. Take that path
    // rather than pretending the index is free: this function is then never slower than
    // [`perceive`], which is what lets a caller default to it.
    if index.prunes_nothing(observer, query.max_limit()) {
        return perceive(buffer, observer, forward, now, query, occluder);
    }
    let mut found = SmallVec::<[Perceived; 8]>::new();
    let forward = forward.normalize_or_zero();
    index.for_each_candidate(observer, query.max_limit(), |stimulus| {
        consider!(
            stimulus, observer, forward, now, query, occluder, &mut found
        );
    });
    found
}

impl StimulusIndex {
    /// Whether an observer's window contains every live record, in which case the index prunes
    /// nothing and the caller should scan the buffer instead.
    ///
    /// Deliberately not "does the window cover the grid": the grid is padded by the biggest record
    /// radius, so a dense world has empty columns the window need not reach, and a grid-shaped test
    /// measured `false` on exactly the case where the index has nothing to offer.
    #[must_use]
    pub fn prunes_nothing(&self, observer: Vec3, limit: f32) -> bool {
        self.live == 0 || self.candidate_count(observer, limit) >= self.live
    }
}

/// A uniform bucket grid over the buffer's live records, rebuilt once per perception pass.
///
/// # Why this is exact, not approximate
///
/// A record is perceived only if `distance(observer, record) <= limit`, and
/// `limit = min(query radius, record radius) <= query.max_limit()`. So every record that can pass
/// the exact predicates has its **centre** inside `observer ± max_limit()`. Insert each record
/// into the single cell containing its centre, and scan every cell overlapping `observer ±
/// max_limit()`: the candidate set is a superset of the perceivable records, with no record
/// visited twice and no predicate weakened. The exact test in [`consider`] then decides, exactly
/// as it does in the linear scan.
///
/// That is the whole bit-identical argument. Three facts carry it:
///
/// 1. **Superset.** Cell coverage is derived from `max_limit`, which bounds every record's own
///    limit, so nothing perceivable is outside the window.
/// 2. **No duplicates.** A record lives in exactly one cell, so a visitor sees it at most once and
///    the bounded insertion cannot count it twice.
/// 3. **Order irrelevance.** The kept slots are chosen by [`ranks`], a total order over the record
///    fields that appear in the result, so the order candidates arrive in cannot change the
///    answer. Bucket traversal is still made canonical (row-major cells, ring order within a
///    cell) so a run is reproducible to the instruction, but correctness does not depend on it.
///
/// The grid is sized from the buffer's bounds, and the cell size grows (by powers of two) if the
/// grid would exceed [`MAX_CELLS`], so a level measured in kilometres cannot allocate a grid
/// measured in metres. Growing the cell only ever adds candidates; it never loses one.
#[derive(Debug, Clone)]
pub struct StimulusIndex {
    origin: Vec3,
    cell: f32,
    cols: u32,
    rows: u32,
    /// CSR offsets: cell `i` owns `entries[starts[i]..starts[i + 1]]`.
    starts: Vec<u32>,
    /// Per-cell write cursors, used only while filling. Kept as a field so a rebuild does not
    /// allocate; the fill must not use `starts` itself, or it destroys the offsets it needs.
    cursors: Vec<u32>,
    /// The records themselves, copied at rebuild. Copies rather than buffer slots because a slot
    /// costs a second indirection per candidate, which measured as 19% on the dense case — the one
    /// case where an index has nothing to prune.
    entries: Vec<Stimulus>,
    live: usize,
}

/// The most cells an index will build before it coarsens the grid instead.
pub const MAX_CELLS: usize = 1 << 16;

impl Default for StimulusIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl StimulusIndex {
    /// An empty index. Buffers are allocated on the first [`rebuild`](Self::rebuild) and reused.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            origin: Vec3::ZERO,
            cell: 1.0,
            cols: 0,
            rows: 0,
            starts: Vec::new(),
            cursors: Vec::new(),
            entries: Vec::new(),
            live: 0,
        }
    }

    /// How many live records the index covers.
    #[must_use]
    pub const fn live(&self) -> usize {
        self.live
    }

    /// The grid's cell size after any coarsening.
    #[must_use]
    pub const fn cell(&self) -> f32 {
        self.cell
    }

    /// The grid's dimensions.
    #[must_use]
    pub const fn grid(&self) -> (u32, u32) {
        (self.cols, self.rows)
    }

    /// Whether the index holds nothing and a caller can skip perception entirely.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.live == 0
    }

    /// Forget every cell, keeping the allocations.
    pub fn clear(&mut self) {
        self.starts.clear();
        self.cursors.clear();
        self.entries.clear();
        self.cols = 0;
        self.rows = 0;
        self.live = 0;
    }

    /// Lay the live records out again. `cell` is the requested cell size; it is raised if the grid
    /// would otherwise exceed [`MAX_CELLS`].
    ///
    /// Rebuild once per perception pass, not per observer: the whole point is that the layout is
    /// shared by every observer due this tick.
    pub fn rebuild(&mut self, buffer: &StimulusBuffer, now: Tick, cell: f32) {
        self.clear();
        let Some((min, max)) = buffer.bounds() else {
            return;
        };
        let mut cell = if cell.is_finite() && cell > 0.0 {
            cell
        } else {
            1.0
        };
        let mut cols = 0_u32;
        let mut rows = 0_u32;
        for _ in 0..32 {
            cols = usize_to_u32((max.x - min.x) / cell).saturating_add(1);
            rows = usize_to_u32((max.z - min.z) / cell).saturating_add(1);
            if (cols as usize) * (rows as usize) <= MAX_CELLS {
                break;
            }
            cell *= 2.0;
        }
        self.origin = Vec3::new(min.x, 0.0, min.z);
        self.cell = cell;
        self.cols = cols;
        self.rows = rows;

        let cells = (cols as usize) * (rows as usize);
        if self.starts.len() < cells + 1 {
            self.starts.resize(cells + 1, 0);
        } else {
            for slot in self.starts.iter_mut().take(cells + 1) {
                *slot = 0;
            }
        }

        // Pass one: count into `starts[i + 1]`, then prefix-sum so `starts[i]` is cell i's start
        // and `starts[i + 1]` its end.
        for stimulus in buffer.iter() {
            if !stimulus.is_live(now) {
                continue;
            }
            self.live += 1;
            if let Some(cell_index) = self.cell_of(stimulus.position)
                && let Some(count) = self.starts.get_mut(cell_index + 1)
            {
                *count += 1;
            }
        }
        for index in 1..=cells {
            let previous = self.starts.get(index - 1).copied().unwrap_or(0);
            if let Some(count) = self.starts.get_mut(index) {
                *count += previous;
            }
        }
        let total = self.starts.get(cells).copied().unwrap_or(0) as usize;
        self.entries.clear();
        self.entries.resize(
            total,
            Stimulus::new(StimulusKind::Custom(0), Vec3::ZERO, 0.0, 0.0, now),
        );
        if self.cursors.len() < cells {
            self.cursors.resize(cells, 0);
        }
        for index in 0..cells {
            let start = self.starts.get(index).copied().unwrap_or(0);
            if let Some(cursor) = self.cursors.get_mut(index) {
                *cursor = start;
            }
        }

        // Pass two in ring order, advancing each cell's cursor: entries land in ring order inside
        // every cell, which is the canonical order the docs promise. The cursors are separate from
        // `starts` because writing through `starts` would destroy the offsets the reader needs.
        for stimulus in buffer.iter() {
            if !stimulus.is_live(now) {
                continue;
            }
            let Some(cell_index) = self.cell_of(stimulus.position) else {
                continue;
            };
            let Some(cursor) = self.cursors.get_mut(cell_index) else {
                continue;
            };
            if let Some(target) = self.entries.get_mut(*cursor as usize) {
                *target = *stimulus;
                *cursor += 1;
            }
        }
    }

    /// Visit every record that could be perceived from `observer`, given the query's largest reach.
    ///
    /// Order is canonical: row-major over cells, ring order inside a cell. A closer look at the
    /// contract: `visit` may be called for a record the exact predicates then reject. It is never
    /// called twice for the same record, and it is never *not* called for a record the exact
    /// predicates would accept.
    pub fn for_each_candidate(&self, observer: Vec3, limit: f32, mut visit: impl FnMut(&Stimulus)) {
        let Some((x0, x1, z0, z1)) = self.window(observer, limit) else {
            return;
        };
        // A window that covers the grid has nothing to prune, and walking cells would pay the
        // per-cell offset lookups for nothing: one flat pass over the entries is the same set,
        // cheaper, and order is irrelevant to the answer (see the type docs).
        if x0 == 0 && z0 == 0 && x1 + 1 == self.cols && z1 + 1 == self.rows {
            for stimulus in &self.entries {
                visit(stimulus);
            }
            return;
        }
        for z in z0..=z1 {
            for x in x0..=x1 {
                let cell_index = (z as usize) * (self.cols as usize) + (x as usize);
                let (Some(&from), Some(&to)) =
                    (self.starts.get(cell_index), self.starts.get(cell_index + 1))
                else {
                    continue;
                };
                if let Some(range) = self.entries.get(from as usize..to as usize) {
                    for stimulus in range {
                        visit(stimulus);
                    }
                }
            }
        }
    }

    /// How many records [`for_each_candidate`](Self::for_each_candidate) would visit.
    ///
    /// The number a test can assert on instead of a wall clock, and the number a game can gate on.
    #[must_use]
    pub fn candidate_count(&self, observer: Vec3, limit: f32) -> usize {
        let Some((x0, x1, z0, z1)) = self.window(observer, limit) else {
            return 0;
        };
        let mut total = 0;
        for z in z0..=z1 {
            for x in x0..=x1 {
                let cell_index = (z as usize) * (self.cols as usize) + (x as usize);
                let (Some(&from), Some(&to)) =
                    (self.starts.get(cell_index), self.starts.get(cell_index + 1))
                else {
                    continue;
                };
                total += (to - from) as usize;
            }
        }
        total
    }

    /// The cells overlapping `observer ± limit`, clamped into the grid.
    ///
    /// Clamping can only add cells (both ends move inward to the nearest edge), so an observer
    /// outside the grid scans the nearest column or row and the exact predicates reject the
    /// candidates. That is cheaper than a special case and cannot lose a record, because every
    /// record is inside the grid by construction.
    fn window(&self, observer: Vec3, limit: f32) -> Option<(u32, u32, u32, u32)> {
        if self.cols == 0 || self.rows == 0 || !limit.is_finite() {
            return None;
        }
        let limit = limit.max(0.0);
        let inv = 1.0 / self.cell;
        let max_x = (self.cols - 1) as f32;
        let max_z = (self.rows - 1) as f32;
        let clamp = |value: f32, high: f32| -> u32 {
            if value.is_nan() {
                0
            } else {
                usize_to_u32(value.max(0.0).min(high))
            }
        };
        let x0 = clamp(((observer.x - limit - self.origin.x) * inv).floor(), max_x);
        let x1 = clamp(((observer.x + limit - self.origin.x) * inv).floor(), max_x);
        let z0 = clamp(((observer.z - limit - self.origin.z) * inv).floor(), max_z);
        let z1 = clamp(((observer.z + limit - self.origin.z) * inv).floor(), max_z);
        Some((x0, x1, z0, z1))
    }

    /// The single cell containing a point, clamped into the grid.
    fn cell_of(&self, point: Vec3) -> Option<usize> {
        if self.cols == 0 || self.rows == 0 {
            return None;
        }
        let inv = 1.0 / self.cell;
        let x = ((point.x - self.origin.x) * inv).floor();
        let z = ((point.z - self.origin.z) * inv).floor();
        if x.is_nan() || z.is_nan() {
            return None;
        }
        let x = usize_to_u32(x.max(0.0).min((self.cols - 1) as f32));
        let z = usize_to_u32(z.max(0.0).min((self.rows - 1) as f32));
        Some((z as usize) * (self.cols as usize) + (x as usize))
    }
}

/// A non-negative, finite float to `u32`, saturating rather than wrapping.
fn usize_to_u32(value: f32) -> u32 {
    if value.is_nan() || value <= 0.0 {
        return 0;
    }
    if value >= u32::MAX as f32 {
        return u32::MAX;
    }
    value as u32
}

/// Whether any record could possibly reach an observer, using the buffer's bounding box.
#[inline]
fn buffer_reaches(buffer: &StimulusBuffer, observer: Vec3, reach: f32) -> bool {
    let Some((min, max)) = buffer.bounds() else {
        return false;
    };
    let edge = Vec3::splat(reach.max(0.0));
    observer
        .max(min - edge)
        .min(max + edge)
        .abs_diff_eq(observer, 1.0e-4)
}

/// The order an observer notices things in: strongest first, then nearest, then by kind and
/// position.
///
/// Every component uses `total_cmp`, so this is a total order and two runs of the same world
/// cannot disagree about a tie. The bounded insertion above depends on it: `partition_point`
/// needs a predicate that is monotone over the slice, which an order with an arbitrary tie-break
/// would not give.
#[inline]
fn ranks(a: &Perceived, b: &Perceived) -> Ordering {
    b.strength
        .total_cmp(&a.strength)
        .then_with(|| a.distance.total_cmp(&b.distance))
        .then_with(|| a.kind.cmp(&b.kind))
        .then_with(|| a.position.x.total_cmp(&b.position.x))
        .then_with(|| a.position.z.total_cmp(&b.position.z))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{GetOrDie, OrDie};

    fn at(x: f32, z: f32) -> Vec3 {
        Vec3::new(x, 0.0, z)
    }

    fn buffer(count: usize) -> StimulusBuffer {
        StimulusBuffer::new(count).or_die()
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
        assert_eq!(heard.at(0).kind, StimulusKind::Sound);
        assert!(heard.at(0).strength > 0.0 && heard.at(0).strength < 1.0);
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
        assert!(!buffer.iter().next().or_die().is_live(later));
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
        assert_eq!(seen.at(0).kind, StimulusKind::Sound);
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
        assert_eq!(seen.at(0).distance, 5.0);
        assert_eq!(seen.at(1).distance, 15.0);
    }

    #[test]
    fn the_bounded_insert_keeps_exactly_the_best_slots() {
        // The bounded insertion replaced a collect-then-sort-then-truncate. This is the parity
        // test for that swap: with distinct strengths, the kept set must be the k strongest in
        // range, in order, and nothing else.
        let mut buffer = buffer(64);
        for index in 0..40_u8 {
            let x = f32::from(index) * 0.5;
            buffer.push(Stimulus::new(
                StimulusKind::Sound,
                at(x, 0.0),
                100.0,
                0.1 + f32::from(index) * 0.02,
                Tick::ZERO,
            ));
        }
        let query = PerceptionQuery::new()
            .with_ranges(100.0, 100.0)
            .with_max_results(4);
        let seen = perceive(&buffer, at(0.0, 0.0), Vec3::X, Tick::ZERO, &query, &OpenAir);
        assert_eq!(seen.len(), 4);
        let mut all: std::vec::Vec<(f32, f32)> = buffer
            .iter()
            .filter_map(|stimulus| {
                let distance = stimulus.position.distance(at(0.0, 0.0));
                let strength = stimulus.strength_at_distance(distance, Tick::ZERO);
                (strength > 0.0).then_some((strength, distance))
            })
            .collect();
        all.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.total_cmp(&b.1)));
        for (index, perceived) in seen.iter().enumerate() {
            let (strength, _) = all.get(index).copied().or_die();
            assert!(
                (perceived.strength - strength).abs() < 1e-6,
                "slot {index}: kept {} but the reference has {strength}",
                perceived.strength
            );
        }
        assert!(
            seen.windows(2).all(|pair| {
                let [a, b] = pair else { return true };
                ranks(a, b) != Ordering::Greater
            }),
            "the kept slots are not in perception order"
        );
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

    /// A tiny deterministic generator: the tests need spread-out pseudo-random positions, and a
    /// fixed sequence is what makes a failure reproducible.
    fn lcg(seed: &mut u32) -> f32 {
        *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        ((*seed >> 8) & 0xffff) as f32 / 65_535.0
    }

    fn scattered(count: usize, extent: f32, seed: u32) -> StimulusBuffer {
        let mut buffer = buffer(count.max(1));
        let mut state = seed;
        for _ in 0..count {
            let x = (lcg(&mut state) - 0.5) * extent;
            let z = (lcg(&mut state) - 0.5) * extent;
            let kind = if lcg(&mut state) < 0.33 {
                StimulusKind::Sound
            } else {
                StimulusKind::Sight
            };
            let radius = 2.0 + lcg(&mut state) * 40.0;
            let intensity = 0.2 + lcg(&mut state) * 0.8;
            let emitted = Tick::new((lcg(&mut state) * 5.0) as u64);
            buffer.push(Stimulus::new(kind, at(x, z), radius, intensity, emitted));
        }
        buffer
    }

    fn same(left: &[Perceived], right: &[Perceived]) -> bool {
        left.len() == right.len()
            && left.iter().zip(right).all(|(a, b)| {
                a.kind == b.kind
                    && a.position == b.position
                    && a.distance.total_cmp(&b.distance) == Ordering::Equal
                    && a.strength.total_cmp(&b.strength) == Ordering::Equal
            })
    }

    #[test]
    fn the_index_agrees_with_the_linear_scan() {
        // The parity test the bit-identical claim rests on: randomised world, randomised observers
        // (including ones outside the bounds and ones with nothing in range), both paths compared
        // element for element.
        let buffer = scattered(600, 300.0, 0x5eed);
        let mut index = StimulusIndex::new();
        index.rebuild(&buffer, Tick::ZERO, 12.0);
        assert_eq!(index.live(), 600);
        let query = PerceptionQuery::new();
        let mut state = 0xabcd_u32;
        let mut compared = 0_usize;
        for _ in 0..400 {
            let observer = at(
                (lcg(&mut state) - 0.5) * 400.0,
                (lcg(&mut state) - 0.5) * 400.0,
            );
            let forward = Vec3::new(lcg(&mut state) - 0.5, 0.0, lcg(&mut state) - 0.5);
            let linear = perceive(&buffer, observer, forward, Tick::ZERO, &query, &OpenAir);
            let indexed = perceive_indexed(
                &buffer,
                &index,
                observer,
                forward,
                Tick::ZERO,
                &query,
                &OpenAir,
            );
            assert!(
                same(&linear, &indexed),
                "indexed path disagreed at {observer}: {linear:?} vs {indexed:?}"
            );
            compared += linear.len();
        }
        // The sample has to actually observe something, or the test proves nothing.
        assert!(compared > 50, "only {compared} perceptions compared");
    }

    #[test]
    fn the_index_agrees_when_records_were_pushed_in_another_order() {
        // Two buffers with the same records published in different orders must agree, and both
        // must agree with the linear path. This is what makes the total order load-bearing.
        let forward = StimulusBuffer::new(64).or_die();
        let mut forwards = forward;
        let mut backwards = StimulusBuffer::new(64).or_die();
        let records: std::vec::Vec<Stimulus> = (0..40_u8)
            .map(|index| {
                let x = f32::from(index) * 1.5 - 30.0;
                Stimulus::new(StimulusKind::Sound, at(x, x * 0.5), 25.0, 0.5, Tick::ZERO)
            })
            .collect();
        for record in &records {
            forwards.push(*record);
        }
        for record in records.iter().rev() {
            backwards.push(*record);
        }
        let mut index = StimulusIndex::new();
        index.rebuild(&forwards, Tick::ZERO, 8.0);
        let mut other = StimulusIndex::new();
        other.rebuild(&backwards, Tick::ZERO, 8.0);
        let query = PerceptionQuery::new();
        let observer = at(0.0, 0.0);
        let first = perceive_indexed(
            &forwards,
            &index,
            observer,
            Vec3::X,
            Tick::ZERO,
            &query,
            &OpenAir,
        );
        let second = perceive_indexed(
            &backwards,
            &other,
            observer,
            Vec3::X,
            Tick::ZERO,
            &query,
            &OpenAir,
        );
        let linear = perceive(&forwards, observer, Vec3::X, Tick::ZERO, &query, &OpenAir);
        assert_eq!(first.len(), 6, "the sample should fill every slot");
        assert!(same(&first, &second), "insertion order changed the answer");
        assert!(same(&linear, &first), "the two paths disagree");
    }

    #[test]
    fn the_index_visits_far_fewer_candidates_when_the_world_is_spread() {
        // The point of the index, asserted on a count rather than a clock.
        let buffer = scattered(2_000, 600.0, 0x1234);
        let mut index = StimulusIndex::new();
        index.rebuild(&buffer, Tick::ZERO, 10.0);
        let query = PerceptionQuery::new();
        let reach = query.max_limit();
        let observer = at(0.0, 0.0);
        let candidates = index.candidate_count(observer, reach);
        assert!(
            candidates < 2_000 / 8,
            "the index offered {candidates} candidates of 2,000 records"
        );
        // And a coarse grid over the same world still visits every record, so coarsening is safe.
        let mut coarse = StimulusIndex::new();
        coarse.rebuild(&buffer, Tick::ZERO, 1_000.0);
        assert_eq!(coarse.candidate_count(observer, reach), 2_000);
    }

    #[test]
    fn an_observer_outside_the_bounds_perceives_nothing() {
        let mut buffer = buffer(8);
        buffer.push(Stimulus::new(
            StimulusKind::Sound,
            at(100.0, 100.0),
            30.0,
            1.0,
            Tick::ZERO,
        ));
        let query = PerceptionQuery::new();
        assert!(
            perceive(&buffer, at(0.0, 0.0), Vec3::X, Tick::ZERO, &query, &OpenAir).is_empty(),
            "the bounding box did not prune a distant observer"
        );
        // An observer just inside the box keeps working, so the prune is conservative.
        assert_eq!(
            perceive(
                &buffer,
                at(80.0, 100.0),
                Vec3::X,
                Tick::ZERO,
                &query,
                &OpenAir
            )
            .len(),
            1
        );
    }

    #[test]
    fn a_record_with_a_long_radius_is_found_from_a_far_cell() {
        // A shout carries further than the cell it was published in; the window is sized from the
        // query, not from the cell, which is what makes centre-only insertion safe.
        let mut buffer = buffer(4);
        buffer.push(Stimulus::new(
            StimulusKind::Sound,
            at(70.0, 0.0),
            200.0,
            1.0,
            Tick::ZERO,
        ));
        buffer.push(Stimulus::new(
            StimulusKind::Sound,
            at(-70.0, 0.0),
            200.0,
            1.0,
            Tick::ZERO,
        ));
        let mut index = StimulusIndex::new();
        index.rebuild(&buffer, Tick::ZERO, 5.0);
        let query = PerceptionQuery::new().with_ranges(50.0, 80.0);
        let seen = perceive_indexed(
            &buffer,
            &index,
            at(0.0, 0.0),
            Vec3::X,
            Tick::ZERO,
            &query,
            &OpenAir,
        );
        assert_eq!(
            seen.len(),
            2,
            "a long-radius record was lost by the bucketing"
        );
    }

    #[test]
    fn a_huge_level_coarsens_the_grid_instead_of_allocating_it() {
        let mut buffer = buffer(4);
        buffer.push(Stimulus::new(
            StimulusKind::Sound,
            at(0.0, 0.0),
            5.0,
            1.0,
            Tick::ZERO,
        ));
        buffer.push(Stimulus::new(
            StimulusKind::Sound,
            at(50_000.0, 50_000.0),
            5.0,
            1.0,
            Tick::ZERO,
        ));
        let mut index = StimulusIndex::new();
        index.rebuild(&buffer, Tick::ZERO, 1.0);
        let (cols, rows) = index.grid();
        assert!(
            (cols as usize) * (rows as usize) <= MAX_CELLS,
            "grid was {}x{cols} cells",
            index.grid().1
        );
        assert!(index.cell() > 1.0, "the cell size was not coarsened");
    }

    #[test]
    fn rebuilding_twice_allocates_nothing_new() {
        let buffer = scattered(200, 120.0, 0x99);
        let mut index = StimulusIndex::new();
        index.rebuild(&buffer, Tick::ZERO, 6.0);
        let capacity = index.entries.capacity();
        let offsets = index.starts.capacity();
        for _ in 0..4 {
            index.rebuild(&buffer, Tick::ZERO, 6.0);
        }
        assert_eq!(
            index.entries.capacity(),
            capacity,
            "entries grew on a rebuild"
        );
        assert_eq!(
            index.starts.capacity(),
            offsets,
            "offsets grew on a rebuild"
        );
    }

    #[test]
    fn an_expired_record_leaves_the_index_on_the_next_rebuild() {
        let mut buffer = buffer(8);
        buffer.push(Stimulus::new(
            StimulusKind::Sound,
            at(1.0, 0.0),
            20.0,
            1.0,
            Tick::ZERO,
        ));
        // A one-tick shout that has expired by the time the index is rebuilt at tick ten.
        buffer.push(
            Stimulus::new(StimulusKind::Sound, at(2.0, 0.0), 20.0, 1.0, Tick::ZERO).with_ttl(1),
        );
        let mut index = StimulusIndex::new();
        index.rebuild(&buffer, Tick::new(10), 4.0);
        assert_eq!(index.live(), 1, "an expired record was indexed");
        let query = PerceptionQuery::new();
        let seen = perceive_indexed(
            &buffer,
            &index,
            at(0.0, 0.0),
            Vec3::X,
            Tick::new(10),
            &query,
            &OpenAir,
        );
        assert_eq!(seen.len(), 1);
    }

    #[test]
    fn a_zero_capacity_buffer_is_refused() {
        assert!(matches!(StimulusBuffer::new(0), Err(StimulusError::Empty)));
    }
}
