//! Population as density, past the manifest band.
//!
//! ADR 0018. Inside the band, inhabitants are entities with brains and blackboards.
//! Outside it they are a per-cell count and a four-field state vector, advected by the
//! flow field. The cost is O(cells) instead of O(agents), and the price is identity:
//! there is no "villager 4,382" out here, only "forty-one people are walking east".
//!
//! Three properties hold this together, and each has a test rather than a promise:
//!
//! - **Conservation.** Advection moves whole persons between cells on a stride, with the
//!   remainder given to the largest-share neighbour, so the sum is exact and integer.
//!   There is no floating-point population anywhere in this module.
//! - **Determinism.** The identity of the *k*-th person to manifest from cell *c* at tick
//!   *t* is `hash(seed, c, t, k)`. Nothing is stored per NPC, and a replay produces the
//!   same people.
//! - **Continuity.** A manifested person inherits the cell's velocity, alarm and
//!   occupation, so it arrives doing what its neighbours are doing.
//!
//! What this module does **not** do is decide who is anchored. That is a caller
//! invariant (ADR 0018, "Anchoring"): anything a player has met is never folded, so it
//! never enters a cell. This type would happily fold a quest giver into a density field,
//! which is why the caller must not.

use crate::domain::ids::NpcId;
use crate::domain::tick::Tick;

/// A cell's coordinate. Not an index: the field owns the mapping, so the grid can be
/// resized without invalidating anything a caller holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CellId {
    /// Column, increasing east.
    pub x: u32,
    /// Row, increasing north.
    pub y: u32,
}

impl CellId {
    /// Construct a coordinate.
    #[must_use]
    pub const fn new(x: u32, y: u32) -> Self {
        Self { x, y }
    }
}

/// What a cell is doing.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Occupation {
    /// Walking somewhere.
    #[default]
    Commuting = 0,
    /// At work.
    Working = 1,
    /// At market.
    Shopping = 2,
    /// Assembled for a reason.
    Mustering = 3,
}

impl Occupation {
    /// Every occupation, in discriminant order.
    pub const ALL: [Self; 4] = [
        Self::Commuting,
        Self::Working,
        Self::Shopping,
        Self::Mustering,
    ];

    /// A stable name for diagnostics and save files.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Commuting => "commuting",
            Self::Working => "working",
            Self::Shopping => "shopping",
            Self::Mustering => "mustering",
        }
    }

    /// The occupation for a raw discriminant, defaulting to commuting.
    #[must_use]
    pub const fn from_u8(raw: u8) -> Self {
        match raw {
            1 => Self::Working,
            2 => Self::Shopping,
            3 => Self::Mustering,
            _ => Self::Commuting,
        }
    }
}

/// One patch of ambient population.
///
/// Twelve bytes, of which four are the population. A million inhabitants at four-metre
/// spacing over four square kilometres is 40,000 of these, or 480 KB — which is the whole
/// argument for the design in one sentence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CellState {
    /// How many people are here. The conserved quantity.
    population: u32,
    /// Mean velocity in metres per second, quantised to millimetres.
    velocity: [i16; 2],
    /// Alarm in permille, 0..=1000. Raised by violence, decays toward calm.
    alarm: u16,
    /// What they are doing.
    occupation: Occupation,
}

impl CellState {
    /// How many people are in this cell.
    #[must_use]
    pub const fn population(self) -> u32 {
        self.population
    }

    /// Mean velocity, in metres per second.
    #[must_use]
    pub fn velocity(self) -> [f32; 2] {
        [
            f32::from(self.velocity[0]) / 1000.0,
            f32::from(self.velocity[1]) / 1000.0,
        ]
    }

    /// Alarm in permille.
    #[must_use]
    pub const fn alarm(self) -> u16 {
        self.alarm
    }

    /// What the cell is doing.
    #[must_use]
    pub const fn occupation(self) -> Occupation {
        self.occupation
    }

