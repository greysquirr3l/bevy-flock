//! Same-frame interactions: an NPC answers in the tick it was spoken to.
//!
//! A dialogue system that resolves at the end of the frame, or worse on the next tick, cannot
//! be used for the thing games actually want: an NPC that reacts *now*. This module is the
//! synchronous path. A system — the game's own input handler, a bark trigger, a scripted
//! setpiece — pushes an [`InteractionRequest`] onto [`InteractionQueue`], and
//! [`resolve_interactions`] drains it in the same tick, in a fixed order, writing the reaction
//! onto the actor's blackboard before cognition runs. Nothing here blocks, allocates per
//! request after warm-up, or needs a future.
//!
//! # Why a queue rather than a direct call
//!
//! Three reasons, all of them practical:
//!
//! * **Determinism.** The queue is drained in submission order, then by priority, then by
//!   subject id. Two runs of the same replay resolve the same interactions in the same order,
//!   whatever order Bevy happened to iterate its archetypes in.
//! * **Budget.** Ten thousand NPCs can all want to greet the player on the same tick. The
//!   budget says how many answers are affordable; the rest stay queued, in order, and are
//!   served next tick. A crowd does not get to spike a frame because it noticed you.
//! * **Separation.** The thing that *decides* to speak (a tree leaf, a director, a networked
//!   player) does not need `ResMut<MemoryStore>`. It needs the queue, which is a plain
//!   `Vec` behind a resource.
//!
//! # Fatigue
//!
//! Talking to the same person four times in a second should not read as four times the
//! friendship. `InteractionLog` records recent pairs, and a repeat inside the
//! configured window earns half the familiarity and a colder reaction — the same mechanism
//! that stops a bark from looping, applied to the memory behind it.

use bevy_app::prelude::{App, Update};
use bevy_ecs::prelude::{Entity, Query, Res, ResMut, Resource};
use bevy_ecs::schedule::IntoScheduleConfigs;
use smallvec::SmallVec;

use crate::adapters::primary::cognition::NpcBlackboard;
use crate::adapters::primary::components::{Npc, NpcMood};
use crate::adapters::primary::memory_bridge::NpcMemoryRow;
use crate::adapters::primary::plugin::FlockSet;
use crate::application::SimClock;
use crate::behavior::blackboard::{BlackboardKey, Value};
use crate::domain::ids::NpcId;
use crate::domain::memory::{
    DecayPolicy, Encounter, EncounterKind, MemoryIndex, MemoryStore, SubjectId,
};
use crate::domain::num;
use crate::domain::tick::Tick;

/// How the last interaction went, as a score a tree can compare.
pub const KEY_INTERACTION_REACTION: BlackboardKey = BlackboardKey::new(46);

/// Which interaction the score belongs to, so a tree can say "a threat, and badly".
pub const KEY_INTERACTION_KIND: BlackboardKey = BlackboardKey::new(47);

/// What an NPC is doing when it interacts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum InteractionKind {
    /// Hello. The cheapest, most common interaction in a crowd.
    Greet,
    /// A question, answered with a name or a direction.
    Ask,
    /// A trade, which both sides remember.
    Trade,
    /// A threat, which is the fastest way to make an enemy.
    Threaten,
    /// An offer of help, which is the fastest way to make a friend.
    Help,
}

impl InteractionKind {
    /// How this reads as a memory.
    #[must_use]
    pub const fn encounter(self) -> EncounterKind {
        match self {
            Self::Greet | Self::Ask => EncounterKind::Spoke,
            Self::Trade => EncounterKind::Traded,
            Self::Threaten => EncounterKind::Attacked,
            Self::Help => EncounterKind::Helped,
        }
    }

    /// How much this interaction adds to the encounter it is recorded as.
    ///
    /// The domain's [`EncounterKind`] table is the single calibration for what an event feels
    /// like — a hello is worth 24, a threat 260 — and an interaction is an event with an
    /// emphasis. Adding a second table here would let the two drift, and the one that drifts
    /// silently is whichever a test does not pin.
    #[must_use]
    pub const fn delta(self) -> i16 {
        match self {
            Self::Greet => 8,
            Self::Ask => 0,
            Self::Trade => 16,
            Self::Threaten => -40,
            Self::Help => 32,
        }
    }

