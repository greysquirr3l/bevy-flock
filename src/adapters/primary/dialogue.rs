//! Barks and dialog on the schedule.
//!
//! The engines live in [`crate::behavior::bark`] and [`crate::behavior::dialog`];
//! this module connects them to entities and to a frame:
//!
//! * [`Barks`] is the resource a game submits [`BarkRequest`]s to. The system turns
//!   requests into at most [`Barks::max_per_tick`] events, honouring each NPC's
//!   per-category cooldown, so a thousand NPCs noticing the player produce a handful
//!   of lines rather than a wall of noise.
//! * [`DialogHub`] owns the provider queue. With no provider attached it still works:
//!   anything that misses its deadline is answered from the fallback table, so a game
//!   without an LLM behaves exactly like one whose LLM is having a bad day.
//!
//! Both write into bounded resources ([`Barks::queue`], [`DialogOutbox`]) that a game
//! drains when it plays the audio or fills the speech bubble. Nothing here blocks,
//! and nothing here allocates per NPC per frame.

use crate::adapters::primary::components::Npc;
use crate::application::SimClock;
use crate::behavior::bark::{BarkError, BarkEvent, BarkQueue, BarkRequest, BarkState, BarkTable};
use crate::behavior::provider::{
    DialogPrompt, DialogProvider, DialogReply, ProviderQueue, ReplySource, TemplateProvider,
};
use crate::domain::ids::NpcId;
use crate::domain::rng::RngStream;
use crate::domain::tick::Tick;
use bevy_ecs::prelude::*;
use std::collections::VecDeque;

/// An NPC's own talking history.
#[derive(Component, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NpcBarks(pub BarkState);

/// What an NPC wants to say, before the cooldowns have had their say.
#[derive(Resource)]
pub struct Barks {
    table: BarkTable,
    queue: BarkQueue,
    requests: VecDeque<BarkRequest>,
    stream: RngStream,
    max_per_tick: u32,
    suppressed: u64,
}

impl Barks {
    /// A bark system with a table and a queue.
    ///
    /// # Errors
    ///
    /// [`BarkError`] if the table is empty in a category or the queue has no room.
    pub fn new(table: BarkTable, queue_capacity: usize) -> Result<Self, BarkError> {
        Ok(Self {
            table,
            queue: BarkQueue::new(queue_capacity)?,
            requests: VecDeque::new(),
            stream: RngStream::new(crate::domain::ids::SimSeed::new(0xB4_2C), 0),
            max_per_tick: 8,
            suppressed: 0,
        })
    }

    /// A starter configuration.
    ///
    /// # Errors
    ///
    /// Cannot fail; the table and capacity are literals.
    pub fn starter() -> Result<Self, BarkError> {
        Self::new(BarkTable::starter()?, 64)
    }

    /// The most barks that may be produced in one tick.
    #[must_use]
    pub const fn max_per_tick(&self) -> u32 {
        self.max_per_tick
    }

    /// Change the per-tick cap.
    pub fn set_max_per_tick(&mut self, max: u32) {
        self.max_per_tick = max.max(1);
    }

    /// Ask an NPC to say something. Duplicate requests in the same tick are merged by
    /// the cooldown, so a game can be liberal about asking.
    pub fn request(&mut self, request: BarkRequest) {
        if self.requests.len() >= 1_024 {
            self.suppressed = self.suppressed.saturating_add(1);
            return;
        }
        self.requests.push_back(request);
    }

    /// The events waiting to be played.
    #[must_use]
    pub const fn queue(&self) -> &BarkQueue {
        &self.queue
    }

    /// The events waiting to be played, mutably, for a game that plays them.
    pub const fn queue_mut(&mut self) -> &mut BarkQueue {
        &mut self.queue
    }

    /// How many requests were refused because the request queue was full.
    #[must_use]
    pub const fn suppressed(&self) -> u64 {
        self.suppressed
    }

    /// The table, so a game can look up the text of an event's line.
    #[must_use]
    pub const fn table(&self) -> &BarkTable {
        &self.table
    }

    /// How many requests are waiting.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.requests.len()
    }
}

impl Default for Barks {
    fn default() -> Self {
        Self::starter().unwrap_or_else(|_| Self {
            table: BarkTable::default(),
            queue: BarkQueue::new(1).unwrap_or_else(|_| unreachable!()),
            requests: VecDeque::new(),
            stream: RngStream::new(crate::domain::ids::SimSeed::new(1), 0),
            max_per_tick: 8,
            suppressed: 0,
        })
    }
}