    /// Whether the cell is empty, which is the common case and must stay cheap.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.population == 0
    }

    /// Add people and fold their state into the cell's averages.
    pub fn absorb(&mut self, count: u32, velocity: [f32; 2], alarm: u16, occupation: Occupation) {
        if count == 0 {
            return;
        }
        let before = u64::from(self.population);
        let after = before + u64::from(count);
        // Weighted mean velocity; an empty cell simply adopts the newcomer's.
        self.velocity = mean_velocity(self.velocity, before, velocity, u64::from(count), after);
        // Alarm is a maximum, not an average: one terrified cell is an alarmed cell.
        self.alarm = self.alarm.max(alarm.min(1000));
        if before == 0 {
            self.occupation = occupation;
        }
        self.population = u32::try_from(after).unwrap_or(u32::MAX);
    }
}

/// A velocity field the continuum samples. Navigation's flow field implements this; a
/// uniform wind is enough for tests and for a game with no pathing.
pub trait FlowSample {
    /// The velocity at a world position, in metres per second.
    fn sample(&self, at: [f32; 2]) -> [f32; 2];
}

impl FlowSample for [f32; 2] {
    fn sample(&self, _at: [f32; 2]) -> [f32; 2] {
        *self
    }
}

/// The field itself: a dense grid, sparse in population.
#[derive(Debug, Clone)]
pub struct AmbientField {
    cells: Vec<CellState>,
    width: u32,
    height: u32,
    spacing: f32,
    origin: [f32; 2],
    seed: u64,
    /// Persons leaving each cell to its four neighbours, rebuilt per advection so the
    /// pass allocates nothing.
    outbox: Vec<[u32; 4]>,
}

/// What one advection pass did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AdvectReport {
    /// Persons that crossed a cell boundary.
    pub moved: u64,
    /// Persons that wanted to leave the grid and stayed instead.
    pub clamped: u64,
    /// Cells that held anyone before the pass.
    pub occupied: u32,
}

/// Why a field could not be constructed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldError {
    /// Width or height was zero.
    Empty,
    /// Width times height overflowed a `u32`, or exceeded the sanity ceiling.
    TooLarge,
    /// Spacing was not finite and positive.
    BadSpacing,
}

impl core::fmt::Display for FieldError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let message = match self {
            Self::Empty => "an ambient field needs at least one cell",
            Self::TooLarge => "an ambient field may hold at most 2^22 cells",
            Self::BadSpacing => "cell spacing must be finite and positive",
        };
        f.write_str(message)
    }
}

impl core::error::Error for FieldError {}

impl AmbientField {
    /// The hard ceiling on cells: 4.2 million cells is 64 MB, past which the caller has
    /// built a world, not a crowd.
    pub const MAX_CELLS: u32 = 1 << 22;

    /// Build an empty field.
    ///
    /// # Errors
    ///
    /// [`FieldError`] when the dimensions are degenerate, oversized, or the spacing is
    /// not finite and positive.
    pub fn new(width: u32, height: u32, spacing: f32, seed: u64) -> Result<Self, FieldError> {
        if width == 0 || height == 0 {
            return Err(FieldError::Empty);
        }
        if !spacing.is_finite() || spacing <= 0.0 {
            return Err(FieldError::BadSpacing);
        }
        let Some(cells) = width.checked_mul(height) else {
            return Err(FieldError::TooLarge);
        };
        if cells > Self::MAX_CELLS {
            return Err(FieldError::TooLarge);
        }
        let cells = cells as usize;
        Ok(Self {
            outbox: vec![[0; 4]; cells],
            cells: vec![CellState::default(); cells],
            width,
            height,
            spacing,
            origin: [0.0, 0.0],
            seed,
        })
    }

    /// A one-cell field. Cannot fail, so callers that must not propagate a construction
    /// error have somewhere to stand.
    #[must_use]
    pub fn minimal(spacing: f32, seed: u64) -> Self {
        Self {
            cells: vec![CellState::default()],
            width: 1,
            height: 1,
            spacing: if spacing.is_finite() && spacing > 0.0 {
                spacing
            } else {
                1.0
            },
            origin: [0.0, 0.0],
            seed,
            outbox: vec![[0; 4]],
        }
    }

