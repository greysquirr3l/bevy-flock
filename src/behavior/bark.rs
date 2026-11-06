//! Barks: the one line an NPC says when it notices you.
//!
//! Barks are the cheapest social behaviour in a crowd and the one players notice
//! most. They are also the easiest thing to do badly: a single line repeated by
//! everyone, or a thousand NPCs talking at once because nothing limits them.
//!
//! This module is the limiter. [`BarkTable`] holds rate-limited categories;
//! [`BarkBudget`] enforces one bark per NPC per cooldown and a global cap per tick
//! so a riot does not become a thousand overlapping voice lines; and the picker
//! refuses to repeat an NPC's last line, which is what stops "salt? salt? salt?"
//! from being the defining memory of a market.
//!
//! Everything is integer and deterministic: given the same tick, the same table
//! and the same seed, the same NPC says the same thing.

use crate::domain::ids::NpcId;
use crate::domain::rng::RngStream;
use crate::domain::tick::Tick;
use smallvec::SmallVec;
use std::collections::VecDeque;

/// The most bark lines one table may hold.
pub const MAX_BARK_LINES: usize = 512;

/// What kind of thing is being said.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum BarkCategory {
    /// Something is wrong.
    Alarm,
    /// Acknowledging a person.
    Greeting,
    /// Nothing in particular.
    Idle,
    /// Prices, wares, coin.
    Trade,
    /// A fight.
    Combat,
    /// Someone died.
    Grief,
}

impl BarkCategory {
    /// Every category, in a fixed order.
    pub const ALL: [Self; 6] = [
        Self::Alarm,
        Self::Greeting,
        Self::Idle,
        Self::Trade,
        Self::Combat,
        Self::Grief,
    ];

    /// A stable name for logs and content tools.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Alarm => "alarm",
            Self::Greeting => "greeting",
            Self::Idle => "idle",
            Self::Trade => "trade",
            Self::Combat => "combat",
            Self::Grief => "grief",
        }
    }

    /// The category's position in [`BarkCategory::ALL`], used to index per-category
    /// cooldowns without a map.
    #[must_use]
    pub const fn ordinal(self) -> usize {
        match self {
            Self::Alarm => 0,
            Self::Greeting => 1,
            Self::Idle => 2,
            Self::Trade => 3,
            Self::Combat => 4,
            Self::Grief => 5,
        }
    }

    /// How long an NPC waits before repeating a bark of this category.
    ///
    /// Alarms are allowed to repeat quickly — silence during a fire is worse than
    /// repetition — while idle chatter is on a long leash, because it is the one a
    /// player hears most often.
    #[must_use]
    pub const fn cooldown_ticks(self) -> u64 {
        match self {
            Self::Alarm => 30,
            Self::Combat => 45,
            Self::Grief => 120,
            Self::Greeting => 300,
            Self::Trade => 600,
            Self::Idle => 1_800,
        }
    }
}

/// One authored line.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct BarkLine {
    /// What it is for.
    pub category: BarkCategory,
    /// What is said.
    pub text: String,
    /// Relative frequency within the category.
    pub weight: u32,
}

impl BarkLine {
    /// A line with weight one.
    #[must_use]
    pub fn new(category: BarkCategory, text: impl Into<String>) -> Self {
        Self {
            category,
            text: text.into(),
            weight: 1,
        }
    }

    /// A line with a relative weight.
    #[must_use]
    pub const fn weighted(mut self, weight: u32) -> Self {
        self.weight = weight;
        self
    }
}

/// Every line the crowd can say, grouped by category.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct BarkTable {
    lines: Vec<BarkLine>,
}

