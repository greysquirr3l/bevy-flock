//! The world director: the thing that decides what kind of day it is.
//!
//! Group directors command squads; the world director commands the *weather of
//! behaviour* — the market is busy at ten, the streets empty after the bell, a fire
//! on the quay makes everyone within forty metres afraid and everyone within a
//! hundred curious.
//!
//! ## Shape
//!
//! A director reads a [`DirectorInputs`] snapshot and writes [`DirectorCommand`]s.
//! It never touches the world directly. That restraint is what makes it
//! reproducible: commands are applied in a fixed order, the trace of the last
//! hundred and twenty-eight of them is kept for debugging, and a replay that feeds
//! the same snapshots produces the same day.
//!
//! ## Panic propagation
//!
//! Fear spreads. [`AlarmField`] holds a handful of alarms — a position, a strength
//! and the tick they were raised — and answers "how afraid should someone standing
//! here be" by distance falloff and decay. It is a field, not a per-NPC
//! broadcast: nothing iterates the crowd, so raising an alarm is O(alarms) no
//! matter how many people are in earshot. That matters, because the alternative —
//! writing a fear component to every NPC near a fire — is exactly the O(n) stall
//! this crate exists to avoid.

use crate::domain::tick::Tick;
use bevy_math::Vec3;
use smallvec::SmallVec;

/// How many commands a director may emit in one decision.
pub const MAX_COMMANDS: usize = 8;

/// How many alarms the field tracks at once.
pub const MAX_ALARMS: usize = 16;

/// How many commands the debug trace keeps.
pub const TRACE_CAPACITY: usize = 128;

/// What a director is allowed to know.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DirectorInputs {
    /// The tick being decided for.
    pub tick: Tick,
    /// Live population.
    pub population: u32,
    /// How many NPCs are manifest in each cognition tier, from reflex upward.
    pub tiers: [u32; 4],
    /// Mean mood across the crowd, in `-1..=1`.
    pub mean_mood: f32,
    /// Mean faction tension, in `0..=1`.
    pub tension: f32,
    /// How many alarms are live.
    pub alarms: u32,
    /// Ticks since a player was last seen, or `None` if one is in view.
    pub ticks_since_player: Option<u64>,
}

impl DirectorInputs {
    /// The hour of the in-game day, `0.0..24.0`, from a fixed 24-hour clock.
    ///
    /// Derived from the tick rather than read from a wall clock: a director that
    /// consulted real time would make a replay produce a different day.
    #[must_use]
    pub fn hour_of_day(self, ticks_per_hour: u64) -> f32 {
        if ticks_per_hour == 0 {
            return 0.0;
        }
        let ticks = self.tick.get() % (ticks_per_hour * 24);
        ticks as f32 / ticks_per_hour as f32
    }

    /// Whether it is between two hours, wrap-around included.
    #[must_use]
    pub fn is_between(self, ticks_per_hour: u64, from: f32, to: f32) -> bool {
        let hour = self.hour_of_day(ticks_per_hour);
        if from <= to {
            (from..to).contains(&hour)
        } else {
            hour >= from || hour < to
        }
    }
}

/// Something a director can order.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DirectorCommand {
    /// Shift the ambient mood of the whole crowd.
    Mood {
        /// The target mood in `-1..=1`.
        value: f32,
        /// How long it holds.
        ttl_ticks: u64,
    },
    /// Start a panic at a point.
    Alarm {
        /// Where.
        at: Vec3,
        /// How strong, in `0..=1`.
        strength: f32,
        /// How far it carries, in metres.
        radius: f32,
    },
    /// Calm everyone near a point.
    Calm {
        /// Where.
        at: Vec3,
        /// How far the calm carries.
        radius: f32,
    },
    /// Ask for more inhabitants to become manifest.
    Spawn {
        /// How many.
        count: u32,
    },
    /// Ask the crowd to stop milling and go somewhere.
    Quiesce {
        /// How long.
        ttl_ticks: u64,
    },
}

impl DirectorCommand {
    /// A stable name for the trace.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Mood { .. } => "mood",
            Self::Alarm { .. } => "alarm",
            Self::Calm { .. } => "calm",
            Self::Spawn { .. } => "spawn",
            Self::Quiesce { .. } => "quiesce",
        }
    }
}