    /// Move the field's south-west corner. Population is not resampled; the grid clips.
    pub fn set_origin(&mut self, origin: [f32; 2]) {
        self.origin = origin;
    }

    /// Grid width in cells.
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// Grid height in cells.
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// Metres per cell edge.
    #[must_use]
    pub const fn spacing(&self) -> f32 {
        self.spacing
    }

    /// The seed manifestation draws from.
    #[must_use]
    pub const fn seed(&self) -> u64 {
        self.seed
    }

    /// Total population, as a 64-bit sum so an overflow is impossible rather than silent.
    #[must_use]
    pub fn population(&self) -> u64 {
        self.cells
            .iter()
            .map(|cell| u64::from(cell.population()))
            .sum()
    }

    /// Bytes of cell storage, for a budget audit.
    #[must_use]
    pub fn bytes(&self) -> usize {
        self.cells.len() * core::mem::size_of::<CellState>()
    }

    /// Read a cell.
    #[must_use]
    pub fn cell(&self, id: CellId) -> Option<&CellState> {
        self.index(id).and_then(|index| self.cells.get(index))
    }

    /// Read a cell mutably.
    pub fn cell_mut(&mut self, id: CellId) -> Option<&mut CellState> {
        self.index(id).and_then(|index| self.cells.get_mut(index))
    }

    /// Where a world position sits.
    #[must_use]
    pub fn cell_of(&self, at: [f32; 2]) -> Option<CellId> {
        let x = (at[0] - self.origin[0]) / self.spacing;
        let y = (at[1] - self.origin[1]) / self.spacing;
        if !x.is_finite() || !y.is_finite() || x < 0.0 || y < 0.0 {
            return None;
        }
        let (x, y) = (x as u32, y as u32);
        (x < self.width && y < self.height).then_some(CellId::new(x, y))
    }

    /// The centre of a cell in world space.
    #[must_use]
    pub fn centre_of(&self, id: CellId) -> [f32; 2] {
        [
            self.origin[0] + (id.x as f32 + 0.5) * self.spacing,
            self.origin[1] + (id.y as f32 + 0.5) * self.spacing,
        ]
    }

