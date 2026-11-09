//! One-shot gestures: a wave, a nod, a bark, a flinch, asked for by a behaviour leaf.
//!
//! A gesture is the cheap half of acting alive, and the easy half to do badly. Every leaf that
//! notices something wants one; a crowd of ten thousand of them noticing at once wants ten thousand
//! animations on one frame. This module is the limiter, and it is modelled on the bark path
//! ([`crate::adapters::primary::dialogue`]) because that path already solved the same three problems:
//!
//! * **A bounded channel.** [`GestureChannel`] holds at most its capacity; a request that arrives
//!   when it is full is *counted* and dropped, never queued. Ten thousand requests in one tick cost
//!   ten thousand increments and no memory.
//! * **A per-NPC, per-category cooldown, in ticks.** A wave is allowed every three seconds and a
//!   flinch every four, per body, compared with [`Tick::elapsed_at_least`]. The stamp survives a
//!   gesture being interrupted, so an interrupted wave still counts as the wave of that window.
//! * **Priority rather than a queue of everything.** A flinch interrupts a wave mid-play; a wave
//!   does not interrupt a flinch. Requests of equal or lower priority that arrive while something is
//!   playing are suppressed and counted rather than stacked up.
//!
//! What is *not* here is the clip. A gesture is played by naming a node in the game's own graph,
//! which the animation crate does; this module only says which kind, from which tick, at what weight.
//! That weight is a pure function of the tick — [`advance_gestures`] writes it once per tick — so
//! a replay gestures identically at any frame rate (ADR 0052).
//!
//! # Asking for a gesture
//!
//! A leaf runs with `&self` and a `&mut Blackboard`; it has no access to resources, which is what
//! keeps leaves pure and their cost measurable. So a leaf asks the way it does everything else, by
//! writing a blackboard slot, with [`request_gesture`]:
//!
//! ```
//! use bevy_flock::behavior::Blackboard;
//! use bevy_flock::prelude::{GestureKind, request_gesture};
//!
//! # let mut board = Blackboard::default();
//! request_gesture(&mut board, GestureKind::Wave);
//! ```
//!
//! [`collect_gesture_requests`] moves those slots into the channel on the same tick the crowd is
//! planned, and [`advance_gestures`] decides which of them actually happen. A game that runs its own
//! logic instead of leaves calls [`GestureChannel::ask`] directly and skips both.

use std::collections::VecDeque;

use bevy_ecs::prelude::{Component, Query, Res, ResMut, Resource};

use crate::adapters::primary::cognition::NpcBlackboard;
use crate::adapters::primary::components::Npc;
use crate::application::SimClock;
use crate::behavior::blackboard::{Blackboard, BlackboardKey, Value};
use crate::domain::ids::NpcId;
use crate::domain::num;
use crate::domain::tick::Tick;

/// The blackboard slot a leaf writes to ask for a gesture.
///
/// Reserved at 50, past the perception slots (20–21), the social ones (30–31), memory (40–42) and
/// interaction (46–47), so a game's own keys and this one cannot collide by accident.
pub const KEY_GESTURE: BlackboardKey = BlackboardKey::new(50);

/// How many requests the channel holds by default: a faction's worth of reactions in one tick.
pub const DEFAULT_CAPACITY: usize = 64;

/// What kind of gesture, declared in ascending priority.
///
/// The declaration order *is* the priority, so `Ord` compares urgency and a greater kind always wins
/// a contest. A flinch outranks a wave because being startled is an interruption and waving is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum GestureKind {
    /// A small nod: agreement, acknowledgement, the cheapest gesture there is.
    Nod,
    /// A wave: greeting somebody at a distance.
    Wave,
    /// A bark: a short, sharp vocal gesture with the body behind it.
    Bark,
    /// A flinch: something happened and the body reacted before the mind did.
    Flinch,
}

impl GestureKind {
    /// Every kind, in ascending priority.
    pub const ALL: [Self; 4] = [Self::Nod, Self::Wave, Self::Bark, Self::Flinch];