    /// The emotional charge the encounter carries, in the memory's units.
    #[must_use]
    pub const fn valence(self) -> i16 {
        self.encounter().valence().saturating_add(self.delta())
    }

    /// How much of a mark the encounter leaves.
    ///
    /// Inherited from the encounter kind rather than restated: a threat is memorable because
    /// an attack is, and there is one place that decides how memorable an attack is.
    #[must_use]
    pub const fn weight(self) -> u16 {
        self.encounter().weight()
    }

    /// How urgent the interaction is when the budget binds.
    #[must_use]
    pub const fn priority(self) -> i8 {
        match self {
            Self::Threaten => 40,
            Self::Help => 30,
            Self::Trade => 20,
            Self::Ask => 10,
            Self::Greet => 0,
        }
    }

    /// A stable name, for logs and for a game's own tables.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Greet => "greet",
            Self::Ask => "ask",
            Self::Trade => "trade",
            Self::Threaten => "threaten",
            Self::Help => "help",
        }
    }
}

/// How an NPC took it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Reaction {
    /// They were pleased to see you: a friend, or someone in a good mood.
    Warm,
    /// They tolerated you.
    Neutral,
    /// Careful: something about you, or the mood, has them hedging.
    Wary,
    /// They want you gone, and remember why.
    Hostile,
}

impl Reaction {
    /// The reaction implied by a score.
    #[must_use]
    const fn from_score(score: i32) -> Self {
        if score >= 20 {
            Self::Warm
        } else if score <= -20 {
            Self::Hostile
        } else if score < 0 {
            Self::Wary
        } else {
            Self::Neutral
        }
    }

    /// A stable name, for logs.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Warm => "warm",
            Self::Neutral => "neutral",
            Self::Wary => "wary",
            Self::Hostile => "hostile",
        }
    }
}

/// A request to interact, made by anyone who can reach the queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InteractionRequest {
    /// The NPC doing the interacting.
    pub actor: Entity,
    /// How many ticks ago it was submitted.
    pub submitted: Tick,
    /// Who they are dealing with.
    pub subject: SubjectId,
    /// What they are doing.
    pub kind: InteractionKind,
    /// Where it sorts against other requests in the same tick.
    pub priority: i8,
}

/// What happened, for the game to read this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InteractionOutcome {
    /// The NPC.
    pub actor: Entity,
    /// Who they dealt with.
    pub subject: SubjectId,
    /// What they did.
    pub kind: InteractionKind,
    /// How it was taken.
    pub reaction: Reaction,
    /// Whether it was a repeat inside the fatigue window.
    pub repeated: bool,
}

/// How many interactions are affordable, and how many can wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Resource)]
pub struct InteractionConfig {
    /// Answers resolved per tick. Above this, requests wait in order.
    pub per_tick: u32,
    /// How many requests may wait. Beyond this, submissions are refused and counted, because
    /// an unbounded queue is a memory leak with good manners.
    pub queue: u32,
    /// How many ticks a repeat of the same pair counts as fatigue rather than news.
    pub revisit: u32,
}

impl InteractionConfig {
    /// A budget a village can afford.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            per_tick: 64,
            queue: 4_096,
            revisit: 600,
        }
    }

    /// Change the per-tick budget.
    #[must_use]
    pub const fn with_budget(mut self, per_tick: u32) -> Self {
        self.per_tick = per_tick;
        self
    }

    /// Change how many requests may wait.
    #[must_use]
    pub const fn with_queue(mut self, queue: u32) -> Self {
        self.queue = queue;
        self
    }

    /// Change the fatigue window, in ticks.
    #[must_use]
    pub const fn with_revisit(mut self, ticks: u32) -> Self {
        self.revisit = ticks;
        self
    }
}

impl Default for InteractionConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// Requests waiting to be answered.
///
/// A resource, not a component: there is exactly one and it is drained every tick.
#[derive(Debug, Default, Resource)]
pub struct InteractionQueue {
    pending: Vec<InteractionRequest>,
    order: u64,
}