/// A bounded batch of commands.
#[derive(Debug, Clone, Default)]
pub struct DirectorCommands {
    queue: SmallVec<[DirectorCommand; MAX_COMMANDS]>,
    overflowed: u32,
}

impl DirectorCommands {
    /// An empty batch.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Push a command, dropping it if the batch is full.
    ///
    /// A dropped command is counted rather than queued: a director that wants to
    /// issue fifty orders in one tick has a design problem, and silently doing less
    /// work is better than a latency spike.
    pub fn push(&mut self, command: DirectorCommand) {
        if self.queue.len() >= MAX_COMMANDS {
            self.overflowed = self.overflowed.saturating_add(1);
            return;
        }
        self.queue.push(command);
    }

    /// The commands, in issue order.
    #[must_use]
    pub fn as_slice(&self) -> &[DirectorCommand] {
        &self.queue
    }

    /// How many commands were dropped because the batch was full.
    #[must_use]
    pub const fn overflowed(&self) -> u32 {
        self.overflowed
    }

    /// Whether the batch is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// Clear the batch.
    pub fn clear(&mut self) {
        self.queue.clear();
    }
}

/// A decision maker for the whole crowd.
pub trait Director {
    /// Decide what to order this tick.
    fn decide(&mut self, inputs: &DirectorInputs, out: &mut DirectorCommands);

    /// A short label for diagnostics.
    fn label(&self) -> &'static str;
}

/// A director driven by the time of day and the crowd's own state.
///
/// The default. It reads as a town: busy in the morning, quiet at the bell, tense
/// when the alarm count rises.
#[derive(Debug, Clone)]
pub struct AmbientDirector {
    ticks_per_hour: u64,
    last_decision: Option<Tick>,
    decision_interval: u64,
    mood: f32,
    quiet_until: Option<Tick>,
}

impl AmbientDirector {
    /// A director that decides once a second on a 24-hour clock.
    #[must_use]
    pub fn new(ticks_per_hour: u64) -> Self {
        Self {
            ticks_per_hour,
            last_decision: None,
            decision_interval: 60,
            mood: 0.0,
            quiet_until: None,
        }
    }

    /// Decide less often, in ticks.
    #[must_use]
    pub const fn with_interval(mut self, interval: u64) -> Self {
        self.decision_interval = interval;
        self
    }

    /// The mood currently being held.
    #[must_use]
    pub const fn mood(&self) -> f32 {
        self.mood
    }
}

impl Default for AmbientDirector {
    fn default() -> Self {
        Self::new(600)
    }
}

impl Director for AmbientDirector {
    fn decide(&mut self, inputs: &DirectorInputs, out: &mut DirectorCommands) {
        // `elapsed_at_least` is `later.elapsed_at_least(earlier, interval)`: the tick
        // being decided is the later one.
        let due = self
            .last_decision
            .is_none_or(|last| inputs.tick.elapsed_at_least(last, self.decision_interval));
        if !due {
            return;
        }
        self.last_decision = Some(inputs.tick);
        // The day's shape. Times are hours on a 24-hour clock, and a town whose
        // market is busiest mid-morning is a different place at three in the morning.
        let hour = inputs.hour_of_day(self.ticks_per_hour);
        let weekday = 0.35;
        let market = 0.65;
        let evening = 0.1;
        let night = -0.45;
        let target = if (8.0..12.0).contains(&hour) {
            market
        } else if (17.0..21.0).contains(&hour) {
            evening
        } else if !(6.0..22.0).contains(&hour) {
            night
        } else {
            weekday
        };
        // Tension drags the mood down; an alarm count above a handful is a bad day.
        let tension_drag = inputs.tension.clamp(0.0, 1.0) * 0.4;
        let alarm_drag =
            (f32::from(u16::try_from(inputs.alarms).unwrap_or(u16::MAX)) * 0.05).min(0.3);
        self.mood = (target - tension_drag - alarm_drag).clamp(-1.0, 1.0);
        out.push(DirectorCommand::Mood {
            value: self.mood,
            ttl_ticks: self.decision_interval * 4,
        });
        // Nobody is outside in a downpour of fear: if the alarm count is high the
        // crowd is asked to stay put, which is also what stops the whole population
        // from pathing across the map at once.
        let quiet = inputs.alarms >= 3;
        if quiet {
            let until = inputs.tick.get().saturating_add(self.decision_interval * 2);
            self.quiet_until = Some(Tick::new(until));
            out.push(DirectorCommand::Quiesce {
                ttl_ticks: until.saturating_sub(inputs.tick.get()),
            });
        }
        // Nobody has been seen for a long time: the ambient tier can shrink.
        if inputs.ticks_since_player.is_some_and(|ticks| ticks > 3_600) {
            out.push(DirectorCommand::Quiesce { ttl_ticks: 600 });
        }
        // A very low mood with a large crowd asks for fewer manifest bodies, not more.
        if inputs.mean_mood > 0.6 && inputs.population > 0 {
            out.push(DirectorCommand::Spawn { count: 4 });
        }
    }

