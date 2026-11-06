//! Dialog that does not come from a table: the provider seam.
//!
//! An LLM, a remote service, a local model, a designer's test double — all of them
//! are the same thing to the simulation: **a source of text that might not answer
//! before the player stops caring**. The library cannot block a frame waiting for
//! one, and it cannot assume one exists.
//!
//! So the seam is a queue with a deadline:
//!
//! 1. The simulation submits a [`DialogPrompt`] with a tick by which it expects an
//!    answer.
//! 2. Every frame, [`ProviderQueue::tick`] polls the provider and drains whatever
//!    came back.
//! 3. Anything still in flight past its deadline is answered from the fallback
//!    table instead, and the substitution is counted.
//!
//! That is the whole contract, and it is deliberately synchronous: the provider
//! decides whether it is asynchronous internally (a worker thread, a channel to a
//! service) and the simulation never learns. Requiring an async runtime would have
//! made this crate's runtime decision for every game that uses it.

use crate::behavior::bark::{BarkCategory, BarkTable};
use crate::domain::ids::NpcId;
use crate::domain::rng::RngStream;
use crate::domain::tick::Tick;
use smallvec::SmallVec;
use std::collections::VecDeque;

/// The most prompts that may be in flight before new ones are refused.
pub const MAX_INFLIGHT: usize = 32;

/// What the simulation tells a provider about the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DialogContext {
    /// The topic, as a game-defined id. Zero means "no particular topic".
    pub topic: u16,
    /// Whether the NPC has met this player before.
    pub met_before: bool,
    /// Whether the NPC is currently alarmed.
    pub alarmed: bool,
}

/// A request for a line of dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DialogPrompt {
    /// The NPC who is speaking.
    pub npc: NpcId,
    /// The kind of thing to say, so a fallback has something to work with.
    pub category: BarkCategory,
    /// The tick by which an answer is needed.
    pub deadline: Tick,
    /// What the provider may use to shape the answer.
    pub context: DialogContext,
}

/// Where a line came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplySource {
    /// A configured provider answered in time.
    Provider,
    /// The deadline passed and the fallback table answered.
    Fallback,
    /// The provider answered so late nobody wanted it; the fallback had already
    /// spoken and this reply was discarded.
    Late,
}

/// A line of dialog, ready to display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialogReply {
    /// The NPC who said it.
    pub npc: NpcId,
    /// The text.
    pub text: String,
    /// The tick it was produced on.
    pub at: Tick,
    /// Where it came from.
    pub source: ReplySource,
}

/// Something that can answer a dialog prompt.
///
/// Object-safe and synchronous. A provider that talks to a service owns a worker
/// and a channel; [`DialogProvider::poll`] drains the channel and returns what has
/// arrived, which keeps the call cheap and the frame unblocked.
pub trait DialogProvider: Send + Sync {
    /// Accept a prompt. A provider may ignore it, in which case the deadline
    /// fallback speaks instead.
    fn submit(&mut self, prompt: DialogPrompt);

    /// Drain any completed replies into `out`, returning how many were appended.
    ///
    /// Must not block. A provider that cannot answer yet returns zero.
    fn poll(&mut self, now: Tick, out: &mut VecDeque<DialogReply>) -> usize;

    /// A short label for diagnostics.
    fn label(&self) -> &'static str;
}

/// A provider that answers from a bark table, for tests and for offline builds.
///
/// Deterministic: the reply depends on the prompt and the provider's own stream, so
/// a replay says the same thing.
#[derive(Debug, Clone)]
pub struct TemplateProvider {
    table: BarkTable,
    rng: RngStream,
    latency: u32,
    submitted: u64,
}

impl TemplateProvider {
    /// A provider that answers immediately from a table.
    #[must_use]
    pub const fn new(table: BarkTable, seed: u64) -> Self {
        Self {
            table,
            rng: RngStream::new(crate::domain::ids::SimSeed::new(seed), 0xBA2C),
            latency: 0,
            submitted: 0,
        }
    }