impl InteractionQueue {
    /// Submit a request. Returns `false` when the queue is full, which the caller should treat
    /// as "too many people want a word at once" rather than as an error.
    pub fn submit(
        &mut self,
        actor: Entity,
        subject: SubjectId,
        kind: InteractionKind,
        at: Tick,
        queue: u32,
    ) -> bool {
        if self.pending.len() >= queue as usize {
            return false;
        }
        self.order = self.order.wrapping_add(1);
        self.pending.push(InteractionRequest {
            actor,
            submitted: at,
            subject,
            kind,
            priority: kind.priority(),
        });
        true
    }

    /// How many are waiting.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.pending.len()
    }

    /// Whether anything is waiting.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// The waiting requests, in submission order.
    #[must_use]
    pub fn pending(&self) -> &[InteractionRequest] {
        &self.pending
    }

    /// The submission counter, which is only observable through ordering.
    #[must_use]
    pub const fn submissions(&self) -> u64 {
        self.order
    }

    /// Drop everything, for a scene change.
    pub fn clear(&mut self) {
        self.pending.clear();
    }
}

/// The answers given, and the recent pairs used for fatigue.
#[derive(Debug, Default, Resource)]
pub struct InteractionLog {
    resolved: Vec<InteractionOutcome>,
    recent: SmallVec<[(u64, SubjectId, Tick); 64]>,
    repeated: u32,
    refused: u32,
    deferred: u32,
}

impl InteractionLog {
    /// The answers from the last tick.
    #[must_use]
    pub fn resolved(&self) -> &[InteractionOutcome] {
        &self.resolved
    }

    /// How many of the last tick's answers were repeats inside the fatigue window.
    #[must_use]
    pub const fn repeated(&self) -> u32 {
        self.repeated
    }

    /// How many submissions were refused because the queue was full, cumulative.
    #[must_use]
    pub const fn refused(&self) -> u32 {
        self.refused
    }

    /// How many requests were left waiting this tick.
    #[must_use]
    pub const fn deferred(&self) -> u32 {
        self.deferred
    }

    const fn note_refusal(&mut self) {
        self.refused = self.refused.saturating_add(1);
    }

    const fn note_repeat(&mut self) {
        self.repeated = self.repeated.saturating_add(1);
    }

    /// Whether this pair already spoke recently, and record that they have now.
    fn revisit(&mut self, actor: NpcId, subject: SubjectId, now: Tick, window: u32) -> bool {
        let key = (u64::from(actor.to_bits()), subject);
        let recent =
            !self
                .recent
                .iter()
                .rev()
                .take(64)
                .all(|(seen_actor, seen_subject, seen_at)| {
                    *seen_actor != key.0
                        || *seen_subject != subject
                        || now.since(*seen_at) > u64::from(window)
                });
        if self.recent.len() >= 64 {
            self.recent.remove(0);
        }
        self.recent.push((key.0, subject, now));
        recent
    }
}