    /// Advect the field one stride.
    ///
    /// Persons move as whole people. The split to neighbours is proportional to the flow
    /// component in each direction, with the remainder given to the largest share, so the
    /// total is preserved exactly and the result is independent of floating-point
    /// association order.
    pub fn advect<F: FlowSample + ?Sized>(
        &mut self,
        flow: &F,
        stride: u32,
        dt: f32,
    ) -> AdvectReport {
        let mut report = AdvectReport::default();
        let seconds = stride as f32 * dt;
        if !seconds.is_finite() || seconds <= 0.0 {
            return report;
        }

        for y in 0..self.height {
            for x in 0..self.width {
                let id = CellId::new(x, y);
                let Some(index) = self.index(id) else {
                    continue;
                };
                let Some(cell) = self.cells.get(index).copied() else {
                    continue;
                };
                if cell.is_empty() {
                    continue;
                }
                report.occupied += 1;

                let velocity = flow.sample(self.centre_of(id));
                // How many persons can leave: at most everyone, and at most one cell
                // width per stride, so a fast flow cannot teleport a crowd.
                let reach = (hypot(velocity) * seconds / self.spacing).min(1.0);
                if !reach.is_finite() || reach <= 0.0 {
                    if let Some(cell) = self.cells.get_mut(index) {
                        cell.alarm = decay_alarm(cell.alarm, stride);
                    }
                    continue;
                }
                let departing = (f64::from(cell.population()) * f64::from(reach)).floor() as u32;
                if departing == 0 {
                    if let Some(cell) = self.cells.get_mut(index) {
                        cell.alarm = decay_alarm(cell.alarm, stride);
                    }
                    continue;
                }

                let shares = split(departing, velocity);
                let mut leaving = 0_u32;
                for (slot, count) in shares.iter().enumerate() {
                    if *count == 0 {
                        continue;
                    }
                    let Some(target) = neighbour(id, slot) else {
                        report.clamped += u64::from(*count);
                        continue;
                    };
                    if target.x >= self.width || target.y >= self.height {
                        report.clamped += u64::from(*count);
                        continue;
                    }
                    if let Some(bucket) = self
                        .outbox
                        .get_mut(index)
                        .and_then(|shares| shares.get_mut(slot))
                    {
                        *bucket = *count;
                        leaving = leaving.saturating_add(*count);
                    }
                }

                if let Some(cell) = self.cells.get_mut(index) {
                    cell.population = cell.population.saturating_sub(leaving);
                    cell.alarm = decay_alarm(cell.alarm, stride);
                }
            }
        }

        // Deliver. Separate pass so a cell that receives does not also donate twice.
        for y in 0..self.height {
            for x in 0..self.width {
                let id = CellId::new(x, y);
                let Some(index) = self.index(id) else {
                    continue;
                };
                let Some(shares) = self.outbox.get(index).copied() else {
                    continue;
                };
                for (slot, count) in shares.iter().enumerate() {
                    if *count == 0 {
                        continue;
                    }
                    let Some(target) = neighbour(id, slot) else {
                        continue;
                    };
                    if let Some(receiver) = self.index(target).and_then(|i| self.cells.get_mut(i)) {
                        receiver.population = receiver.population.saturating_add(*count);
                        report.moved += u64::from(*count);
                    } else if let Some(source) = self.cells.get_mut(index) {
                        // Off-grid: the people stay where they are. Conservation first.
                        source.population = source.population.saturating_add(*count);
                    }
                }
                if let Some(bucket) = self.outbox.get_mut(index) {
                    *bucket = [0; 4];
                }
            }
        }
        report
    }

    /// Take `count` people out of a cell, returning whether they were there.
    ///
    /// Manifestation is this plus [`AmbientField::manifest`]: the caller removes the
    /// people from the density and spawns entities for them. Splitting the two would let
    /// a caller spawn a crowd it never took, which is how population doubles.
    pub fn take(&mut self, id: CellId, count: u32) -> bool {
        let Some(cell) = self.cell_mut(id) else {
            return false;
        };
        if cell.population < count {
            return false;
        }
        cell.population -= count;
        true
    }

    /// Fold people back into a cell, folding their state into its averages.
    pub fn fold(&mut self, id: CellId, count: u32, velocity: [f32; 2], alarm: u16) {
        let occupation = self
            .cell(id)
            .map_or_else(Occupation::default, |cell| cell.occupation());
        if let Some(cell) = self.cell_mut(id) {
            cell.absorb(count, velocity, alarm, occupation);
        }
    }

    /// Raise alarm across a radius, as violence does.
    pub fn raise_alarm(&mut self, centre: [f32; 2], radius: f32, amount: u16) {
        let Some(origin) = self.cell_of([centre[0] - radius, centre[1] - radius]) else {
            return;
        };
        let Some(far) = self.cell_of([centre[0] + radius, centre[1] + radius]) else {
            return;
        };
        for y in origin.y..=far.y {
            for x in origin.x..=far.x {
                if let Some(cell) = self.cell_mut(CellId::new(x, y))
                    && !cell.is_empty()
                {
                    cell.alarm = cell.alarm.saturating_add(amount).min(1000);
                }
            }
        }
    }

    /// The identifiers a cell yields when it manifests, without taking them.
    ///
    /// Pure in `(seed, cell, tick)` and ordinal: the same call twice returns the same
    /// people, and a save file needs to store nothing to reproduce them.
    #[must_use]
    pub fn manifest(&self, id: CellId, count: u32, tick: Tick) -> ManifestIter {
        ManifestIter {
            seed: self.seed,
            cell: id,
            tick: tick.get(),
            remaining: count,
            ordinal: 0,
        }
    }