    /// Answer only after this many ticks, to exercise the deadline path.
    #[must_use]
    pub const fn with_latency(mut self, ticks: u32) -> Self {
        self.latency = ticks;
        self
    }

    /// How many prompts the provider has accepted.
    #[must_use]
    pub const fn submitted(&self) -> u64 {
        self.submitted
    }
}

impl DialogProvider for TemplateProvider {
    fn submit(&mut self, _prompt: DialogPrompt) {
        self.submitted = self.submitted.saturating_add(1);
    }

    fn poll(&mut self, now: Tick, out: &mut VecDeque<DialogReply>) -> usize {
        let _ = now;
        let _ = out;
        // The queue owns the timing: this provider answers nothing on its own. It
        // exists so the fallback path has a deterministic, table-driven answer.
        0
    }

    fn label(&self) -> &'static str {
        "template"
    }
}

impl TemplateProvider {
    /// Build the reply for a prompt from the table.
    fn answer(&mut self, prompt: DialogPrompt, now: Tick) -> Option<DialogReply> {
        let line = self.table.pick(prompt.category, None, &mut self.rng)?;
        let text = self.table.text(line)?.to_owned();
        Some(DialogReply {
            npc: prompt.npc,
            text,
            at: now,
            source: ReplySource::Provider,
        })
    }

    /// The artificial latency, in ticks.
    #[must_use]
    pub const fn latency(&self) -> u32 {
        self.latency
    }
}

/// A prompt and the tick it was submitted on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct InFlight {
    prompt: DialogPrompt,
    submitted: Tick,
}

/// The queue that keeps dialog off the critical path.
#[derive(Debug, Clone, Default)]
pub struct ProviderQueue {
    inflight: SmallVec<[InFlight; MAX_INFLIGHT]>,
    replies: VecDeque<DialogReply>,
    submitted: u64,
    answered: u64,
    fell_back: u64,
    discarded_late: u64,
    refused: u64,
}

impl ProviderQueue {
    /// An empty queue.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Prompts currently awaiting an answer.
    #[must_use]
    pub fn inflight(&self) -> usize {
        self.inflight.len()
    }

    /// How many prompts were accepted.
    #[must_use]
    pub const fn submitted(&self) -> u64 {
        self.submitted
    }

    /// How many were answered by a provider.
    #[must_use]
    pub const fn answered(&self) -> u64 {
        self.answered
    }

    /// How many fell back because the deadline passed.
    #[must_use]
    pub const fn fell_back(&self) -> u64 {
        self.fell_back
    }

    /// How many answers arrived after the fallback had already spoken.
    #[must_use]
    pub const fn discarded_late(&self) -> u64 {
        self.discarded_late
    }

    /// How many prompts were refused because too many were in flight.
    #[must_use]
    pub const fn refused(&self) -> u64 {
        self.refused
    }

    /// Submit a prompt.
    ///
    /// Returns `false` when the queue is already at [`MAX_INFLIGHT`]; the caller
    /// should treat that as "this NPC does not get to talk right now", which is a
    /// better outcome than unbounded latency or an allocation spike.
    pub fn submit(&mut self, prompt: DialogPrompt, now: Tick) -> bool {
        if self.inflight.len() >= MAX_INFLIGHT {
            self.refused = self.refused.saturating_add(1);
            return false;
        }
        self.inflight.push(InFlight {
            prompt,
            submitted: now,
        });
        self.submitted = self.submitted.saturating_add(1);
        true
    }

    /// Cancel a prompt, for example because its NPC despawned.
    ///
    /// Returns whether anything was cancelled.
    pub fn cancel(&mut self, npc: NpcId) -> bool {
        let before = self.inflight.len();
        self.inflight.retain(|entry| entry.prompt.npc != npc);
        before != self.inflight.len()
    }