/// Answer queued interactions: in order, within budget, in the tick they were submitted.
///
/// The reaction is written to the actor's blackboard under
/// [`KEY_INTERACTION_REACTION`] and [`KEY_INTERACTION_KIND`] before cognition runs, so a
/// behaviour tree can branch on "they liked that" in the same frame the smile happened.
#[expect(
    clippy::needless_pass_by_value,
    reason = "Bevy hands a system its parameters by value; `&Res<T>` is not a `SystemParam`"
)]
pub fn resolve_interactions(
    clock: Res<SimClock>,
    config: Res<InteractionConfig>,
    mut queue: ResMut<InteractionQueue>,
    mut log: ResMut<InteractionLog>,
    mut store: ResMut<MemoryStore>,
    mut minds: Query<(&Npc, &NpcMemoryRow, Option<&NpcMood>, &mut NpcBlackboard)>,
) {
    let now = clock.tick;
    log.resolved.clear();
    log.repeated = 0;
    if queue.pending.is_empty() {
        log.deferred = 0;
        return;
    }

    // Submission order first, so nothing starves; priority second, so a threat is answered
    // before a hello on the same tick; then the subject, so the resolution is independent of
    // the order Bevy happened to build the query in.
    queue.pending.sort_unstable_by(|left, right| {
        left.submitted
            .cmp(&right.submitted)
            .then(right.priority.cmp(&left.priority))
            .then(left.subject.cmp(&right.subject))
            .then(left.actor.index().cmp(&right.actor.index()))
    });

    let budget = usize::try_from(config.per_tick).unwrap_or(usize::MAX);
    let take = budget.min(queue.pending.len());
    let mut answered: Vec<InteractionRequest> = queue.pending.drain(..take).collect();
    log.deferred = u32::try_from(queue.pending.len()).unwrap_or(u32::MAX);
    // Drain order is already deterministic, but a second sort makes the invariant explicit
    // and keeps a hand-built queue honest.
    answered.sort_unstable_by_key(|request| {
        (
            request.submitted,
            std::cmp::Reverse(request.priority),
            request.subject,
            request.actor.index(),
        )
    });

    for request in answered {
        let Ok((npc, row, mood, mut board)) = minds.get_mut(request.actor) else {
            // The actor was despawned between submission and resolution. Dropping the
            // request is the only sane answer: there is nobody left to have an opinion.
            continue;
        };
        let valence = mood.map_or(0.0, |mood| mood.0.valence);
        unravel_one(
            Recording {
                store: &mut store,
                row: row.0,
                log: &mut log,
                board: &mut board,
            },
            npc.0,
            valence,
            request,
            now,
            config.revisit,
            InteractionConfig::decay(),
        );
    }
}

impl InteractionConfig {
    /// The decay the store is read through. The memory bridge owns the authoritative policy;
    /// this is the default a standalone interaction layer uses.
    const fn decay() -> DecayPolicy {
        DecayPolicy::MINUTE
    }
}

/// The durable state one interaction is recorded against.
///
/// Every field is borrowed, so the struct is a view rather than a copy: it exists to keep the
/// resolver's signature about the *decision* and the state about the *store*.
struct Recording<'a> {
    /// The social-memory store the interaction is recorded in.
    store: &'a mut MemoryStore,
    /// The store row belonging to the actor.
    row: MemoryIndex,
    /// Everything the pass has written so far, including the revisit window.
    log: &'a mut InteractionLog,
    /// The actor's cognition blackboard, which receives the reaction.
    board: &'a mut NpcBlackboard,
}

/// Record one interaction and write its reaction onto the actor's blackboard.
fn unravel_one(
    recording: Recording<'_>,
    actor: NpcId,
    mood: f32,
    request: InteractionRequest,
    now: Tick,
    window: u32,
    decay: DecayPolicy,
) {
    let Recording {
        store,
        row,
        log,
        board,
    } = recording;
    let repeated = log.revisit(actor, request.subject, now, window);
    if repeated {
        log.note_repeat();
    }
    let remembered = store.recall_decayed(row, request.subject, now, decay);
    let reaction = reaction_of(remembered, mood, request.kind, repeated);

    // A repeat is worth half: the memory of a hello should not compound at typing speed, so a
    // third hello leaves the same mark as the first rather than three times as much.
    let valence = if repeated {
        request.kind.valence() / 2
    } else {
        request.kind.valence()
    };
    let encounter = Encounter::new(request.kind.encounter(), now).with_valence(valence);
    let recorded = store.remember(row, request.subject, &encounter);
    if !recorded {
        // The subject is out of the store's range or the row was reclaimed; the interaction
        // still happened, it just is not memorable, which is what a nameless extra is.
        log.note_refusal();
    }

    let _ = board.0.set(
        KEY_INTERACTION_KIND,
        Value::Int(i64::from(match request.kind {
            InteractionKind::Greet => 0,
            InteractionKind::Ask => 1,
            InteractionKind::Trade => 2,
            InteractionKind::Threaten => 3,
            InteractionKind::Help => 4,
        })),
    );
    let _ = board.0.set(
        KEY_INTERACTION_REACTION,
        Value::Int(i64::from(score_of(
            remembered,
            mood,
            request.kind,
            repeated,
        ))),
    );

    log.resolved.push(InteractionOutcome {
        actor: request.actor,
        subject: request.subject,
        kind: request.kind,
        reaction,
        repeated,
    });
}