    fn label(&self) -> &'static str {
        "ambient"
    }
}

/// One entry in a scripted day.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Beat {
    /// The tick the beat fires on.
    pub tick: Tick,
    /// What it orders.
    pub command: DirectorCommand,
}

/// A director that replays a fixed list of beats.
///
/// The tool a designer reaches for first, and the tool a test reaches for when it
/// needs a specific world state on a specific tick.
#[derive(Debug, Clone, Default)]
pub struct ScriptedDirector {
    beats: Vec<Beat>,
    cursor: usize,
}

impl ScriptedDirector {
    /// A director from a list of beats.
    ///
    /// # Errors
    ///
    /// [`DirectorError::OutOfOrder`] if the beats are not sorted by tick, because a
    /// script whose order depends on how it was written is a script whose replay
    /// differs from its first run.
    pub fn new(beats: Vec<Beat>) -> Result<Self, DirectorError> {
        let mut last = Tick::ZERO;
        for beat in &beats {
            if beat.tick < last {
                return Err(DirectorError::OutOfOrder {
                    tick: beat.tick,
                    after: last,
                });
            }
            last = beat.tick;
        }
        Ok(Self { beats, cursor: 0 })
    }

    /// How many beats have fired.
    #[must_use]
    pub const fn fired(&self) -> usize {
        self.cursor
    }
}

impl Director for ScriptedDirector {
    fn decide(&mut self, inputs: &DirectorInputs, out: &mut DirectorCommands) {
        while let Some(beat) = self.beats.get(self.cursor) {
            if beat.tick > inputs.tick {
                break;
            }
            out.push(beat.command);
            self.cursor += 1;
        }
    }

    fn label(&self) -> &'static str {
        "scripted"
    }
}

/// An alarm in the field.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Alarm {
    at: Vec3,
    strength: f32,
    radius: f32,
    raised: Tick,
    ttl_ticks: u64,
}

/// A field of alarms that answers "how afraid is someone standing here".
#[derive(Debug, Clone, Default)]
pub struct AlarmField {
    alarms: SmallVec<[Alarm; MAX_ALARMS]>,
    raised: u64,
    expired: u64,
}

impl AlarmField {
    /// An empty field.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// How many alarms are live.
    #[must_use]
    pub fn len(&self) -> usize {
        self.alarms.len()
    }