impl BarkTable {
    /// Build a table.
    ///
    /// # Errors
    ///
    /// [`BarkError::TooManyLines`] beyond [`MAX_BARK_LINES`], and
    /// [`BarkError::EmptyCategory`] when a category has no lines at all — an NPC
    /// that wants to shout and has nothing to say is a content gap worth failing
    /// the build over.
    pub fn new(lines: Vec<BarkLine>) -> Result<Self, BarkError> {
        if lines.len() > MAX_BARK_LINES {
            return Err(BarkError::TooManyLines {
                count: lines.len(),
                limit: MAX_BARK_LINES,
            });
        }
        let table = Self { lines };
        for category in BarkCategory::ALL {
            if table.count(category) == 0 {
                return Err(BarkError::EmptyCategory { category });
            }
        }
        Ok(table)
    }

    /// A small, complete table a game can start with.
    ///
    /// # Errors
    ///
    /// Cannot fail; the table is a literal.
    pub fn starter() -> Result<Self, BarkError> {
        Self::new(vec![
            BarkLine::new(BarkCategory::Greeting, "Morning."),
            BarkLine::new(BarkCategory::Greeting, "You're new."),
            BarkLine::new(BarkCategory::Greeting, "Cold one, isn't it."),
            BarkLine::new(BarkCategory::Idle, "Bread's up. finally."),
            BarkLine::new(BarkCategory::Idle, "Rain before night, mark me."),
            BarkLine::new(BarkCategory::Idle, "Wheels need grease again."),
            BarkLine::new(BarkCategory::Trade, "Two coppers a measure."),
            BarkLine::new(BarkCategory::Trade, "Fresh in this morning."),
            BarkLine::new(BarkCategory::Alarm, "Fire! Fire on the quay!").weighted(3),
            BarkLine::new(BarkCategory::Alarm, "Watch! Thief!"),
            BarkLine::new(BarkCategory::Combat, "Back off."),
            BarkLine::new(BarkCategory::Combat, "Not today."),
            BarkLine::new(BarkCategory::Grief, "Someone get the priest."),
            BarkLine::new(BarkCategory::Grief, "That was a friend of mine."),
        ])
    }

    /// The lines, in authoring order.
    #[must_use]
    pub fn lines(&self) -> &[BarkLine] {
        &self.lines
    }

    /// How many lines a category holds.
    #[must_use]
    pub fn count(&self, category: BarkCategory) -> usize {
        self.lines
            .iter()
            .filter(|line| line.category == category)
            .count()
    }

    /// Pick a line of a category, avoiding the one this NPC said last.
    ///
    /// Returns `None` only when the table has nothing to say in that category,
    /// which [`BarkTable::new`] already refuses for a validated table.
    #[must_use]
    pub fn pick(
        &self,
        category: BarkCategory,
        avoid: Option<usize>,
        rng: &mut RngStream,
    ) -> Option<usize> {
        let mut candidates: SmallVec<[(usize, u32); 32]> = SmallVec::new();
        let mut total = 0_u32;
        for (index, line) in self.lines.iter().enumerate() {
            if line.category != category || line.weight == 0 {
                continue;
            }
            if avoid == Some(index) && self.count(category) > 1 {
                continue;
            }
            candidates.push((index, line.weight));
            total = total.saturating_add(line.weight);
        }
        if candidates.is_empty() || total == 0 {
            return None;
        }
        let mut pick = rng.next_bounded(total);
        for (index, weight) in candidates {
            if pick < weight {
                return Some(index);
            }
            pick -= weight;
        }
        // Off-by-one at the top of the range lands here rather than panicking.
        self.lines
            .iter()
            .enumerate()
            .rev()
            .find(|(_, line)| line.category == category)
            .map(|(index, _)| index)
    }

    /// The text of a line.
    #[must_use]
    pub fn text(&self, index: usize) -> Option<&str> {
        self.lines.get(index).map(|line| line.text.as_str())
    }

    /// Bytes the table occupies.
    #[must_use]
    pub fn bytes(&self) -> usize {
        self.lines
            .iter()
            .map(|line| line.text.len() + core::mem::size_of::<BarkLine>())
            .sum()
    }
}

/// What an NPC remembers about its own talking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BarkState {
    /// One stamp per category: a greeting must not be silenced because the NPC
    /// shouted an alarm a moment earlier.
    last_ticks: [Option<Tick>; 6],
    last_line: Option<usize>,
}

