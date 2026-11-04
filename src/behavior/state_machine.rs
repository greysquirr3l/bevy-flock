//! A guarded state machine for NPC modes.
//!
//! Behaviour trees answer "what should I do next"; state machines answer "what
//! mode am I in, and what would change it". Real NPCs need both — a guard cannot
//! abandon a patrol because a butterfly distracted it — so this is a small,
//! deterministic machine with dwell times and ordered guards rather than a
//! general-purpose graph.

use core::fmt;

use crate::behavior::blackboard::Blackboard;
use crate::domain::tick::Tick;

/// A guard: a predicate over working memory.
pub type Guard = Box<dyn Fn(&Blackboard) -> bool + Send + Sync>;

/// One allowed transition.
pub struct Transition<S> {
    from: S,
    to: S,
    min_dwell: u64,
    guard: Guard,
    priority: u8,
}

impl<S> fmt::Debug for Transition<S>
where
    S: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Transition")
            .field("from", &self.from)
            .field("to", &self.to)
            .field("min_dwell", &self.min_dwell)
            .field("priority", &self.priority)
            .finish_non_exhaustive()
    }
}

/// A state machine over a game-defined state enum.
#[derive(Debug)]
pub struct StateMachine<S> {
    current: S,
    entered: Tick,
    transitions: Vec<Transition<S>>,
}

impl<S> StateMachine<S>
where
    S: Copy + PartialEq + fmt::Debug,
{
    /// Start in `initial`.
    #[must_use]
    pub fn new(initial: S, at: Tick) -> Self {
        Self {
            current: initial,
            entered: at,
            transitions: Vec::new(),
        }
    }

    /// Allow `from -> to` whenever `guard` holds.
    #[must_use]
    pub fn transition(mut self, from: S, to: S, guard: Guard) -> Self {
        let priority = self.transitions.len() as u8;
        self.transitions.push(Transition {
            from,
            to,
            min_dwell: 0,
            guard,
            priority,
        });
        self
    }

    /// Allow `from -> to` once the machine has dwelt in `from` for `ticks`.
    #[must_use]
    pub fn transition_after(mut self, from: S, to: S, ticks: u64, guard: Guard) -> Self {
        let priority = self.transitions.len() as u8;
        self.transitions.push(Transition {
            from,
            to,
            min_dwell: ticks,
            guard,
            priority,
        });
        self
    }

    /// The current state.
    #[must_use]
    pub const fn current(&self) -> S {
        self.current
    }

    /// The tick the current state was entered on.
    #[must_use]
    pub const fn entered(&self) -> Tick {
        self.entered
    }

    /// How long the machine has been in the current state.
    #[must_use]
    pub const fn dwell(&self, now: Tick) -> u64 {
        now.since(self.entered)
    }

    /// Evaluate guards and take at most one transition.
    ///
    /// Transitions are tried in registration order, so priority is explicit and
    /// replayable rather than depending on a hash or a sort.
    pub fn tick(&mut self, now: Tick, guard_context: &Blackboard) -> Option<(S, S)> {
        let dwell = self.dwell(now);
        for transition in &self.transitions {
            if transition.from != self.current {
                continue;
            }
            if dwell < transition.min_dwell {
                continue;
            }
            if !(transition.guard)(guard_context) {
                continue;
            }
            let from = self.current;
            self.current = transition.to;
            self.entered = now;
            return Some((from, transition.to));
        }
        None
    }

    /// Force a state, resetting the dwell timer.
    ///
    /// Used by group directives, which outrank guards: a leader shouting "take
    /// cover" does not wait for a predicate to agree.
    pub fn force(&mut self, state: S, now: Tick) {
        self.current = state;
        self.entered = now;
    }

    /// How many transitions are registered.
    #[must_use]
    pub fn transition_count(&self) -> usize {
        self.transitions.len()
    }
}

/// A guard that always passes.
pub fn always() -> Guard {
    Box::new(|_| true)
}

/// A guard that never passes.
pub fn never() -> Guard {
    Box::new(|_| false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::behavior::blackboard::{BlackboardKey, Value};

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Mode {
        Patrol,
        Chase,
        Flee,
    }

    const ALARM: BlackboardKey = BlackboardKey::new(1);

    fn alarm_guard() -> Guard {
        Box::new(|board: &Blackboard| board.get(ALARM).and_then(Value::as_bool).unwrap_or(false))
    }

    #[test]
    fn takes_a_transition_when_the_guard_passes() {
        let mut machine = StateMachine::new(Mode::Patrol, Tick::ZERO).transition(
            Mode::Patrol,
            Mode::Chase,
            alarm_guard(),
        );
        let mut board = Blackboard::new();
        board.set(ALARM, Value::Bool(true));
        let change = machine.tick(Tick::new(1), &board);
        assert_eq!(change, Some((Mode::Patrol, Mode::Chase)));
        assert_eq!(machine.current(), Mode::Chase);
    }

    #[test]
    fn does_nothing_when_the_guard_fails() {
        let mut machine = StateMachine::new(Mode::Patrol, Tick::ZERO).transition(
            Mode::Patrol,
            Mode::Chase,
            alarm_guard(),
        );
        let board = Blackboard::new();
        assert_eq!(machine.tick(Tick::new(1), &board), None);
        assert_eq!(machine.current(), Mode::Patrol);
    }

    #[test]
    fn dwell_time_blocks_until_elapsed() {
        let mut machine = StateMachine::new(Mode::Patrol, Tick::ZERO).transition_after(
            Mode::Patrol,
            Mode::Chase,
            10,
            always(),
        );
        let board = Blackboard::new();
        assert_eq!(machine.tick(Tick::new(5), &board), None);
        assert_eq!(
            machine.tick(Tick::new(10), &board),
            Some((Mode::Patrol, Mode::Chase))
        );
    }

    #[test]
    fn earlier_registration_wins_when_two_guards_pass() {
        let mut machine = StateMachine::new(Mode::Patrol, Tick::ZERO)
            .transition(Mode::Patrol, Mode::Chase, always())
            .transition(Mode::Patrol, Mode::Flee, always());
        let board = Blackboard::new();
        assert_eq!(
            machine.tick(Tick::new(1), &board),
            Some((Mode::Patrol, Mode::Chase))
        );
    }

    #[test]
    fn transitions_from_other_states_are_ignored() {
        let mut machine = StateMachine::new(Mode::Flee, Tick::ZERO).transition(
            Mode::Patrol,
            Mode::Chase,
            always(),
        );
        let board = Blackboard::new();
        assert_eq!(machine.tick(Tick::new(1), &board), None);
    }

    #[test]
    fn force_overrides_guards_and_resets_dwell() {
        let mut machine = StateMachine::new(Mode::Patrol, Tick::ZERO).transition_after(
            Mode::Patrol,
            Mode::Chase,
            100,
            never(),
        );
        machine.force(Mode::Flee, Tick::new(7));
        assert_eq!(machine.current(), Mode::Flee);
        assert_eq!(machine.dwell(Tick::new(9)), 2);
    }

    #[test]
    fn dwell_saturates_when_time_goes_backwards() {
        let machine: StateMachine<Mode> = StateMachine::new(Mode::Patrol, Tick::new(10));
        assert_eq!(machine.dwell(Tick::new(4)), 0);
    }
}