    fn index(&self, id: CellId) -> Option<usize> {
        if id.x >= self.width || id.y >= self.height {
            return None;
        }
        Some((id.y as usize) * (self.width as usize) + (id.x as usize))
    }
}

/// Identifiers produced by [`AmbientField::manifest`].
#[derive(Debug, Clone)]
pub struct ManifestIter {
    seed: u64,
    cell: CellId,
    tick: u64,
    remaining: u32,
    ordinal: u32,
}

impl Iterator for ManifestIter {
    type Item = NpcId;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        self.remaining -= 1;
        let ordinal = self.ordinal;
        self.ordinal += 1;
        // Four independent 64-bit inputs, mixed once: a collision within a cell would be
        // two NPCs with the same identity, which is the one outcome that cannot be
        // tolerated, so the mix is splitmix64's finaliser rather than a multiply.
        let mixed = mix(self.seed)
            ^ mix(u64::from(self.cell.x) | (u64::from(self.cell.y) << 32))
            ^ mix(self.tick)
            ^ mix(u64::from(ordinal).wrapping_add(0x9E37_79B9_7F4A_7C15));
        let mixed = mix(mixed);
        Some(NpcId::unique(mixed as u32))
    }
}

/// The population-weighted mean of a cell's velocity and a newcomer's, in millimetres.
fn mean_velocity(
    existing: [i16; 2],
    holders: u64,
    newcomer: [f32; 2],
    arrivals: u64,
    total: u64,
) -> [i16; 2] {
    if total == 0 {
        return existing;
    }
    let axis = |index: usize| -> i16 {
        let held = existing.get(index).copied().unwrap_or_default();
        let fresh = newcomer.get(index).copied().unwrap_or_default();
        let weighted = f64::from(held) * holders as f64
            + f64::from((fresh * 1000.0).round() as i32) * arrivals as f64;
        (weighted / total as f64)
            .round()
            .clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16
    };
    [axis(0), axis(1)]
}

fn mix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = x;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

fn hypot(v: [f32; 2]) -> f32 {
    v[0].hypot(v[1])
}

fn decay_alarm(alarm: u16, stride: u32) -> u16 {
    // One permille per stride, floored at calm. Alarm that decays per *stride* rather
    // than per frame is the only version that behaves the same at 30 and 240 Hz.
    let drop = u16::try_from(stride.min(u32::from(u16::MAX))).unwrap_or(u16::MAX);
    alarm.saturating_sub(drop)
}

/// East, west, north, south. The order is load-bearing: the split sums into it.
fn neighbour(id: CellId, slot: usize) -> Option<CellId> {
    match slot {
        0 => id.x.checked_add(1).map(|x| CellId::new(x, id.y)),
        1 => id.x.checked_sub(1).map(|x| CellId::new(x, id.y)),
        2 => id.y.checked_add(1).map(|y| CellId::new(id.x, y)),
        3 => id.y.checked_sub(1).map(|y| CellId::new(id.x, y)),
        _ => None,
    }
}