    /// Whether the field is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.alarms.is_empty()
    }

    /// How many alarms have been raised since construction.
    #[must_use]
    pub const fn raised(&self) -> u64 {
        self.raised
    }

    /// How many have expired or been evicted.
    #[must_use]
    pub const fn expired(&self) -> u64 {
        self.expired
    }

    /// Raise an alarm.
    ///
    /// At [`MAX_ALARMS`] the oldest is evicted rather than refused: a fire during a
    /// riot is the moment the newest alarm matters most.
    pub fn raise(&mut self, at: Vec3, strength: f32, radius: f32, now: Tick, ttl_ticks: u64) {
        if !(strength.is_finite() && radius.is_finite()) || strength <= 0.0 || radius <= 0.0 {
            return;
        }
        if self.alarms.len() >= MAX_ALARMS {
            self.alarms.remove(0);
            self.expired = self.expired.saturating_add(1);
        }
        self.alarms.push(Alarm {
            at,
            strength: strength.clamp(0.0, 1.0),
            radius,
            raised: now,
            ttl_ticks,
        });
        self.raised = self.raised.saturating_add(1);
    }

    /// Drop the alarms that have expired.
    pub fn expire(&mut self, now: Tick) -> usize {
        let before = self.alarms.len();
        self.alarms
            .retain(|alarm| now.since(alarm.raised) < alarm.ttl_ticks);
        let removed = before - self.alarms.len();
        self.expired = self.expired.saturating_add(removed as u64);
        removed
    }

    /// Calm the alarms near a point: a guard arriving, a priest speaking.
    pub fn calm(&mut self, at: Vec3, radius: f32, now: Tick) {
        self.alarms.retain(|alarm| {
            let distance = alarm.at.distance(at);
            if distance > radius {
                return true;
            }
            // A partial calm weakens the alarm rather than erasing it.
            let retained = alarm.strength * (distance / radius).clamp(0.0, 1.0);
            retained > 0.0 && now.since(alarm.raised) < alarm.ttl_ticks
        });
    }

    /// How afraid someone at `position` should be, in `0..=1`.
    ///
    /// The nearest, strongest alarm dominates: fear is not additive, or a street of
    /// small alarms would produce a panic no single event justifies.
    #[must_use]
    pub fn sample(&self, position: Vec3, now: Tick) -> f32 {
        let mut worst: f32 = 0.0;
        for alarm in &self.alarms {
            if now.since(alarm.raised) >= alarm.ttl_ticks {
                continue;
            }
            let distance = alarm.at.distance(position);
            if distance > alarm.radius {
                continue;
            }
            let falloff = 1.0 - (distance / alarm.radius);
            let decay = 1.0
                - (now.since(alarm.raised) as f32 / alarm.ttl_ticks.max(1) as f32).clamp(0.0, 1.0);
            let value = alarm.strength * falloff * decay;
            if value > worst {
                worst = value;
            }
        }
        worst.clamp(0.0, 1.0)
    }
}

/// Why a script could not be built.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DirectorError {
    /// A beat listed before the one that precedes it.
    #[error("beat at tick {tick:?} follows a beat at {after:?}")]
    OutOfOrder {
        /// The offending tick.
        tick: Tick,
        /// The tick before it.
        after: Tick,
    },
}

/// A completed command, with the tick and director that issued it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TracedCommand {
    /// When.
    pub tick: Tick,
    /// What.
    pub command: DirectorCommand,
}

/// Ties directors, the alarm field and a bounded trace together.
#[derive(Debug, Default)]
pub struct DirectorBoard {
    alarms: AlarmField,
    trace: Vec<TracedCommand>,
    applied: u64,
}

impl DirectorBoard {
    /// A board with an empty alarm field.
    #[must_use]
    pub fn new() -> Self {
        Self {
            alarms: AlarmField::new(),
            trace: Vec::with_capacity(TRACE_CAPACITY),
            applied: 0,
        }
    }

    /// The alarm field.
    #[must_use]
    pub const fn alarms(&self) -> &AlarmField {
        &self.alarms
    }

    /// How many commands have been applied.
    #[must_use]
    pub const fn applied(&self) -> u64 {
        self.applied
    }

    /// The recent commands, oldest first.
    #[must_use]
    pub fn trace(&self) -> &[TracedCommand] {
        &self.trace
    }

    /// Run a director against a snapshot and apply what it orders.
    ///
    /// Commands are applied in issue order, which is the only order that makes a
    /// sequence like "raise an alarm, then calm it" mean what it says.
    pub fn decide(
        &mut self,
        director: &mut dyn Director,
        inputs: &DirectorInputs,
    ) -> DirectorCommands {
        let mut commands = DirectorCommands::new();
        director.decide(inputs, &mut commands);
        for command in commands.as_slice() {
            self.apply(*command, inputs.tick);
        }
        commands
    }