    /// A stable name for logs, content tools and suppression reasons.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Nod => "nod",
            Self::Wave => "wave",
            Self::Bark => "bark",
            Self::Flinch => "flinch",
        }
    }

    /// The kind's position in [`GestureKind::ALL`], which is also its priority.
    #[must_use]
    pub const fn ordinal(self) -> usize {
        match self {
            Self::Nod => 0,
            Self::Wave => 1,
            Self::Bark => 2,
            Self::Flinch => 3,
        }
    }

    /// The kind at a position, or `None` for a number no kind answers to.
    #[must_use]
    pub const fn from_ordinal(raw: u16) -> Option<Self> {
        match raw {
            0 => Some(Self::Nod),
            1 => Some(Self::Wave),
            2 => Some(Self::Bark),
            3 => Some(Self::Flinch),
            _ => None,
        }
    }

    /// How long the gesture holds at full weight, in ticks, before it blends out.
    ///
    /// The clip is authored for this; a game whose wave is longer than forty-two ticks at sixty a
    /// second is waving for three quarters of a second, which reads as a summoning.
    #[must_use]
    pub const fn duration_ticks(self) -> u64 {
        match self {
            Self::Nod => 18,
            Self::Wave => 42,
            Self::Bark => 24,
            Self::Flinch => 15,
        }
    }

    /// How long one NPC waits before this kind of gesture is available again, in ticks.
    ///
    /// A flinch has the longest cooldown of any *frequent* gesture and a bark the shortest: a guard
    /// who flinches at every passer-by is a broken guard, while a dog that barks twice is a dog.
    #[must_use]
    pub const fn cooldown_ticks(self) -> u64 {
        match self {
            Self::Nod => 120,
            Self::Wave => 180,
            Self::Bark => 60,
            Self::Flinch => 240,
        }
    }
}

/// A leaf's wish: this NPC would like to do this, from this tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GestureRequest {
    /// Who wants to gesture.
    pub npc: NpcId,
    /// What they want to do.
    pub kind: GestureKind,
    /// The tick the request was made on, so a stale one can be spotted in a log.
    pub at: Tick,
}

/// How a gesture blends in and out, and how many are let through per tick.
///
/// Ticks, not seconds. `blend_in` and `blend_out` are raised to at least one each because a
/// zero-length ramp is a division by zero, and one tick is already the shortest blend that exists.
#[derive(Resource, Debug, Clone, Copy, PartialEq, Eq)]
pub struct GestureConfig {
    blend_in: u32,
    blend_out: u32,
    max_per_tick: u32,
}

impl GestureConfig {
    /// A blend in, a blend out and a per-tick cap, all in ticks.
    #[must_use]
    pub const fn new(blend_in: u32, blend_out: u32, max_per_tick: u32) -> Self {
        Self {
            blend_in: if blend_in == 0 { 1 } else { blend_in },
            blend_out: if blend_out == 0 { 1 } else { blend_out },
            max_per_tick: if max_per_tick == 0 { 1 } else { max_per_tick },
        }
    }

    /// Ticks from starting a gesture to full weight.
    #[must_use]
    pub const fn blend_in(&self) -> u32 {
        self.blend_in
    }

    /// Ticks from the end of the hold to zero weight.
    #[must_use]
    pub const fn blend_out(&self) -> u32 {
        self.blend_out
    }

    /// The most requests adjudicated in one tick.
    ///
    /// A cap rather than a budget: a request that misses this tick's cap stays queued and is
    /// adjudicated next tick, so a large town's gestures are spread over frames instead of dropped.
    #[must_use]
    pub const fn max_per_tick(&self) -> u32 {
        self.max_per_tick
    }
}

impl Default for GestureConfig {
    fn default() -> Self {
        // Six ticks in, eight out: a tenth of a second each way at sixty a second, which is the
        // window in which a blend reads as a body moving rather than as a clip changing.
        Self::new(6, 8, 32)
    }
}

/// The bounded channel a game or a leaf submits gesture requests to.
///
/// The queue is bounded and its refusals are counted by reason, because a crowd under pressure
/// should show up in a diagnostic rather than as a staircase in the frame time.
#[derive(Resource, Debug)]
pub struct GestureChannel {
    queue: VecDeque<GestureRequest>,
    capacity: usize,
    accepted: u64,
    refused_full: u64,
    refused_cooldown: u64,
    refused_busy: u64,
    dropped_absent: u64,
}