/// Turn requests into events, respecting cooldowns and the per-tick cap.
pub fn produce_barks(
    clock: Res<SimClock>,
    mut barks: ResMut<Barks>,
    mut npcs: Query<(&Npc, &mut NpcBarks)>,
) {
    if barks.requests.is_empty() {
        return;
    }
    let now = clock.tick;
    let cap = barks.max_per_tick;
    let mut produced = 0_u32;
    while produced < cap {
        let Some(request) = barks.requests.pop_front() else {
            break;
        };
        let Some((_, mut state)) = npcs.iter_mut().find(|(npc, _)| npc.0 == request.npc) else {
            // The NPC despawned between asking and speaking: drop the request rather
            // than speaking for a ghost.
            continue;
        };
        if !state.0.ready_at(now, request.category) {
            continue;
        }
        let avoid = state.0.last_line();
        // Split the borrow: the table is read while the stream advances.
        let Barks { table, stream, .. } = &mut *barks;
        let Some(line) = table.pick(request.category, avoid, stream) else {
            continue;
        };
        state.0.record(now, request.category, line);
        barks.queue.push(BarkEvent {
            npc: request.npc,
            category: request.category,
            line,
            at: now,
        });
        produced = produced.saturating_add(1);
    }
    // Anything left over stays queued for the next tick: dropping it would make a
    // large town's background chatter depend on how many NPCs noticed at once.
}

/// A line of dialog ready to display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpokenLine {
    /// Who said it.
    pub npc: NpcId,
    /// What was said.
    pub text: String,
    /// When.
    pub at: Tick,
    /// Whether it came from a provider or the fallback.
    pub source: ReplySource,
}

/// Lines waiting to be displayed.
#[derive(Resource, Debug, Default)]
pub struct DialogOutbox {
    lines: VecDeque<SpokenLine>,
}

impl DialogOutbox {
    /// The lines, oldest first.
    #[must_use]
    pub const fn lines(&self) -> &VecDeque<SpokenLine> {
        &self.lines
    }

    /// Take every line.
    pub fn drain(&mut self) -> impl Iterator<Item = SpokenLine> + '_ {
        self.lines.drain(..)
    }

    /// How many lines are waiting.
    #[must_use]
    pub fn len(&self) -> usize {
        self.lines.len()
    }

    /// Whether nothing is waiting.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }
}

/// A provider that never answers, so the queue always falls back.
#[derive(Debug, Clone, Copy, Default)]
pub struct SilentProvider;

impl DialogProvider for SilentProvider {
    fn submit(&mut self, _prompt: DialogPrompt) {}

    fn poll(&mut self, _now: Tick, _out: &mut VecDeque<DialogReply>) -> usize {
        0
    }

    fn label(&self) -> &'static str {
        "silent"
    }
}

/// The dialog provider queue, with a fallback table behind it.
#[derive(Resource)]
pub struct DialogHub {
    queue: ProviderQueue,
    provider: Option<Box<dyn DialogProvider>>,
    silent: SilentProvider,
    fallback: TemplateProvider,
    last: Option<Vec<DialogReply>>,
}

impl core::fmt::Debug for DialogHub {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("DialogHub")
            .field("provider", &self.provider_label())
            .field("inflight", &self.queue.inflight())
            .field("fallback", &self.fallback)
            .finish_non_exhaustive()
    }
}

impl DialogHub {
    /// A hub with a fallback table and no provider.
    #[must_use]
    pub fn new(fallback: TemplateProvider) -> Self {
        Self {
            queue: ProviderQueue::new(),
            provider: None,
            silent: SilentProvider,
            fallback,
            last: None,
        }
    }

    /// Attach a provider.
    pub fn set_provider(&mut self, provider: Box<dyn DialogProvider>) {
        self.provider = Some(provider);
    }

    /// Whether a provider is attached.
    #[must_use]
    pub fn has_provider(&self) -> bool {
        self.provider.is_some()
    }

    /// The provider's label.
    #[must_use]
    pub fn provider_label(&self) -> &'static str {
        self.provider
            .as_ref()
            .map_or("fallback-only", |provider| provider.label())
    }

    /// The queue, for the counters a game wants to ship.
    #[must_use]
    pub const fn queue(&self) -> &ProviderQueue {
        &self.queue
    }

    /// Ask for a line. Returns `false` when the queue is saturated.
    pub fn ask(&mut self, prompt: DialogPrompt, now: Tick) -> bool {
        let accepted = self.queue.submit(prompt, now);
        if accepted && let Some(provider) = self.provider.as_mut() {
            provider.submit(prompt);
        }
        accepted
    }

    /// The replies produced last tick.
    #[must_use]
    pub fn last(&self) -> Option<&[DialogReply]> {
        self.last.as_deref()
    }
}

impl Default for DialogHub {
    fn default() -> Self {
        let table = BarkTable::starter().unwrap_or_default();
        Self::new(TemplateProvider::new(table, 0x00D1_A106))
    }
}