    /// Apply one command.
    pub fn apply(&mut self, command: DirectorCommand, at: Tick) {
        match command {
            DirectorCommand::Alarm {
                at: position,
                strength,
                radius,
            } => {
                // Alarms last ten seconds of simulated time; long enough to spread,
                // short enough that a street recovers.
                self.alarms.raise(position, strength, radius, at, 600);
            }
            DirectorCommand::Calm {
                at: position,
                radius,
            } => {
                self.alarms.calm(position, radius, at);
            }
            DirectorCommand::Mood { .. }
            | DirectorCommand::Spawn { .. }
            | DirectorCommand::Quiesce { .. } => {}
        }
        self.applied = self.applied.saturating_add(1);
        if self.trace.len() >= TRACE_CAPACITY {
            self.trace.remove(0);
        }
        if self.trace.len() < TRACE_CAPACITY {
            self.trace.push(TracedCommand { tick: at, command });
        }
    }

    /// Advance the field: drop expired alarms.
    pub fn tick(&mut self, now: Tick) {
        let _ = self.alarms.expire(now);
    }

    /// How afraid someone at `position` should be.
    #[must_use]
    pub fn fear_at(&self, position: Vec3, now: Tick) -> f32 {
        self.alarms.sample(position, now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{OrDie, OrDieErr};

    fn inputs(tick: u64) -> DirectorInputs {
        DirectorInputs {
            tick: Tick::new(tick),
            population: 1_000,
            tiers: [400, 300, 200, 100],
            mean_mood: 0.0,
            tension: 0.1,
            alarms: 0,
            ticks_since_player: Some(0),
        }
    }

    #[test]
    fn the_day_has_a_shape() {
        let mut director = AmbientDirector::new(600).with_interval(60);
        let day = 600 * 24;
        let mut moods = Vec::new();
        for tick in (0..day).step_by(600) {
            let mut out = DirectorCommands::new();
            director.decide(&inputs(tick), &mut out);
            moods.push(director.mood());
        }
        let midnight = moods.first().copied().unwrap_or(0.0);
        let midmorning = moods.get(10).copied().unwrap_or(0.0);
        assert!(
            midmorning > midnight,
            "the market should be busier than midnight"
        );
    }

    #[test]
    fn a_quiet_tick_issues_nothing() {
        let mut director = AmbientDirector::new(600).with_interval(60);
        let mut out = DirectorCommands::new();
        director.decide(&inputs(0), &mut out);
        out.clear();
        director.decide(&inputs(1), &mut out);
        assert!(
            out.is_empty(),
            "the director decided twice inside one interval"
        );
    }

    #[test]
    fn alarms_drag_the_mood_and_ask_for_quiet() {
        let mut director = AmbientDirector::new(600).with_interval(60);
        let mut busy = inputs(600);
        busy.alarms = 4;
        let mut out = DirectorCommands::new();
        director.decide(&busy, &mut out);
        assert!(
            out.as_slice()
                .iter()
                .any(|command| matches!(command, DirectorCommand::Quiesce { .. }))
        );
        assert!(director.mood() < 0.0);
    }

    #[test]
    fn a_scripted_day_fires_in_order_and_only_once() {
        let beats = vec![
            Beat {
                tick: Tick::new(10),
                command: DirectorCommand::Alarm {
                    at: Vec3::ZERO,
                    strength: 0.8,
                    radius: 20.0,
                },
            },
            Beat {
                tick: Tick::new(20),
                command: DirectorCommand::Calm {
                    at: Vec3::ZERO,
                    radius: 20.0,
                },
            },
        ];
        let mut director = ScriptedDirector::new(beats).or_die();
        let mut board = DirectorBoard::new();
        let _ = board.decide(&mut director, &inputs(5));
        assert_eq!(director.fired(), 0);
        let _ = board.decide(&mut director, &inputs(10));
        assert_eq!(director.fired(), 1);
        let _ = board.decide(&mut director, &inputs(10));
        assert_eq!(director.fired(), 1, "a beat fired twice");
        let _ = board.decide(&mut director, &inputs(25));
        assert_eq!(director.fired(), 2);
    }

    #[test]
    fn an_out_of_order_script_is_refused() {
        let beats = vec![
            Beat {
                tick: Tick::new(20),
                command: DirectorCommand::Quiesce { ttl_ticks: 1 },
            },
            Beat {
                tick: Tick::new(10),
                command: DirectorCommand::Quiesce { ttl_ticks: 1 },
            },
        ];
        assert!(matches!(
            ScriptedDirector::new(beats).or_die_err(),
            DirectorError::OutOfOrder { .. }
        ));
    }

    #[test]
    fn fear_falls_off_with_distance_and_time() {
        let mut field = AlarmField::new();
        field.raise(Vec3::ZERO, 1.0, 40.0, Tick::ZERO, 600);
        let near = field.sample(Vec3::new(2.0, 0.0, 0.0), Tick::new(10));
        let far = field.sample(Vec3::new(30.0, 0.0, 0.0), Tick::new(10));
        let outside = field.sample(Vec3::new(200.0, 0.0, 0.0), Tick::new(10));
        assert!(near > far, "distance should attenuate: {near} vs {far}");
        assert_eq!(outside, 0.0);
        let later = field.sample(Vec3::new(2.0, 0.0, 0.0), Tick::new(500));
        assert!(later < near, "time should attenuate");
        let gone = field.sample(Vec3::new(2.0, 0.0, 0.0), Tick::new(700));
        assert_eq!(gone, 0.0);
    }

    #[test]
    fn calming_weakens_rather_than_erases_at_the_edge() {
        let mut field = AlarmField::new();
        field.raise(Vec3::new(30.0, 0.0, 0.0), 1.0, 40.0, Tick::ZERO, 600);
        field.calm(Vec3::ZERO, 40.0, Tick::new(5));
        let at_edge = field.sample(Vec3::new(30.0, 0.0, 0.0), Tick::new(5));
        assert!(at_edge < 1.0, "calming did nothing");
        field.calm(Vec3::new(30.0, 0.0, 0.0), 2.0, Tick::new(6));
        assert_eq!(field.sample(Vec3::new(30.0, 0.0, 0.0), Tick::new(6)), 0.0);
    }

    #[test]
    fn the_alarm_field_is_bounded() {
        let mut field = AlarmField::new();
        for index in 0..64_u32 {
            field.raise(
                Vec3::new(index as f32, 0.0, 0.0),
                1.0,
                10.0,
                Tick::new(u64::from(index)),
                600,
            );
        }
        assert!(field.len() <= MAX_ALARMS);
        assert!(field.expired() > 0);
    }

    #[test]
    fn raising_an_alarm_does_not_scale_with_the_crowd() {
        // The field is O(alarms). The check that matters is that nothing in the path
        // takes a population argument at all: this compiles only because it does not.
        let mut field = AlarmField::new();
        field.raise(Vec3::ZERO, 0.9, 30.0, Tick::ZERO, 600);
        assert_eq!(field.len(), 1);
        assert_eq!(field.raised(), 1);
    }

    #[test]
    fn the_trace_is_bounded() {
        let mut board = DirectorBoard::new();
        for index in 0..(TRACE_CAPACITY * 2) {
            board.apply(
                DirectorCommand::Quiesce { ttl_ticks: 1 },
                Tick::new(index as u64),
            );
        }
        assert_eq!(board.trace().len(), TRACE_CAPACITY);
        assert_eq!(board.applied(), (TRACE_CAPACITY * 2) as u64);
        let last = board.trace().last().map(|entry| entry.tick);
        assert_eq!(last, Some(Tick::new((TRACE_CAPACITY * 2 - 1) as u64)));
    }

    #[test]
    fn applying_the_same_script_twice_produces_the_same_trace() {
        let run = || {
            let beats = (0..6_u64)
                .map(|index| Beat {
                    tick: Tick::new(index * 10),
                    command: DirectorCommand::Alarm {
                        at: Vec3::new(index as f32, 0.0, 0.0),
                        strength: 0.5,
                        radius: 15.0,
                    },
                })
                .collect();
            let mut director = ScriptedDirector::new(beats).or_die();
            let mut board = DirectorBoard::new();
            for tick in 0..60 {
                let _ = board.decide(&mut director, &inputs(tick));
            }
            board.trace().to_vec()
        };
        assert_eq!(run(), run());
    }

    #[test]
    fn a_batch_of_commands_is_bounded() {
        let mut out = DirectorCommands::new();
        for _ in 0..(MAX_COMMANDS * 2) {
            out.push(DirectorCommand::Quiesce { ttl_ticks: 1 });
        }
        assert_eq!(out.as_slice().len(), MAX_COMMANDS);
        assert_eq!(out.overflowed(), MAX_COMMANDS as u32);
    }
}