impl GestureChannel {
    /// A channel holding at most `capacity` requests, or one if that is zero.
    ///
    /// A capacity of zero is raised rather than refused: a request nobody can make is a
    /// configuration mistake, but it is not a reason to fail a frame.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            // The initial allocation is capped so a game that asks for a million does not pay for a
            // million slots it will never fill; the deque grows only if the requests arrive.
            queue: VecDeque::with_capacity(capacity.min(256)),
            capacity,
            accepted: 0,
            refused_full: 0,
            refused_cooldown: 0,
            refused_busy: 0,
            dropped_absent: 0,
        }
    }

    /// Submit a request. Returns whether it was queued.
    ///
    /// A `false` is not an error: it means the channel is at capacity and the request was counted
    /// in [`GestureChannel::refused_full`]. Dropping the newest is the right choice for gestures,
    /// because the newest request is the one a crowded tick produced by accident.
    pub fn request(&mut self, request: GestureRequest) -> bool {
        if self.queue.len() >= self.capacity {
            self.refused_full = self.refused_full.saturating_add(1);
            return false;
        }
        self.queue.push_back(request);
        true
    }

    /// Ask for a gesture without building a [`GestureRequest`].
    ///
    /// The call a game's own logic makes: `channel.ask(npc, GestureKind::Wave, clock.tick)`.
    pub fn ask(&mut self, npc: NpcId, kind: GestureKind, now: Tick) -> bool {
        self.request(GestureRequest { npc, kind, at: now })
    }

    /// The most requests the channel holds.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// How many requests are waiting to be adjudicated.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.queue.len()
    }

    /// How many gestures were started.
    #[must_use]
    pub const fn accepted(&self) -> u64 {
        self.accepted
    }

    /// How many requests were refused because the channel was full.
    #[must_use]
    pub const fn refused_full(&self) -> u64 {
        self.refused_full
    }

    /// How many requests were refused because that NPC had gestured too recently.
    #[must_use]
    pub const fn refused_cooldown(&self) -> u64 {
        self.refused_cooldown
    }

    /// How many requests were refused because a gesture of equal or higher priority was playing.
    #[must_use]
    pub const fn refused_busy(&self) -> u64 {
        self.refused_busy
    }

    /// How many requests were dropped because their NPC no longer exists, or never had gesture state.
    #[must_use]
    pub const fn dropped_absent(&self) -> u64 {
        self.dropped_absent
    }

    /// Every request that never became a gesture, for whichever reason.
    #[must_use]
    pub const fn refused(&self) -> u64 {
        self.refused_full
            .saturating_add(self.refused_cooldown)
            .saturating_add(self.refused_busy)
            .saturating_add(self.dropped_absent)
    }

    /// Take the oldest queued request.
    fn pop(&mut self) -> Option<GestureRequest> {
        self.queue.pop_front()
    }
}

impl Default for GestureChannel {
    fn default() -> Self {
        Self::new(DEFAULT_CAPACITY)
    }
}

/// What the animation layer is asked to play this tick.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GestureCue {
    kind: GestureKind,
    weight: f32,
    serial: u32,
}

impl GestureCue {
    /// What to play.
    #[must_use]
    pub const fn kind(&self) -> GestureKind {
        self.kind
    }

    /// How much of it to apply, in `0.0..=1.0`.
    #[must_use]
    pub const fn weight(&self) -> f32 {
        self.weight
    }

    /// Which gesture of this body's life this is, counting from one.
    ///
    /// A consumer needs it to act once per request rather than once per tick: the cue is present for
    /// every tick of the gesture, and a counter incremented on every tick it is seen would report one
    /// suppression forty times.
    #[must_use]
    pub const fn serial(&self) -> u32 {
        self.serial
    }
}

/// What one gesture is, once it has started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Running {
    kind: GestureKind,
    started: Tick,
    serial: u32,
}