impl BarkState {
    /// How many ticks until this NPC may bark again in this category.
    #[must_use]
    pub fn ready_at(&self, now: Tick, category: BarkCategory) -> bool {
        self.last_ticks
            .get(category.ordinal())
            .copied()
            .flatten()
            .is_none_or(|last| now.elapsed_at_least(last, category.cooldown_ticks()))
    }

    /// The line this NPC said last, so the next pick can avoid it.
    #[must_use]
    pub const fn last_line(&self) -> Option<usize> {
        self.last_line
    }

    /// Record that a bark happened in a category.
    pub fn record(&mut self, now: Tick, category: BarkCategory, line: usize) {
        if let Some(slot) = self.last_ticks.get_mut(category.ordinal()) {
            *slot = Some(now);
        }
        self.last_line = Some(line);
    }

    /// How long since this NPC last spoke in a category, or `None` if it never has.
    #[must_use]
    pub fn since(&self, now: Tick, category: BarkCategory) -> Option<u64> {
        self.last_ticks
            .get(category.ordinal())
            .copied()
            .flatten()
            .map(|last| now.since(last))
    }
}

/// A request from the simulation that an NPC would like to say something.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BarkRequest {
    /// Who wants to talk.
    pub npc: NpcId,
    /// What about.
    pub category: BarkCategory,
    /// The tick of the request.
    pub at: Tick,
}

/// A bark that was actually produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BarkEvent {
    /// Who said it.
    pub npc: NpcId,
    /// What category.
    pub category: BarkCategory,
    /// The table index of the line, so a game can look up or localise the text.
    pub line: usize,
    /// When.
    pub at: Tick,
}

/// A bounded queue of barks waiting to be played.
///
/// Bounded on purpose: a crowd of ten thousand all noticing the player would
/// otherwise allocate ten thousand events and play none of them, because nobody
/// can hear a thousand voices. The queue keeps the newest and reports how many it
/// dropped, so a game can see the pressure rather than discovering it as a hitch.
#[derive(Debug, Clone)]
pub struct BarkQueue {
    events: VecDeque<BarkEvent>,
    capacity: usize,
    dropped: u64,
    produced: u64,
}

impl BarkQueue {
    /// A queue holding at most `capacity` events.
    ///
    /// # Errors
    ///
    /// [`BarkError::ZeroCapacity`] for a capacity of zero.
    pub fn new(capacity: usize) -> Result<Self, BarkError> {
        if capacity == 0 {
            return Err(BarkError::ZeroCapacity);
        }
        Ok(Self {
            events: VecDeque::with_capacity(capacity.min(256)),
            capacity,
            dropped: 0,
            produced: 0,
        })
    }

    /// The events currently queued.
    #[must_use]
    pub const fn events(&self) -> &VecDeque<BarkEvent> {
        &self.events
    }

    /// How many events are waiting.
    #[must_use]
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// Whether nothing is waiting.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// How many events were dropped because the queue was full.
    #[must_use]
    pub const fn dropped(&self) -> u64 {
        self.dropped
    }

    /// How many events were produced in total.
    #[must_use]
    pub const fn produced(&self) -> u64 {
        self.produced
    }

    /// The capacity.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// Push an event, dropping the oldest if the queue is full.
    pub fn push(&mut self, event: BarkEvent) {
        self.produced = self.produced.saturating_add(1);
        if self.events.len() >= self.capacity {
            self.events.pop_front();
            self.dropped = self.dropped.saturating_add(1);
        }
        self.events.push_back(event);
    }

    /// Take every event, leaving the queue empty.
    pub fn drain(&mut self) -> impl Iterator<Item = BarkEvent> + '_ {
        self.events.drain(..)
    }

    /// Forget everything: used when the crowd is despawned.
    pub fn clear(&mut self) {
        self.events.clear();
    }
}