/// Poll the provider and publish whatever came back.
pub fn tick_dialog(
    clock: Res<SimClock>,
    mut hub: ResMut<DialogHub>,
    mut outbox: ResMut<DialogOutbox>,
) {
    let hub = &mut *hub;
    let provider: &mut dyn DialogProvider = match hub.provider.as_mut() {
        Some(provider) => provider.as_mut(),
        None => &mut hub.silent,
    };
    let replies = hub.queue.tick(provider, &mut hub.fallback, clock.tick);
    if replies.is_empty() {
        hub.last = None;
        return;
    }
    let mut kept = Vec::with_capacity(replies.len());
    for reply in replies {
        outbox.lines.push_back(SpokenLine {
            npc: reply.npc,
            text: reply.text.clone(),
            at: reply.at,
            source: reply.source,
        });
        kept.push(reply);
    }
    hub.last = Some(kept);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::behavior::bark::BarkCategory;
    use crate::behavior::provider::DialogContext;
    use crate::testkit::OrDie;
    use bevy_app::{App, Update};

    fn app() -> App {
        let mut app = App::new();
        app.insert_resource(SimClock::default())
            .insert_resource(Barks::starter().or_die())
            .init_resource::<DialogOutbox>()
            .insert_resource(DialogHub::default())
            .add_systems(Update, (produce_barks, tick_dialog));
        app
    }

    #[test]
    fn a_request_becomes_a_bark_with_its_text_available() {
        let mut app = app();
        app.world_mut()
            .spawn((Npc(NpcId::unique(1)), NpcBarks::default()));
        {
            let mut barks = app.world_mut().resource_mut::<Barks>();
            barks.request(BarkRequest {
                npc: NpcId::unique(1),
                category: BarkCategory::Greeting,
                at: Tick::ZERO,
            });
        }
        app.update();
        let barks = app.world().resource::<Barks>();
        assert_eq!(barks.queue().len(), 1);
        let event = barks.queue().events().front().copied().or_die();
        assert_eq!(event.npc, NpcId::unique(1));
        assert!(barks.table().text(event.line).is_some());
    }

    #[test]
    fn the_cooldown_suppresses_a_second_bark() {
        let mut app = app();
        app.world_mut()
            .spawn((Npc(NpcId::unique(1)), NpcBarks::default()));
        for _ in 0..2 {
            let mut barks = app.world_mut().resource_mut::<Barks>();
            barks.request(BarkRequest {
                npc: NpcId::unique(1),
                category: BarkCategory::Greeting,
                at: Tick::ZERO,
            });
            app.update();
        }
        assert_eq!(app.world().resource::<Barks>().queue().len(), 1);
    }

    #[test]
    fn the_per_tick_cap_is_respected() {
        let mut app = app();
        {
            let mut barks = app.world_mut().resource_mut::<Barks>();
            barks.set_max_per_tick(2);
        }
        for index in 0..6_u32 {
            app.world_mut()
                .spawn((Npc(NpcId::unique(index)), NpcBarks::default()));
            let mut barks = app.world_mut().resource_mut::<Barks>();
            barks.request(BarkRequest {
                npc: NpcId::unique(index),
                category: BarkCategory::Idle,
                at: Tick::ZERO,
            });
        }
        app.update();
        let barks = app.world().resource::<Barks>();
        assert_eq!(barks.queue().len(), 2);
        assert_eq!(barks.pending(), 4);
    }

    #[test]
    fn a_dead_npc_does_not_speak() {
        let mut app = app();
        {
            let mut barks = app.world_mut().resource_mut::<Barks>();
            barks.request(BarkRequest {
                npc: NpcId::unique(77),
                category: BarkCategory::Alarm,
                at: Tick::ZERO,
            });
        }
        app.update();
        assert!(app.world().resource::<Barks>().queue().is_empty());
    }

    #[test]
    fn dialog_falls_back_when_no_provider_is_attached() {
        let mut app = app();
        {
            let mut hub = app.world_mut().resource_mut::<DialogHub>();
            assert!(!hub.has_provider());
            let accepted = hub.ask(
                DialogPrompt {
                    npc: NpcId::unique(3),
                    category: BarkCategory::Trade,
                    deadline: Tick::ZERO,
                    context: DialogContext::default(),
                },
                Tick::ZERO,
            );
            assert!(accepted);
        }
        app.update();
        let outbox = app.world().resource::<DialogOutbox>();
        assert_eq!(outbox.len(), 1);
        let line = outbox.lines().front().cloned().or_die();
        assert_eq!(line.source, ReplySource::Fallback);
        assert_eq!(line.npc, NpcId::unique(3));
        assert!(!line.text.is_empty());
    }

    #[test]
    fn a_saturated_queue_refuses_new_prompts() {
        let mut app = app();
        let mut hub = app.world_mut().resource_mut::<DialogHub>();
        for index in 0..(crate::behavior::provider::MAX_INFLIGHT as u32 + 4) {
            hub.ask(
                DialogPrompt {
                    npc: NpcId::unique(index),
                    category: BarkCategory::Idle,
                    deadline: Tick::new(10_000),
                    context: DialogContext::default(),
                },
                Tick::ZERO,
            );
        }
        assert!(hub.queue().refused() > 0);
    }
}