/// An NPC's gesture history and what it is doing right now.
///
/// One component per body, `Copy`, and small: four cooldown stamps and the running gesture. It is
/// what makes a cooldown survive an interruption — the stamps and the running gesture are separate
/// fields, so a flinch cutting a wave short still leaves the wave on its cooldown.
#[derive(Component, Debug, Clone, Copy, PartialEq, Default)]
pub struct NpcGestures {
    last: [Option<Tick>; 4],
    active: Option<Running>,
    cue: Option<GestureCue>,
    serial: u32,
}

impl NpcGestures {
    /// A body that has never gestured.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            last: [None; 4],
            active: None,
            cue: None,
            serial: 0,
        }
    }

    /// What to play this tick, if anything.
    ///
    /// Written once per tick by [`advance_gestures`]; read by the animation layer, which needs no
    /// clock of its own to know how far into the gesture the body is.
    #[must_use]
    pub const fn cue(&self) -> Option<GestureCue> {
        self.cue
    }

    /// The kind currently playing, if any.
    #[must_use]
    pub const fn active_kind(&self) -> Option<GestureKind> {
        match self.active {
            Some(running) => Some(running.kind),
            None => None,
        }
    }

    /// Whether this kind may start now, given the body's own cooldown for it.
    #[must_use]
    pub fn ready_at(&self, now: Tick, kind: GestureKind) -> bool {
        self.last
            .get(kind.ordinal())
            .copied()
            .flatten()
            .is_none_or(|last| now.elapsed_at_least(last, kind.cooldown_ticks()))
    }

    /// How long since this body last gestured this way, or `None` if it never has.
    #[must_use]
    pub fn since(&self, now: Tick, kind: GestureKind) -> Option<u64> {
        self.last
            .get(kind.ordinal())
            .copied()
            .flatten()
            .map(|last| now.since(last))
    }

    /// Start a gesture now, replacing whatever was playing.
    fn begin(&mut self, kind: GestureKind, now: Tick) {
        self.serial = self.serial.wrapping_add(1);
        self.active = Some(Running {
            kind,
            started: now,
            serial: self.serial,
        });
        if let Some(slot) = self.last.get_mut(kind.ordinal()) {
            *slot = Some(now);
        }
    }

    /// Compute this tick's weight, and forget a gesture that has finished.
    ///
    /// The final tick of a gesture reports a weight of exactly zero rather than reporting nothing:
    /// a consumer that stops seeing the cue keeps whatever weight it last wrote, and "the blend
    /// ended at zero" and "the blend ended at one twenty-fourth" are very different poses.
    fn advance(&mut self, now: Tick, config: &GestureConfig) {
        self.cue = self.active.and_then(|running| {
            envelope(now.since(running.started), running.kind, config).map(|weight| GestureCue {
                kind: running.kind,
                weight,
                serial: running.serial,
            })
        });
        if self.cue.is_none() {
            self.active = None;
        }
    }
}

/// Ask for a gesture from a behaviour leaf.
///
/// The leaf-side API: a leaf has `&self` and a `&mut Blackboard`, so the request goes on the
/// blackboard under [`KEY_GESTURE`] and [`collect_gesture_requests`] picks it up. Writing twice
/// before the collector runs asks once, which is what a leaf stuck in a `Running` state wants.
pub fn request_gesture(board: &mut Blackboard, kind: GestureKind) {
    let _ = board.set(KEY_GESTURE, Value::Int(num::usize_to_i64(kind.ordinal())));
}

/// Move the gesture requests leaves wrote onto the blackboard into the channel.
///
/// Runs in [`FlockSet::Cognition`](crate::adapters::primary::plugin::FlockSet) after the chosen
/// brains have ticked, so a leaf's request is adjudicated on the tick it was made rather than the
/// next one. The slot is consumed either way: a request that cannot be honoured must not be re-read
/// every tick until the crowd stops thinking.
#[expect(
    clippy::needless_pass_by_value,
    reason = "Bevy hands a system its parameters by value; `&Res<T>` is not a `SystemParam`"
)]
pub fn collect_gesture_requests(
    clock: Res<SimClock>,
    mut channel: ResMut<GestureChannel>,
    mut boards: Query<(&Npc, &mut NpcBlackboard)>,
) {
    let now = clock.tick;
    for (npc, mut board) in &mut boards {
        let Some(Value::Int(raw)) = board.0.get(KEY_GESTURE) else {
            continue;
        };
        let _ = board.0.remove(KEY_GESTURE);
        let Some(kind) = kind_from_request(raw) else {
            continue;
        };
        let _ = channel.request(GestureRequest {
            npc: npc.0,
            kind,
            at: now,
        });
    }
}