/// The reaction score: memory, mood and the act itself, in one integer.
///
/// Memory dominates, because a grudge should outrank a good mood; mood is worth a nudge and
/// the act itself is worth its own charge. The scale is arbitrary but fixed, so a game can
/// compare scores across NPCs and a test can pin them.
fn score_of(
    remembered: Option<(i16, u16)>,
    mood: f32,
    kind: InteractionKind,
    repeated: bool,
) -> i32 {
    // The store's units, observed and then pinned by the table test below: a remembered
    // valence runs to a few hundred after a strong event and settles near a tenth of that, and
    // familiarity accumulates by the encounter weight (8 for a hello, 96 for an attack).
    const VALENCE_PER_POINT: i32 = 4;
    const FAMILIARITY_PER_POINT: i32 = 16;
    const FAMILIARITY_CEILING: u16 = 256;
    const MOOD_POINTS: f32 = 20.0;
    const DELTA_PER_POINT: i32 = 8;
    const FATIGUE_POINTS: i32 = 12;

    let mood_points = num::f32_to_i32(mood.clamp(-1.0, 1.0) * MOOD_POINTS);
    let act = i32::from(kind.delta()) / DELTA_PER_POINT;
    let fatigue = if repeated { -FATIGUE_POINTS } else { 0 };
    let Some((valence, familiarity)) = remembered else {
        // A stranger: the mood and the act itself, and nothing else.
        return mood_points.saturating_add(act).saturating_add(fatigue);
    };
    let trust = i32::from(valence) / VALENCE_PER_POINT;
    let ease = i32::from(familiarity.min(FAMILIARITY_CEILING)) / FAMILIARITY_PER_POINT;
    // Familiarity is not warmth. It makes a friend easier to be around and an enemy *more*
    // dangerous to stand next to, so it takes the sign of the opinion rather than adding to it.
    let ease = if valence < 0 { -ease } else { ease };
    trust
        .saturating_add(ease)
        .saturating_add(mood_points)
        .saturating_add(act)
        .saturating_add(fatigue)
}

/// The reaction an actor gives, given what it remembers and how it feels.
fn reaction_of(
    remembered: Option<(i16, u16)>,
    mood: f32,
    kind: InteractionKind,
    repeated: bool,
) -> Reaction {
    Reaction::from_score(score_of(remembered, mood, kind, repeated))
}

/// Adds the same-frame interaction layer, gated on the `memory` feature.
///
/// Opt-in rather than part of [`FlockPlugin`](crate::adapters::primary::plugin::FlockPlugin): it needs a social-memory store
/// to record into, and a game that has not enrolled its NPCs would be paying for resources and a
/// system it never reads. Adding both plugins in either order works — the interaction system runs
/// in [`FlockSet::Perception`], after the memory bridge's own perception pass.
#[derive(Debug, Clone, Copy, Default)]
pub struct InteractionPlugin {
    /// Queue limits, response thresholds and decay.
    pub config: InteractionConfig,
}

impl InteractionPlugin {
    /// A plugin with explicit configuration.
    #[must_use]
    pub const fn new(config: InteractionConfig) -> Self {
        Self { config }
    }
}

impl bevy_app::Plugin for InteractionPlugin {
    fn build(&self, app: &mut App) {
        register(app, self.config);
    }
}

/// Register the interaction layer.
pub fn register(app: &mut App, config: InteractionConfig) {
    app.insert_resource(config)
        .init_resource::<InteractionQueue>()
        .init_resource::<InteractionLog>()
        .add_systems(Update, resolve_interactions.in_set(FlockSet::Perception));
}