/// Split `count` persons across the four directions in proportion to the flow, exactly.
fn split(count: u32, velocity: [f32; 2]) -> [u32; 4] {
    let weights = [
        velocity[0].max(0.0),
        (-velocity[0]).max(0.0),
        velocity[1].max(0.0),
        (-velocity[1]).max(0.0),
    ];
    let total: f32 = weights.iter().sum();
    if total <= 0.0 || !total.is_finite() {
        return [0; 4];
    }

    // Integer allocation with the largest remainder: the parts always sum to `count`,
    // which is what makes conservation a property rather than a hope.
    let mut out = [0_u32; 4];
    let mut given = 0_u32;
    let mut remainders = [(0_f64, 0_usize); 4];
    for (slot, weight) in weights.iter().enumerate() {
        let exact = f64::from(count) * f64::from(*weight) / f64::from(total);
        let whole = exact.floor() as u32;
        if let Some(slot_out) = out.get_mut(slot) {
            *slot_out = whole;
        }
        given += whole;
        if let Some(slot_remainder) = remainders.get_mut(slot) {
            *slot_remainder = (exact - f64::from(whole), slot);
        }
    }

    let mut leftover = count.saturating_sub(given);
    remainders.sort_by(|left, right| {
        right
            .0
            .partial_cmp(&left.0)
            .unwrap_or(core::cmp::Ordering::Equal)
    });
    for (_, slot) in remainders {
        if leftover == 0 {
            break;
        }
        if let Some(slot_out) = out.get_mut(slot)
            && (*slot_out > 0 || count > 0)
        {
            *slot_out = slot_out.saturating_add(1);
            leftover = leftover.saturating_sub(1);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{OrDie, OrDieErr};

    fn field(population: u32) -> AmbientField {
        let mut field = AmbientField::new(16, 16, 4.0, 0x5EED).or_die();
        for y in 0..16 {
            for x in 0..16 {
                if let Some(cell) = field.cell_mut(CellId::new(x, y)) {
                    cell.absorb(population, [0.0, 0.0], 0, Occupation::Commuting);
                }
            }
        }
        field
    }

    #[test]
    fn advection_moves_population_downwind_and_conserves_it() {
        let mut field = field(100);
        let before = field.population();
        let report = field.advect(&[2.0, 0.0], 1, 0.5);
        assert!(report.moved > 0, "a wind should move somebody");
        assert_eq!(
            field.population(),
            before,
            "conservation is not a rounding error"
        );
        // The east edge must have lost people to its neighbours; the west edge gained.
        let west = field.cell(CellId::new(0, 8)).or_die().population();
        let east = field.cell(CellId::new(15, 8)).or_die().population();
        // The downwind edge accumulates what the upwind edge cannot replace: it gains
        // from its inner neighbour and loses only to the grid boundary.
        assert!(
            east > west,
            "population flows east: west {west}, east {east}"
        );
        assert_eq!(west, 75, "a quarter of 100 left and nothing arrived");
        assert_eq!(
            east, 125,
            "a quarter arrived and a quarter stayed at the wall"
        );
    }

    #[test]
    fn ten_thousand_strides_conserve_to_the_person() {
        let mut field = field(37);
        let before = field.population();
        // A wandering wind, so flux crosses the grid in every direction.
        let mut state = 0x1234_5678_u64;
        for stride in 0..10_000_u32 {
            state = mix(state);
            let angle = (state % 6283) as f32 / 1000.0;
            let wind = [angle.cos() * 3.0, angle.sin() * 3.0];
            field.advect(&wind, 1, 1.0 / 60.0);
            if stride % 997 == 0 {
                assert_eq!(
                    field.population(),
                    before,
                    "conservation broke at stride {stride}"
                );
            }
        }
        assert_eq!(field.population(), before);
    }

    #[test]
    fn a_million_inhabitants_fit_in_a_megabyte() {
        // 40,000 cells at 4 m spacing is 800 m square; a million people inside it.
        let mut field = AmbientField::new(200, 200, 4.0, 7).or_die();
        for y in 0..200 {
            for x in 0..200 {
                if let Some(cell) = field.cell_mut(CellId::new(x, y)) {
                    cell.absorb(25, [0.0, 0.0], 0, Occupation::Commuting);
                }
            }
        }
        assert_eq!(field.population(), 1_000_000);
        assert_eq!(
            core::mem::size_of::<CellState>(),
            12,
            "the layout claim other documents depend on"
        );
        assert_eq!(field.bytes(), 40_000 * 12);
        assert!(
            field.bytes() <= 1 << 20,
            "the footprint is the whole argument"
        );
    }

    #[test]
    fn manifestations_are_deterministic_and_distinct() {
        let field = field(50);
        let id = CellId::new(3, 4);
        let first: Vec<NpcId> = field.manifest(id, 200, Tick::new(9)).collect();
        let second: Vec<NpcId> = field.manifest(id, 200, Tick::new(9)).collect();
        assert_eq!(first, second, "a replay must produce the same people");

        let mut sorted = first.clone();
        sorted.sort_by_key(|id| id.index());
        sorted.dedup();
        assert_eq!(sorted.len(), first.len(), "two NPCs, one identity");

        let later: Vec<NpcId> = field.manifest(id, 200, Tick::new(10)).collect();
        assert_ne!(first, later, "a different tick is a different cast");
    }

    #[test]
    fn manifestation_from_an_empty_cell_still_yields_the_ordinals() {
        // The iterator is a pure function, so it cannot know the cell is empty. The
        // caller takes first; `take` is what refuses.
        let mut field = field(0);
        assert_eq!(
            field.manifest(CellId::new(0, 0), 3, Tick::new(1)).count(),
            3
        );
        assert!(!field.take(CellId::new(0, 0), 1), "nothing to take");
    }

    #[test]
    fn take_then_fold_conserves_population() {
        let mut field = field(10);
        let before = field.population();
        let cell = CellId::new(5, 5);
        assert!(field.take(cell, 4), "four were there");
        assert_eq!(field.population(), before - 4, "they left the density");
        field.fold(cell, 4, [1.5, 0.0], 200);
        assert_eq!(field.population(), before, "and came back");
        let state = field.cell(cell).or_die();
        assert!(
            (state.velocity()[0] - 0.6).abs() < 0.05,
            "four people at 1.5 m/s into six at rest is 0.6 m/s, got {:?}",
            state.velocity()
        );
    }

    #[test]
    fn alarm_rises_locally_and_decays_by_stride() {
        let mut field = field(5);
        let centre = field.centre_of(CellId::new(8, 8));
        field.raise_alarm(centre, 8.0, 400);
        let alarmed = field.cell(CellId::new(8, 8)).or_die().alarm();
        assert_eq!(alarmed, 400);
        assert_eq!(
            field.cell(CellId::new(0, 0)).or_die().alarm(),
            0,
            "alarm is local"
        );
        field.advect(&[0.0, 0.0], 8, 1.0 / 60.0);
        let after = field.cell(CellId::new(8, 8)).or_die().alarm();
        assert_eq!(after, 392, "eight permille per stride of eight ticks");
    }

    #[test]
    fn a_fast_flow_cannot_teleport_a_crowd() {
        let mut field = field(100);
        let before = field.population();
        // 400 m/s over a stride: the reach clamp is what stops the grid being a blender.
        let report = field.advect(&[400.0, 0.0], 1, 1.0);
        assert_eq!(field.population(), before);
        assert!(
            report.clamped > 0,
            "the east edge holds what it cannot pass"
        );
    }

    #[test]
    fn the_split_always_sums_to_the_whole() {
        for count in [0_u32, 1, 7, 999, 100_000] {
            for velocity in [[1.0, 0.0], [0.0, -1.0], [3.0, 4.0], [-7.0, 2.5]] {
                let parts = split(count, velocity);
                let total: u32 = parts.iter().sum();
                assert_eq!(total, count, "split({count}, {velocity:?}) = {parts:?}");
            }
        }
    }

    #[test]
    fn degenerate_fields_are_refused_not_panicked() {
        assert_eq!(
            AmbientField::new(0, 4, 4.0, 1).or_die_err(),
            FieldError::Empty
        );
        assert_eq!(
            AmbientField::new(4, 4, 0.0, 1).or_die_err(),
            FieldError::BadSpacing
        );
        assert_eq!(
            AmbientField::new(u32::MAX, 2, 4.0, 1).or_die_err(),
            FieldError::TooLarge
        );
    }

    #[test]
    fn world_positions_map_to_cells_and_back() {
        let mut field = AmbientField::new(10, 10, 4.0, 1).or_die();
        field.set_origin([-20.0, -20.0]);
        let centre = field.centre_of(CellId::new(2, 3));
        assert_eq!(field.cell_of(centre), Some(CellId::new(2, 3)));
        assert_eq!(field.cell_of([-100.0, 0.0]), None, "outside the grid");
    }
}