/// Adjudicate the channel and advance every body's gesture.
///
/// Two passes, in this order: first every running gesture is advanced, so a gesture that ended on
/// this tick is already looking retired when the channel is drained; then at most
/// [`GestureConfig::max_per_tick`] requests are considered. A request refused for its cooldown or
/// because something more urgent is playing is *counted* and dropped — queuing it would make the
/// crowd's behaviour depend on how long the queue had grown, which is the failure mode the bark path
/// documents.
#[expect(
    clippy::needless_pass_by_value,
    reason = "Bevy hands a system its parameters by value; `&Res<T>` is not a `SystemParam`"
)]
pub fn advance_gestures(
    clock: Res<SimClock>,
    config: Res<GestureConfig>,
    mut channel: ResMut<GestureChannel>,
    mut bodies: Query<(&Npc, &mut NpcGestures)>,
) {
    let now = clock.tick;
    for (_, mut state) in &mut bodies {
        state.advance(now, &config);
    }
    let mut started = 0_u32;
    while started < config.max_per_tick() {
        let Some(request) = channel.pop() else {
            break;
        };
        let Some((_, mut state)) = bodies.iter_mut().find(|(npc, _)| npc.0 == request.npc) else {
            // The NPC despawned between asking and gesturing, or was never given gesture state:
            // drop the request rather than holding the queue open for a ghost.
            channel.dropped_absent = channel.dropped_absent.saturating_add(1);
            continue;
        };
        if !state.ready_at(now, request.kind) {
            channel.refused_cooldown = channel.refused_cooldown.saturating_add(1);
            continue;
        }
        if let Some(playing) = state.active_kind()
            && request.kind <= playing
        {
            channel.refused_busy = channel.refused_busy.saturating_add(1);
            continue;
        }
        state.begin(request.kind, now);
        state.advance(now, &config);
        channel.accepted = channel.accepted.saturating_add(1);
        started = started.saturating_add(1);
    }
}

/// The weight of a gesture `elapsed` ticks in, or `None` once it is over.
///
/// Three linear stages in ticks: up over the blend-in, one for the kind's hold, down over the
/// blend-out. The last tick of the ramp down reports exactly zero and the tick after reports nothing,
/// so the pose side writes a clean zero before it stops being asked to.
fn envelope(elapsed: u64, kind: GestureKind, config: &GestureConfig) -> Option<f32> {
    let blend_in = u64::from(config.blend_in());
    let blend_out = u64::from(config.blend_out());
    let hold = kind.duration_ticks();
    let total = blend_in.saturating_add(hold).saturating_add(blend_out);
    if elapsed > total {
        return None;
    }
    if elapsed < blend_in {
        return Some(ramp(elapsed, blend_in));
    }
    if elapsed < blend_in.saturating_add(hold) {
        return Some(1.0);
    }
    Some(1.0 - ramp(elapsed - blend_in - hold, blend_out))
}

/// A ramp from nothing to everything, exactly one at the end of it.
fn ramp(elapsed: u64, ticks: u64) -> f32 {
    if elapsed >= ticks {
        return 1.0;
    }
    num::u64_to_f32(elapsed) / num::u64_to_f32(ticks)
}