    /// Advance: poll the provider, then expire anything past its deadline.
    ///
    /// Returns the replies produced this call, which the caller drains into
    /// whatever it uses for display.
    pub fn tick(
        &mut self,
        provider: &mut (impl DialogProvider + ?Sized),
        fallback: &mut TemplateProvider,
        now: Tick,
    ) -> SmallVec<[DialogReply; 4]> {
        let mut produced: SmallVec<[DialogReply; 4]> = SmallVec::new();
        let mut fresh = VecDeque::new();
        let _ = provider.poll(now, &mut fresh);
        for reply in fresh {
            // A reply for a prompt that already fell back is discarded rather than
            // shown twice.
            let still_wanted = self
                .inflight
                .iter()
                .any(|entry| entry.prompt.npc == reply.npc);
            if !still_wanted {
                self.discarded_late = self.discarded_late.saturating_add(1);
                continue;
            }
            self.inflight.retain(|entry| entry.prompt.npc != reply.npc);
            self.answered = self.answered.saturating_add(1);
            produced.push(reply);
        }
        let mut expired: SmallVec<[DialogPrompt; 4]> = SmallVec::new();
        for entry in &self.inflight {
            if now >= entry.prompt.deadline {
                expired.push(entry.prompt);
            }
        }
        if !expired.is_empty() {
            self.inflight.retain(|entry| now < entry.prompt.deadline);
            for prompt in expired {
                if let Some(reply) = fallback.answer(prompt, now) {
                    self.fell_back = self.fell_back.saturating_add(1);
                    produced.push(DialogReply {
                        source: ReplySource::Fallback,
                        ..reply
                    });
                }
            }
        }
        // Published for `drain`, and returned so a caller can react within the frame.
        for reply in &produced {
            self.replies.push_back(reply.clone());
        }
        produced
    }