/// Submit a request from anywhere with the queue, counting a refusal in the log.
pub fn submit(
    queue: &mut InteractionQueue,
    log: &mut InteractionLog,
    config: &InteractionConfig,
    actor: Entity,
    subject: SubjectId,
    kind: InteractionKind,
    at: Tick,
) -> bool {
    let accepted = queue.submit(actor, subject, kind, at, config.queue);
    if !accepted {
        log.note_refusal();
    }
    accepted
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::primary::cognition::NpcBlackboard;
    use crate::testkit::{GetOrDie, OrDie};
    use bevy_app::prelude::App;
    use bevy_transform::prelude::Transform;

    fn app() -> App {
        let mut app = App::new();
        app.insert_resource(SimClock::default())
            .insert_resource(MemoryStore::new(64, 8))
            .insert_resource(InteractionConfig::new().with_budget(8))
            .init_resource::<InteractionQueue>()
            .init_resource::<InteractionLog>()
            .add_systems(bevy_app::prelude::Update, resolve_interactions);
        app
    }

    fn spawn(app: &mut App, id: u32, row: Option<u32>, subjects: u16) -> Entity {
        let mut entity = app.world_mut().spawn((
            Npc(NpcId::unique(id)),
            NpcBlackboard::default(),
            Transform::default(),
        ));
        if let Some(row) = row {
            entity.insert(NpcMemoryRow(MemoryIndex::new(row)));
        }
        let entity = entity.id();
        if let Some(row) = row {
            let mut store = app.world_mut().resource_mut::<MemoryStore>();
            let npc = NpcId::unique(id);
            // Enrol explicitly so the row number in the test is the row the store uses.
            let index = store.enrol(npc).or_die();
            assert_eq!(index.get(), row);
            let _ = subjects;
        }
        entity
    }

    #[test]
    fn an_interaction_is_answered_in_the_tick_it_was_made() {
        let mut app = app();
        let actor = spawn(&mut app, 1, Some(0), 8);
        app.world_mut().resource_mut::<InteractionQueue>().submit(
            actor,
            SubjectId::new(1),
            InteractionKind::Greet,
            Tick::ZERO,
            64,
        );
        app.update();
        let log = app.world().resource::<InteractionLog>();
        assert_eq!(log.resolved().len(), 1);
        assert_eq!(log.resolved().at(0).reaction, Reaction::Neutral);
        assert_eq!(log.deferred(), 0);
    }

    #[test]
    fn the_budget_is_spent_in_priority_order_and_the_rest_wait() {
        let mut app = app();
        let mut actors = Vec::new();
        for id in 0..20 {
            actors.push(spawn(&mut app, id, Some(id), 8));
        }
        {
            let mut queue = app.world_mut().resource_mut::<InteractionQueue>();
            for (index, actor) in actors.iter().enumerate() {
                let kind = if index % 5 == 0 {
                    InteractionKind::Threaten
                } else {
                    InteractionKind::Greet
                };
                queue.submit(*actor, SubjectId::new(1), kind, Tick::ZERO, 64);
            }
        }
        app.update();
        let log = app.world().resource::<InteractionLog>();
        assert_eq!(log.resolved().len(), 8, "the budget is eight a tick");
        assert_eq!(log.deferred(), 12);
        // All four threats were answered before any hello.
        let threats = log
            .resolved()
            .iter()
            .filter(|outcome| outcome.kind == InteractionKind::Threaten)
            .count();
        assert_eq!(threats, 4);
    }

    #[test]
    fn a_friend_is_greeted_warmly_and_a_grudge_hostile() {
        let mut app = app();
        let friend = spawn(&mut app, 1, Some(0), 8);
        let enemy = spawn(&mut app, 2, Some(1), 8);
        {
            let mut store = app.world_mut().resource_mut::<MemoryStore>();
            for _ in 0..3 {
                store.remember(
                    MemoryIndex::new(0),
                    SubjectId::new(1),
                    &Encounter::new(EncounterKind::Helped, Tick::ZERO),
                );
            }
            store.remember(
                MemoryIndex::new(1),
                SubjectId::new(1),
                &Encounter::new(EncounterKind::Attacked, Tick::ZERO),
            );
        }
        {
            let mut queue = app.world_mut().resource_mut::<InteractionQueue>();
            queue.submit(
                friend,
                SubjectId::new(1),
                InteractionKind::Greet,
                Tick::ZERO,
                64,
            );
            queue.submit(
                enemy,
                SubjectId::new(1),
                InteractionKind::Greet,
                Tick::ZERO,
                64,
            );
        }
        app.update();
        let log = app.world().resource::<InteractionLog>();
        let warm = log
            .resolved()
            .iter()
            .find(|outcome| outcome.actor == friend)
            .or_die();
        let cold = log
            .resolved()
            .iter()
            .find(|outcome| outcome.actor == enemy)
            .or_die();
        assert_eq!(warm.reaction, Reaction::Warm);
        assert_eq!(cold.reaction, Reaction::Hostile);
    }

    #[test]
    fn pestering_the_same_person_turns_a_hello_cold() {
        let mut app = app();
        let actor = spawn(&mut app, 1, Some(0), 8);
        app.update();
        for _ in 0..3 {
            app.world_mut().resource_mut::<InteractionQueue>().submit(
                actor,
                SubjectId::new(1),
                InteractionKind::Greet,
                Tick::ZERO,
                64,
            );
            app.update();
        }
        let log = app.world().resource::<InteractionLog>();
        assert_eq!(log.repeated(), 1, "the third hello is a repeat");
        assert_eq!(log.resolved().at(0).reaction, Reaction::Wary);
    }

    #[test]
    fn a_full_queue_refuses_and_counts_rather_than_growing() {
        let mut app = app();
        let actor = spawn(&mut app, 1, Some(0), 8);
        let config = InteractionConfig::new().with_queue(2);
        let mut queue = InteractionQueue::default();
        let mut log = InteractionLog::default();
        assert!(submit(
            &mut queue,
            &mut log,
            &config,
            actor,
            SubjectId::new(1),
            InteractionKind::Greet,
            Tick::ZERO
        ));
        assert!(submit(
            &mut queue,
            &mut log,
            &config,
            actor,
            SubjectId::new(2),
            InteractionKind::Greet,
            Tick::ZERO
        ));
        assert!(!submit(
            &mut queue,
            &mut log,
            &config,
            actor,
            SubjectId::new(3),
            InteractionKind::Greet,
            Tick::ZERO
        ));
        assert_eq!(queue.len(), 2);
        assert_eq!(log.refused(), 1);
    }

    #[test]
    fn a_despawned_actor_is_dropped_without_panicking() {
        let mut app = app();
        let actor = spawn(&mut app, 1, Some(0), 8);
        app.world_mut().resource_mut::<InteractionQueue>().submit(
            actor,
            SubjectId::new(1),
            InteractionKind::Greet,
            Tick::ZERO,
            64,
        );
        app.world_mut().despawn(actor);
        app.update();
        let log = app.world().resource::<InteractionLog>();
        assert!(log.resolved().is_empty());
    }

    #[test]
    fn a_subject_outside_the_stores_range_is_not_written() {
        let mut app = app();
        app.insert_resource(MemoryStore::new(16, 4));
        let actor = spawn(&mut app, 1, Some(0), 4);
        app.world_mut().resource_mut::<InteractionQueue>().submit(
            actor,
            SubjectId::new(60),
            InteractionKind::Greet,
            Tick::ZERO,
            64,
        );
        app.update();
        let store = app.world().resource::<MemoryStore>();
        assert!(
            store
                .recall(MemoryIndex::new(0), SubjectId::new(60))
                .is_none()
        );
    }

    #[test]
    fn the_reaction_is_visible_on_the_blackboard_the_same_tick() {
        let mut app = app();
        let actor = spawn(&mut app, 1, Some(0), 8);
        app.world_mut().resource_mut::<InteractionQueue>().submit(
            actor,
            SubjectId::new(1),
            InteractionKind::Help,
            Tick::ZERO,
            64,
        );
        app.update();
        let board = app.world().get::<NpcBlackboard>(actor).or_die();
        assert!(matches!(
            board.0.get(KEY_INTERACTION_REACTION),
            Some(Value::Int(_))
        ));
        assert_eq!(
            board.0.get(KEY_INTERACTION_KIND),
            Some(Value::Int(4)),
            "help is kind 4"
        );
    }

    #[test]
    fn a_repeat_scores_lower_than_a_first_meeting() {
        let first = score_of(None, 0.0, InteractionKind::Greet, false);
        let again = score_of(None, 0.0, InteractionKind::Greet, true);
        assert!(again < first);
        assert!(score_of(None, 0.5, InteractionKind::Greet, false) > first);
        assert!(score_of(None, -0.5, InteractionKind::Greet, false) < first);
    }
}