/// The kind a blackboard request names, if any.
const fn kind_from_request(raw: i64) -> Option<GestureKind> {
    let ordinal = num::i64_to_u32(raw);
    let ordinal = num::u32_to_u16(ordinal);
    GestureKind::from_ordinal(ordinal)
}

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_CAPACITY, GestureChannel, GestureConfig, GestureKind, GestureRequest, KEY_GESTURE,
        NpcGestures, advance_gestures, collect_gesture_requests, request_gesture,
    };
    use crate::adapters::primary::cognition::NpcBlackboard;
    use crate::adapters::primary::components::Npc;
    use crate::application::SimClock;
    use crate::behavior::blackboard::Value;
    use crate::domain::ids::NpcId;
    use crate::domain::tick::Tick;
    use crate::testkit::OrDie;
    use bevy_ecs::prelude::{Entity, World};
    use bevy_ecs::system::RunSystemOnce;

    /// A world with a clock, a channel and one NPC that can gesture.
    fn world() -> (World, Entity) {
        let mut world = World::new();
        world.insert_resource(SimClock::default());
        world.insert_resource(GestureConfig::default());
        world.insert_resource(GestureChannel::new(DEFAULT_CAPACITY));
        let entity = world
            .spawn((Npc(NpcId::unique(0)), NpcGestures::new()))
            .id();
        (world, entity)
    }

    /// One tick: advance the crowd, then move the clock on, as the plugin's sets do.
    fn tick(world: &mut World) {
        world.run_system_once(advance_gestures).or_die();
        world.resource_mut::<SimClock>().advance();
    }

    fn channel(world: &World) -> &GestureChannel {
        world.resource::<GestureChannel>()
    }

    #[test]
    fn four_identical_requests_in_ten_ticks_produce_one_gesture_and_three_suppressions() {
        let (mut world, entity) = world();
        let npc = *world.get::<Npc>(entity).or_die();
        // One request every third tick: identical in every way that matters, and spread far enough
        // apart that a queue-full refusal could not excuse the answer.
        for tick_index in [0_u64, 3, 6, 9] {
            world.resource_mut::<GestureChannel>().ask(
                npc.0,
                GestureKind::Wave,
                Tick::new(tick_index),
            );
            tick(&mut world);
        }
        assert_eq!(
            channel(&world).accepted(),
            1,
            "{}",
            channel(&world).accepted()
        );
        assert_eq!(channel(&world).refused_cooldown(), 3);
        assert_eq!(channel(&world).pending(), 0);
        assert_eq!(
            world.get::<NpcGestures>(entity).or_die().active_kind(),
            Some(GestureKind::Wave)
        );
    }

    #[test]
    fn a_flood_of_ten_thousand_requests_in_one_tick_does_not_grow_the_queue() {
        let (mut world, entity) = world();
        let npc = *world.get::<Npc>(entity).or_die();
        {
            let mut channel = world.resource_mut::<GestureChannel>();
            for _ in 0..10_000 {
                channel.ask(npc.0, GestureKind::Nod, Tick::ZERO);
            }
            // The bound is the whole allocation argument: a request refused at the door costs an
            // increment, not a place in a deque that a game can grow a million at a time.
            let capacity = channel.capacity();
            assert_eq!(channel.pending(), capacity);
            assert_eq!(
                channel.refused_full(),
                10_000 - u64::from(num_u64(capacity))
            );
        }
        tick(&mut world);
        // Adjudicated at the per-tick cap, and what is left stays bounded.
        assert_eq!(channel(&world).accepted(), 1);
        assert!(channel(&world).pending() <= channel(&world).capacity());
    }

    /// A `usize` as a `u64`, for counting against the channel's own counters.
    fn num_u64(value: usize) -> u32 {
        crate::domain::num::usize_to_u32(value)
    }

    #[test]
    fn a_higher_priority_gesture_interrupts_a_lower_one() {
        let (mut world, entity) = world();
        let npc = *world.get::<Npc>(entity).or_die();
        world
            .resource_mut::<GestureChannel>()
            .ask(npc.0, GestureKind::Wave, Tick::ZERO);
        tick(&mut world);
        assert_eq!(
            world.get::<NpcGestures>(entity).or_die().active_kind(),
            Some(GestureKind::Wave)
        );

        world
            .resource_mut::<GestureChannel>()
            .ask(npc.0, GestureKind::Flinch, Tick::new(1));
        tick(&mut world);
        let state = *world.get::<NpcGestures>(entity).or_die();
        assert_eq!(state.active_kind(), Some(GestureKind::Flinch));
        assert_eq!(state.cue().map(|cue| cue.serial()), Some(2));
        // The interrupted wave is still on its own cooldown: the stamp is not part of the run.
        assert!(!state.ready_at(Tick::new(1), GestureKind::Wave));
        assert!(state.ready_at(Tick::new(180), GestureKind::Wave));
    }

    #[test]
    fn a_lower_priority_gesture_does_not_interrupt_a_higher_one() {
        let (mut world, entity) = world();
        let npc = *world.get::<Npc>(entity).or_die();
        world
            .resource_mut::<GestureChannel>()
            .ask(npc.0, GestureKind::Flinch, Tick::ZERO);
        tick(&mut world);
        world
            .resource_mut::<GestureChannel>()
            .ask(npc.0, GestureKind::Wave, Tick::new(1));
        tick(&mut world);
        assert_eq!(
            world.get::<NpcGestures>(entity).or_die().active_kind(),
            Some(GestureKind::Flinch)
        );
        assert_eq!(channel(&world).refused_busy(), 1);
    }

    #[test]
    fn the_envelope_rises_holds_falls_and_ends_at_zero() {
        let config = GestureConfig::new(2, 2, 32);
        let mut state = NpcGestures::new();
        state.begin(GestureKind::Nod, Tick::ZERO);
        let hold = GestureKind::Nod.duration_ticks();
        // Blend in over two ticks: nothing, halfway, full.
        state.advance(Tick::ZERO, &config);
        assert!(state.cue().or_die().weight().abs() < f32::EPSILON);
        state.advance(Tick::new(1), &config);
        assert!((state.cue().or_die().weight() - 0.5).abs() < 1e-6);
        state.advance(Tick::new(2), &config);
        assert!((state.cue().or_die().weight() - 1.0).abs() < f32::EPSILON);
        // Held through the hold.
        state.advance(Tick::new(2 + hold - 1), &config);
        assert!((state.cue().or_die().weight() - 1.0).abs() < f32::EPSILON);
        // Falling through the blend out.
        state.advance(Tick::new(2 + hold), &config);
        assert!((state.cue().or_die().weight() - 1.0).abs() < f32::EPSILON);
        state.advance(Tick::new(3 + hold), &config);
        assert!((state.cue().or_die().weight() - 0.5).abs() < 1e-6);
        // And the last tick is exactly zero, so the pose side can write a clean zero.
        state.advance(Tick::new(4 + hold), &config);
        assert_eq!(state.cue().or_die().weight().to_bits(), 0.0_f32.to_bits());
        // After which there is nothing to play at all.
        state.advance(Tick::new(5 + hold), &config);
        assert!(state.cue().is_none());
        assert!(state.active_kind().is_none());
    }

    #[test]
    fn a_gesture_that_never_started_has_no_cue() {
        let mut state = NpcGestures::new();
        state.advance(Tick::new(100), &GestureConfig::default());
        assert!(state.cue().is_none());
    }

    #[test]
    fn a_request_for_a_missing_npc_is_dropped_rather_than_stalling_the_queue() {
        let (mut world, _) = world();
        world.resource_mut::<GestureChannel>().ask(
            NpcId::unique(999),
            GestureKind::Wave,
            Tick::ZERO,
        );
        tick(&mut world);
        assert_eq!(channel(&world).dropped_absent(), 1);
        assert_eq!(channel(&world).pending(), 0);
        assert_eq!(channel(&world).accepted(), 0);
    }

    #[test]
    fn the_per_tick_cap_spreads_a_burst_over_ticks() {
        let (mut world, first) = world();
        world.insert_resource(GestureConfig::new(6, 8, 1));
        let second = world
            .spawn((Npc(NpcId::unique(1)), NpcGestures::new()))
            .id();
        for entity in [first, second] {
            let npc = *world.get::<Npc>(entity).or_die();
            world
                .resource_mut::<GestureChannel>()
                .ask(npc.0, GestureKind::Bark, Tick::ZERO);
        }
        tick(&mut world);
        assert_eq!(channel(&world).accepted(), 1);
        assert_eq!(channel(&world).pending(), 1);
        tick(&mut world);
        assert_eq!(channel(&world).accepted(), 2);
        assert_eq!(channel(&world).pending(), 0);
    }

    #[test]
    fn a_leaf_request_becomes_a_gesture_on_the_same_tick() {
        let (mut world, entity) = world();
        let mut board = NpcBlackboard::default();
        request_gesture(&mut board.0, GestureKind::Nod);
        world.entity_mut(entity).insert(board);

        world.run_system_once(collect_gesture_requests).or_die();
        assert_eq!(channel(&world).pending(), 1);
        // The slot is consumed, so a leaf that keeps writing while it runs does not pile up requests
        // within one tick.
        assert!(
            world
                .entity(entity)
                .get::<NpcBlackboard>()
                .and_then(|board| board.0.get(KEY_GESTURE))
                .is_none()
        );

        world.run_system_once(advance_gestures).or_die();
        assert_eq!(channel(&world).accepted(), 1);
        assert_eq!(
            world.get::<NpcGestures>(entity).or_die().active_kind(),
            Some(GestureKind::Nod)
        );
    }

    #[test]
    fn a_nonsense_blackboard_value_is_ignored_rather_than_guessed_at() {
        let (mut world, entity) = world();
        let mut board = NpcBlackboard::default();
        let _ = board.0.set(KEY_GESTURE, Value::Int(99));
        world.entity_mut(entity).insert(board);
        world.run_system_once(collect_gesture_requests).or_die();
        assert_eq!(channel(&world).pending(), 0);
        tick(&mut world);
        assert_eq!(channel(&world).accepted(), 0);
    }

    #[test]
    fn the_channel_is_bounded_even_at_zero_capacity() {
        let channel = GestureChannel::new(0);
        assert_eq!(channel.capacity(), 1);
        assert!(GestureChannel::new(DEFAULT_CAPACITY).capacity() >= 4);
    }

    #[test]
    fn kinds_are_ordered_by_priority_and_named() {
        let all = GestureKind::ALL;
        for (index, kind) in all.iter().enumerate() {
            assert_eq!(kind.ordinal(), index);
            assert_eq!(GestureKind::from_ordinal(num_u16(index)), Some(*kind));
            assert!(!kind.as_str().is_empty());
            assert!(kind.cooldown_ticks() > kind.duration_ticks());
        }
        assert!(GestureKind::Flinch > GestureKind::Wave);
        assert!(GestureKind::Wave > GestureKind::Nod);
        assert_eq!(GestureKind::from_ordinal(4), None);
    }

    /// A `usize` as a `u16`, for the ordinal round trip.
    fn num_u16(value: usize) -> u16 {
        crate::domain::num::usize_to_u16(value)
    }

    #[test]
    fn a_configuration_of_zeros_is_raised_to_one_tick() {
        let config = GestureConfig::new(0, 0, 0);
        assert_eq!(config.blend_in(), 1);
        assert_eq!(config.blend_out(), 1);
        assert_eq!(config.max_per_tick(), 1);
    }

    #[test]
    fn a_request_carries_the_tick_it_was_made_on() {
        let request = GestureRequest {
            npc: NpcId::unique(3),
            kind: GestureKind::Bark,
            at: Tick::new(7),
        };
        assert_eq!(request.at, Tick::new(7));
        assert_eq!(request.kind.as_str(), "bark");
        // The default config blends over a tenth of a second each way at sixty ticks a second.
        let config = GestureConfig::default();
        assert_eq!(config.blend_in(), 6);
        assert_eq!(config.blend_out(), 8);
    }

    #[test]
    fn a_body_remembers_how_long_ago_it_gestured() {
        let (mut world, entity) = world();
        let npc = *world.get::<Npc>(entity).or_die();
        world
            .resource_mut::<GestureChannel>()
            .ask(npc.0, GestureKind::Bark, Tick::ZERO);
        tick(&mut world);
        for _ in 0..9 {
            tick(&mut world);
        }
        let state = *world.get::<NpcGestures>(entity).or_die();
        assert_eq!(state.since(Tick::new(10), GestureKind::Bark), Some(10));
        assert_eq!(state.since(Tick::new(10), GestureKind::Wave), None);
        assert!(!state.ready_at(Tick::new(10), GestureKind::Bark));
    }
}