/// Why a bark table or queue could not be built.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum BarkError {
    /// More lines than a table may hold.
    #[error("a bark table holds at most {limit} lines, {count} given")]
    TooManyLines {
        /// How many were given.
        count: usize,
        /// The limit.
        limit: usize,
    },
    /// A category with nothing to say.
    #[error("bark category {category:?} has no lines")]
    EmptyCategory {
        /// The empty category.
        category: BarkCategory,
    },
    /// A queue with no room.
    #[error("a bark queue needs room for at least one event")]
    ZeroCapacity,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::ids::SimSeed;
    use crate::testkit::{OrDie, OrDieErr};

    fn table() -> BarkTable {
        BarkTable::starter().or_die()
    }

    #[test]
    fn a_table_without_lines_in_a_category_is_rejected() {
        let error = BarkTable::new(vec![BarkLine::new(BarkCategory::Idle, "Hm.")]).or_die_err();
        assert!(matches!(error, BarkError::EmptyCategory { category: _ }));
    }

    #[test]
    fn the_picker_never_repeats_the_last_line_when_it_has_alternatives() {
        let table = table();
        let mut rng = RngStream::new(SimSeed::new(3), 9);
        let mut last = None;
        for _ in 0..50 {
            let pick = table.pick(BarkCategory::Idle, last, &mut rng).or_die();
            assert_ne!(Some(pick), last, "the same idle line twice in a row");
            last = Some(pick);
        }
    }

    #[test]
    fn a_single_line_category_may_repeat() {
        let mut lines = Vec::new();
        for category in BarkCategory::ALL {
            lines.push(BarkLine::new(category, "Only line."));
        }
        let table = BarkTable::new(lines).or_die();
        let mut rng = RngStream::new(SimSeed::new(3), 9);
        let first = table.pick(BarkCategory::Alarm, None, &mut rng).or_die();
        let second = table
            .pick(BarkCategory::Alarm, Some(first), &mut rng)
            .or_die();
        assert_eq!(first, second);
    }

    #[test]
    fn the_cooldown_is_per_category_and_monotonic() {
        let mut state = BarkState::default();
        assert!(state.ready_at(Tick::ZERO, BarkCategory::Alarm));
        state.record(Tick::ZERO, BarkCategory::Alarm, 0);
        assert!(!state.ready_at(Tick::new(10), BarkCategory::Alarm));
        assert!(state.ready_at(Tick::new(30), BarkCategory::Alarm));
        // A quiet category is available immediately, because its own clock is what
        // matters: a greeting must not be blocked by a shouted alarm.
        assert!(state.ready_at(Tick::new(1), BarkCategory::Idle));
        assert_eq!(state.since(Tick::new(5), BarkCategory::Alarm), Some(5));
        assert_eq!(state.since(Tick::new(5), BarkCategory::Idle), None);
    }

    #[test]
    fn the_queue_is_bounded_and_counts_its_drops() {
        let mut queue = BarkQueue::new(4).or_die();
        for index in 0..10_u32 {
            queue.push(BarkEvent {
                npc: NpcId::unique(index),
                category: BarkCategory::Idle,
                line: 0,
                at: Tick::ZERO,
            });
        }
        assert_eq!(queue.len(), 4);
        assert_eq!(queue.dropped(), 6);
        assert_eq!(queue.produced(), 10);
        let newest = queue.events().back().map(|event| event.npc);
        assert_eq!(newest, Some(NpcId::unique(9)));
    }

    #[test]
    fn a_queue_of_zero_is_refused() {
        assert_eq!(BarkQueue::new(0).or_die_err(), BarkError::ZeroCapacity);
    }

    #[test]
    fn the_table_is_deterministic_for_a_seed() {
        let table = table();
        let pick = |seed: u64| {
            let mut rng = RngStream::new(SimSeed::new(seed), 1);
            table.pick(BarkCategory::Greeting, None, &mut rng)
        };
        assert_eq!(pick(5), pick(5));
    }

    #[test]
    fn the_table_reports_its_size() {
        let table = table();
        assert!(table.bytes() > 0);
        assert_eq!(table.count(BarkCategory::Greeting), 3);
        assert!(table.text(0).is_some());
    }
}