    /// Take every finished reply, oldest first.
    pub fn drain(&mut self) -> impl Iterator<Item = DialogReply> + '_ {
        self.replies.drain(..)
    }

    /// How many finished replies are waiting to be read.
    #[must_use]
    pub fn ready(&self) -> usize {
        self.replies.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::num;
    use crate::testkit::{GetOrDieRef, OrDie};

    /// A provider that answers after a fixed number of polls.
    struct DelayedProvider {
        after: u32,
        polls: u32,
        queued: VecDeque<DialogPrompt>,
    }

    impl DelayedProvider {
        fn new(after: u32) -> Self {
            Self {
                after,
                polls: 0,
                queued: VecDeque::new(),
            }
        }
    }

    impl DialogProvider for DelayedProvider {
        fn submit(&mut self, prompt: DialogPrompt) {
            self.queued.push_back(prompt);
        }

        fn poll(&mut self, now: Tick, out: &mut VecDeque<DialogReply>) -> usize {
            self.polls = self.polls.saturating_add(1);
            if self.polls < self.after {
                return 0;
            }
            let mut count = 0;
            while let Some(prompt) = self.queued.pop_front() {
                out.push_back(DialogReply {
                    npc: prompt.npc,
                    text: "from the provider".to_owned(),
                    at: now,
                    source: ReplySource::Provider,
                });
                count += 1;
            }
            count
        }

        fn label(&self) -> &'static str {
            "delayed"
        }
    }

    fn fallback() -> TemplateProvider {
        TemplateProvider::new(BarkTable::starter().or_die(), 7)
    }

    fn prompt(npc: u32, _at: u64, deadline: u64) -> DialogPrompt {
        DialogPrompt {
            npc: NpcId::unique(npc),
            category: BarkCategory::Greeting,
            deadline: Tick::new(deadline),
            context: DialogContext::default(),
        }
    }

    #[test]
    fn a_provider_that_answers_in_time_is_used() {
        let mut queue = ProviderQueue::new();
        let mut provider = DelayedProvider::new(1);
        let mut fallback = fallback();
        assert!(queue.submit(prompt(1, 0, 30), Tick::ZERO));
        provider.submit(prompt(1, 0, 30));
        let replies = queue.tick(&mut provider, &mut fallback, Tick::new(1));
        assert_eq!(replies.len(), 1);
        assert_eq!(replies.at_ref(0).source, ReplySource::Provider);
        assert_eq!(queue.answered(), 1);
        assert_eq!(queue.fell_back(), 0);
    }

    #[test]
    fn a_provider_that_misses_its_deadline_falls_back() {
        let mut queue = ProviderQueue::new();
        let mut provider = DelayedProvider::new(100);
        let mut fallback = fallback();
        assert!(queue.submit(prompt(1, 0, 5), Tick::ZERO));
        provider.submit(prompt(1, 0, 5));
        // Before the deadline nothing is produced.
        assert!(
            queue
                .tick(&mut provider, &mut fallback, Tick::new(2))
                .is_empty()
        );
        // At the deadline the fallback speaks.
        let replies = queue.tick(&mut provider, &mut fallback, Tick::new(5));
        assert_eq!(replies.len(), 1);
        assert_eq!(replies.at_ref(0).source, ReplySource::Fallback);
        assert!(!replies.at_ref(0).text.is_empty());
        assert_eq!(queue.fell_back(), 1);
        assert_eq!(queue.inflight(), 0);
    }

    #[test]
    fn a_late_answer_is_discarded_rather_than_shown_twice() {
        let mut queue = ProviderQueue::new();
        // Answers on its second poll, which is one tick after the deadline.
        let mut provider = DelayedProvider::new(2);
        let mut fallback = fallback();
        assert!(queue.submit(prompt(1, 0, 2), Tick::ZERO));
        provider.submit(prompt(1, 0, 2));
        let _ = queue.tick(&mut provider, &mut fallback, Tick::new(2));
        assert_eq!(queue.fell_back(), 1);
        let late = queue.tick(&mut provider, &mut fallback, Tick::new(3));
        assert!(late.is_empty(), "the late answer was shown anyway");
        assert_eq!(queue.discarded_late(), 1);
    }

    #[test]
    fn the_queue_refuses_more_than_it_can_track() {
        let mut queue = ProviderQueue::new();
        for index in 0..MAX_INFLIGHT {
            assert!(queue.submit(prompt(num::usize_to_u32(index), 0, 100), Tick::ZERO));
        }
        assert!(!queue.submit(prompt(999, 0, 100), Tick::ZERO));
        assert_eq!(queue.refused(), 1);
        assert_eq!(queue.inflight(), MAX_INFLIGHT);
    }

    #[test]
    fn a_despawned_npc_stops_waiting() {
        let mut queue = ProviderQueue::new();
        assert!(queue.submit(prompt(1, 0, 100), Tick::ZERO));
        assert!(queue.cancel(NpcId::unique(1)));
        assert_eq!(queue.inflight(), 0);
        assert!(!queue.cancel(NpcId::unique(1)));
    }

    #[test]
    fn the_fallback_is_deterministic_for_a_seed() {
        let reply = |seed: u64| {
            let mut queue = ProviderQueue::new();
            let mut provider = DelayedProvider::new(100);
            let mut fallback = fallback_with(seed);
            assert!(queue.submit(prompt(4, 0, 1), Tick::ZERO));
            let replies = queue.tick(&mut provider, &mut fallback, Tick::new(1));
            replies.at_ref(0).text.clone()
        };
        assert_eq!(reply(11), reply(11));
    }

    fn fallback_with(seed: u64) -> TemplateProvider {
        TemplateProvider::new(BarkTable::starter().or_die(), seed)
    }

    #[test]
    fn replies_are_readable_after_the_tick() {
        let mut queue = ProviderQueue::new();
        let mut provider = DelayedProvider::new(100);
        let mut fallback = fallback();
        assert!(queue.submit(prompt(1, 0, 1), Tick::ZERO));
        let _ = queue.tick(&mut provider, &mut fallback, Tick::new(1));
        assert_eq!(queue.ready(), 1);
        assert_eq!(queue.drain().count(), 1);
        assert_eq!(queue.ready(), 0);
    }
}
