//! Utility scoring: pick a behaviour by weighing what an NPC cares about.
//!
//! A behaviour tree says *how* to do something; utility scoring says *what* is
//! worth doing. A guard with a grudge and an empty stomach should choose between
//! the two on the merits, and a designer should be able to read the arithmetic
//! afterwards.
//!
//! ## The arithmetic
//!
//! Each candidate is a [`UtilitySet`]: a list of [`Consideration`]s, each of which
//! reads one input from the blackboard, bends it through a [`Curve`], and weights
//! it. The considerations multiply:
//!
//! ```text
//! raw   = Π (curve(input_i) * weight_i)
//! score = raw ^ (1 - 1/n)        // n = number of considerations
//! ```
//!
//! Multiplication rather than averaging, because a consideration is a gate: if the
//! door is locked (0.0), the behaviour is impossible, and no amount of wanting to
//! go through it should average the score back up. The exponent is the standard
//! compensation for multiplying many fractions — without it a five-consideration
//! behaviour scores far below a two-consideration one regardless of content.
//!
//! ## Inhibition
//!
//! Fear does not compete with conversation; it cancels it. An [`Inhibition`] names
//! a target and the condition under which the target scores zero, which is how
//! believable social behaviour is expressed without a table of weights tuned
//! against each other.
//!
//! ## Explainability
//!
//! [`UtilityTable::explain`] returns every candidate, its raw product, its
//! compensated score, each consideration's contribution, and whether an inhibitor
//! silenced it. That is what makes "why did this NPC do that" answerable from the
//! output alone, which is a requirement rather than a nicety.

use crate::behavior::blackboard::{Blackboard, BlackboardKey, Value};
use smallvec::SmallVec;
use std::fmt::Write as _;

/// The most considerations one utility set may hold.
pub const MAX_CONSIDERATIONS: usize = 8;

/// A candidate behaviour's identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct BehaviorId(u16);

impl BehaviorId {
    /// A behaviour identifier.
    #[must_use]
    pub const fn new(raw: u16) -> Self {
        Self(raw)
    }

    /// The raw identifier.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

/// Where a consideration's input comes from.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Input {
    /// Read a blackboard slot. Floats are used as they are, integers are cast,
    /// booleans read as 1.0 or 0.0, and anything else reads as 0.0 — a position is
    /// not a score.
    Key(BlackboardKey),
    /// A fixed value, for a consideration that is a constant preference.
    Constant(f32),
}

impl Input {
    /// Read the input from a blackboard.
    #[must_use]
    pub fn read(self, blackboard: &Blackboard) -> f32 {
        match self {
            Self::Constant(value) => value,
            Self::Key(key) => match blackboard.get(key) {
                Some(Value::Float(value)) => value,
                Some(Value::Int(value)) => value as f32,
                Some(Value::Bool(value)) => f32::from(u8::from(value)),
                _ => 0.0,
            },
        }
    }

    /// A short description for the explain output.
    #[must_use]
    pub fn label(self) -> String {
        match self {
            Self::Constant(value) => format!("constant {value:.2}"),
            Self::Key(key) => format!("key {}", key.get()),
        }
    }
}

/// How an input is bent into a 0..=1 score.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Curve {
    /// A straight line: `slope * x + intercept`, clamped.
    Linear {
        /// How much the input counts.
        slope: f32,
        /// Where the line starts.
        intercept: f32,
    },
    /// `x ^ exponent`: convex for exponents above 1, concave below.
    Quadratic {
        /// The exponent.
        exponent: f32,
    },
    /// `1 / (1 + falloff * x)`: high for small inputs, decaying.
    Inverse {
        /// How fast it decays.
        falloff: f32,
    },
    /// A smooth S: useful for "nothing until it matters, then all at once".
    Logistic {
        /// How sharply it rises.
        steepness: f32,
        /// The input at which it is 0.5.
        midpoint: f32,
    },
    /// Nothing below the threshold, everything at or above it.
    Step {
        /// The threshold.
        threshold: f32,
    },
    /// A designer-shaped response: piecewise-linear through the given points.
    Piecewise(SmallVec<[(f32, f32); 4]>),
}

impl Curve {
    /// All the simple curves, for tests and for a designer's tooling.
    pub const SIMPLE: [Self; 5] = [
        Self::Linear {
            slope: 1.0,
            intercept: 0.0,
        },
        Self::Quadratic { exponent: 2.0 },
        Self::Inverse { falloff: 1.0 },
        Self::Logistic {
            steepness: 8.0,
            midpoint: 0.5,
        },
        Self::Step { threshold: 0.5 },
    ];

    /// Evaluate the curve, clamped to `0..=1`.
    ///
    /// NaN reads as 0: a consideration that cannot be evaluated must not vote, and
    /// a NaN score would poison the whole product.
    #[must_use]
    pub fn evaluate(&self, input: f32) -> f32 {
        let raw = match self {
            Self::Linear { slope, intercept } => *slope * input + *intercept,
            Self::Quadratic { exponent } => input.max(0.0).powf(*exponent),
            Self::Inverse { falloff } => 1.0 / (1.0 + *falloff * input.max(0.0)),
            Self::Logistic {
                steepness,
                midpoint,
            } => 1.0 / (1.0 + (-*steepness * (input - *midpoint)).exp()),
            Self::Step { threshold } => {
                if input >= *threshold {
                    1.0
                } else {
                    0.0
                }
            }
            Self::Piecewise(points) => Self::piecewise(points, input),
        };
        if raw.is_nan() {
            0.0
        } else {
            raw.clamp(0.0, 1.0)
        }
    }

    fn piecewise(points: &[(f32, f32)], input: f32) -> f32 {
        let Some(&(first_x, first_y)) = points.first() else {
            return 0.0;
        };
        if input <= first_x {
            return first_y;
        }
        for pair in points.windows(2) {
            let (Some(&(x0, y0)), Some(&(x1, y1))) = (pair.first(), pair.get(1)) else {
                continue;
            };
            if input <= x1 {
                let span = x1 - x0;
                if span <= f32::EPSILON {
                    return y1;
                }
                let t = (input - x0) / span;
                return y0 + (y1 - y0) * t;
            }
        }
        points.last().map_or(0.0, |(_, y)| *y)
    }
}

impl Default for Curve {
    fn default() -> Self {
        Self::Linear {
            slope: 1.0,
            intercept: 0.0,
        }
    }
}

/// One input, bent and weighted.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Consideration {
    /// What it reads.
    pub input: Input,
    /// How the input is bent.
    pub curve: Curve,
    /// How much it counts, in `0..=1`. A weight below one damps the consideration;
    /// a weight of zero makes the behaviour impossible, which is how a hard
    /// requirement is written.
    pub weight: f32,
}

impl Consideration {
    /// A consideration with a linear curve and full weight.
    #[must_use]
    pub const fn keyed(key: BlackboardKey) -> Self {
        Self {
            input: Input::Key(key),
            curve: Curve::Linear {
                slope: 1.0,
                intercept: 0.0,
            },
            weight: 1.0,
        }
    }

    /// Set the curve.
    #[must_use]
    pub fn with_curve(mut self, curve: Curve) -> Self {
        self.curve = curve;
        self
    }

    /// Set the weight.
    #[must_use]
    pub const fn with_weight(mut self, weight: f32) -> Self {
        self.weight = weight;
        self
    }

    /// The contribution this consideration makes.
    #[must_use]
    pub fn evaluate(&self, blackboard: &Blackboard) -> f32 {
        let weight = if self.weight.is_finite() {
            self.weight.clamp(0.0, 1.0)
        } else {
            0.0
        };
        self.curve.evaluate(self.input.read(blackboard)) * weight
    }
}

/// A behaviour's considerations, and whether to compensate for their count.
#[derive(Debug, Clone, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct UtilitySet {
    /// The considerations, in evaluation order.
    pub considerations: SmallVec<[Consideration; MAX_CONSIDERATIONS]>,
    /// Whether to apply the `1 - 1/n` compensation exponent.
    pub compensation: bool,
}

impl UtilitySet {
    /// An empty set, which scores 1.0 — a behaviour that always wants to happen.
    #[must_use]
    pub fn new() -> Self {
        Self {
            considerations: SmallVec::new(),
            compensation: true,
        }
    }

    /// Add a consideration.
    ///
    /// # Errors
    ///
    /// [`UtilityError::TooManyConsiderations`] beyond [`MAX_CONSIDERATIONS`].
    pub fn push(&mut self, consideration: Consideration) -> Result<(), UtilityError> {
        if self.considerations.len() >= MAX_CONSIDERATIONS {
            return Err(UtilityError::TooManyConsiderations {
                limit: MAX_CONSIDERATIONS,
            });
        }
        self.considerations.push(consideration);
        Ok(())
    }

    /// The compensated score.
    #[must_use]
    pub fn score(&self, blackboard: &Blackboard) -> f32 {
        self.breakdown(blackboard).1
    }

    /// The raw product and the compensated score.
    #[must_use]
    pub fn breakdown(&self, blackboard: &Blackboard) -> (f32, f32) {
        if self.considerations.is_empty() {
            return (1.0, 1.0);
        }
        let mut raw = 1.0_f32;
        for consideration in &self.considerations {
            raw *= consideration.evaluate(blackboard);
        }
        if !self.compensation {
            return (raw, raw);
        }
        let count = self.considerations.len() as f32;
        // With one consideration the exponent is zero, and `x^0` is 1.0 — which would
        // turn every single-consideration behaviour into an unconditional winner. The
        // compensation is the identity below two considerations, so it is applied only
        // where it means something.
        if count < 2.0 {
            return (raw, raw);
        }
        let exponent = 1.0 - 1.0 / count;
        (raw, raw.max(0.0).powf(exponent))
    }
}

/// A rule that silences a behaviour when a condition holds.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Inhibition {
    /// The behaviour to silence.
    pub target: BehaviorId,
    /// The condition: the input is read and compared against the threshold.
    pub when: Input,
    /// The input at or above which the target is silenced.
    pub threshold: f32,
}

impl Inhibition {
    /// Whether this inhibitor is active.
    #[must_use]
    pub fn is_active(self, blackboard: &Blackboard) -> bool {
        self.when.read(blackboard) >= self.threshold
    }
}

/// A candidate behaviour and the weights that recommend it.
#[derive(Debug, Clone, PartialEq)]
pub struct UtilityEntry {
    /// The identity used in traces and inhibitions.
    pub id: BehaviorId,
    /// A label a human reads.
    pub label: &'static str,
    /// The considerations.
    pub set: UtilitySet,
}

impl UtilityEntry {
    /// An entry.
    #[must_use]
    pub fn new(id: BehaviorId, label: &'static str, set: UtilitySet) -> Self {
        Self { id, label, set }
    }
}

/// What won, and by how much.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Selection {
    /// The winner.
    pub id: BehaviorId,
    /// Its score.
    pub score: f32,
    /// The runner-up and its score, if there was one.
    pub runner_up: Option<(BehaviorId, f32)>,
}

/// One candidate's row in an explanation.
#[derive(Debug, Clone)]
pub struct CandidateScore {
    /// The candidate.
    pub id: BehaviorId,
    /// Its label.
    pub label: &'static str,
    /// The raw product before compensation.
    pub raw: f32,
    /// The score that competed.
    pub score: f32,
    /// Whether an inhibitor silenced it.
    pub inhibited: bool,
    /// Each consideration's contribution, with its weight, for the trace.
    pub contributions: SmallVec<[(f32, f32); MAX_CONSIDERATIONS]>,
}

/// Everything needed to answer "why did this NPC do that".
#[derive(Debug, Clone)]
pub struct Explanation {
    /// Every candidate, in registration order.
    pub candidates: SmallVec<[CandidateScore; 8]>,
    /// The winner, if any candidate survived inhibition.
    pub winner: Option<Selection>,
}

impl Explanation {
    /// A paste-ready account of the decision.
    #[must_use]
    pub fn text(&self) -> String {
        let mut out = String::new();
        match &self.winner {
            Some(selection) => {
                let label = self
                    .candidates
                    .iter()
                    .find(|candidate| candidate.id == selection.id)
                    .map_or("unknown", |candidate| candidate.label);
                let _ = write!(out, "chose {label} at {:.3}", selection.score);
                if let Some((runner_up, score)) = selection.runner_up {
                    let label = self
                        .candidates
                        .iter()
                        .find(|candidate| candidate.id == runner_up)
                        .map_or("unknown", |candidate| candidate.label);
                    let _ = write!(out, " (runner-up {label} at {score:.3})");
                }
                out.push('\n');
            }
            None => out.push_str("chose nothing: every candidate was inhibited\n"),
        }
        for candidate in &self.candidates {
            let contributions: Vec<String> = candidate
                .contributions
                .iter()
                .map(|(value, weight)| format!("{value:.2}×{weight:.2}"))
                .collect();
            let _ = writeln!(
                out,
                "  {:<16} score {:.3} (raw {:.3}) {}{}",
                candidate.label,
                candidate.score,
                candidate.raw,
                if contributions.is_empty() {
                    "no considerations".to_owned()
                } else {
                    contributions.join(" ")
                },
                if candidate.inhibited {
                    "  [inhibited]"
                } else {
                    ""
                },
            );
        }
        out
    }
}

/// A table of candidate behaviours.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UtilityTable {
    entries: Vec<UtilityEntry>,
    inhibitions: Vec<Inhibition>,
}

impl UtilityTable {
    /// A table from entries.
    ///
    /// # Errors
    ///
    /// [`UtilityError::DuplicateBehavior`] when two entries share an id, because a
    /// duplicate makes the trace ambiguous about which one won.
    pub fn new(entries: Vec<UtilityEntry>) -> Result<Self, UtilityError> {
        let mut seen: Vec<BehaviorId> = Vec::with_capacity(entries.len());
        for entry in &entries {
            if seen.contains(&entry.id) {
                return Err(UtilityError::DuplicateBehavior { id: entry.id });
            }
            seen.push(entry.id);
        }
        Ok(Self {
            entries,
            inhibitions: Vec::new(),
        })
    }

    /// The entries, in registration order.
    #[must_use]
    pub fn entries(&self) -> &[UtilityEntry] {
        &self.entries
    }

    /// Add an inhibition rule.
    pub fn inhibit(&mut self, inhibition: Inhibition) {
        self.inhibitions.push(inhibition);
    }

    /// Whether a behaviour is silenced by any inhibitor.
    fn is_inhibited(&self, id: BehaviorId, blackboard: &Blackboard) -> bool {
        self.inhibitions
            .iter()
            .any(|inhibition| inhibition.target == id && inhibition.is_active(blackboard))
    }

    /// Score every candidate, in registration order.
    #[must_use]
    pub fn score_all(&self, blackboard: &Blackboard) -> SmallVec<[CandidateScore; 8]> {
        let mut out = SmallVec::with_capacity(self.entries.len());
        for entry in &self.entries {
            let (raw, score) = entry.set.breakdown(blackboard);
            let inhibited = self.is_inhibited(entry.id, blackboard);
            let contributions = entry
                .set
                .considerations
                .iter()
                .map(|consideration| (consideration.evaluate(blackboard), consideration.weight))
                .collect();
            out.push(CandidateScore {
                id: entry.id,
                label: entry.label,
                raw,
                score: if inhibited { 0.0 } else { score },
                inhibited,
                contributions,
            });
        }
        out
    }

    /// Choose the highest-scoring behaviour.
    ///
    /// Ties go to the earlier registration, never to iteration order of a map: a
    /// hash map's order is exactly what breaks a replay.
    #[must_use]
    pub fn select(&self, blackboard: &Blackboard) -> Option<Selection> {
        let scored = self.score_all(blackboard);
        let mut best: Option<CandidateScore> = None;
        for candidate in &scored {
            if candidate.inhibited {
                continue;
            }
            match &best {
                Some(current) if candidate.score <= current.score => {}
                _ => best = Some(candidate.clone()),
            }
        }
        let best = best?;
        let runner_up = scored
            .iter()
            .filter(|candidate| !candidate.inhibited && candidate.id != best.id)
            .max_by(|a, b| a.score.total_cmp(&b.score).then_with(|| b.id.cmp(&a.id)))
            .map(|candidate| (candidate.id, candidate.score));
        Some(Selection {
            id: best.id,
            score: best.score,
            runner_up,
        })
    }

    /// Everything about the decision, including the arithmetic.
    #[must_use]
    pub fn explain(&self, blackboard: &Blackboard) -> Explanation {
        Explanation {
            candidates: self.score_all(blackboard),
            winner: self.select(blackboard),
        }
    }
}

/// Why a utility table could not be built.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum UtilityError {
    /// More considerations than a set can hold.
    #[error("a utility set holds at most {limit} considerations")]
    TooManyConsiderations {
        /// The limit.
        limit: usize,
    },
    /// Two behaviours sharing an id.
    #[error("behaviour id {id:?} is registered twice")]
    DuplicateBehavior {
        /// The duplicated id.
        id: BehaviorId,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY_FEAR: BlackboardKey = BlackboardKey::new(101);
    const KEY_GRUDGE: BlackboardKey = BlackboardKey::new(102);
    const KEY_HUNGER: BlackboardKey = BlackboardKey::new(103);

    fn board(fear: f32, grudge: f32, hunger: f32) -> Blackboard {
        let mut blackboard = Blackboard::new();
        blackboard.set(KEY_FEAR, Value::Float(fear));
        blackboard.set(KEY_GRUDGE, Value::Float(grudge));
        blackboard.set(KEY_HUNGER, Value::Float(hunger));
        blackboard
    }

    fn table() -> UtilityTable {
        let mut chat = UtilitySet::new();
        chat.push(Consideration::keyed(KEY_GRUDGE)).expect("push");
        chat.push(Consideration::keyed(KEY_FEAR).with_curve(Curve::Linear {
            slope: -1.0,
            intercept: 1.0,
        }))
        .expect("push");
        let mut eat = UtilitySet::new();
        eat.push(Consideration::keyed(KEY_HUNGER).with_curve(Curve::Step { threshold: 0.5 }))
            .expect("push");
        let mut table = UtilityTable::new(vec![
            UtilityEntry::new(BehaviorId::new(1), "chat", chat),
            UtilityEntry::new(BehaviorId::new(2), "eat", eat),
        ])
        .expect("table");
        table.inhibit(Inhibition {
            target: BehaviorId::new(1),
            when: Input::Key(KEY_FEAR),
            threshold: 0.9,
        });
        table
    }

    #[test]
    fn curves_stay_inside_zero_and_one() {
        for curve in Curve::SIMPLE {
            for input in [-1.0, 0.0, 0.25, 0.5, 1.0, 4.0, f32::NAN] {
                let value = curve.evaluate(input);
                assert!(
                    (0.0..=1.0).contains(&value),
                    "{curve:?} at {input} gave {value}"
                );
            }
        }
    }

    #[test]
    fn a_piecewise_curve_passes_through_its_points() {
        let curve = Curve::Piecewise(smallvec::smallvec![(0.0, 0.0), (0.5, 0.9), (1.0, 1.0)]);
        assert!((curve.evaluate(0.0) - 0.0).abs() < 1e-6);
        assert!((curve.evaluate(0.5) - 0.9).abs() < 1e-6);
        assert!((curve.evaluate(0.75) - 0.95).abs() < 1e-6);
        assert!((curve.evaluate(2.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn multiplication_gates_a_behaviour_off() {
        let mut set = UtilitySet::new();
        set.push(Consideration::keyed(KEY_GRUDGE)).expect("push");
        set.push(Consideration::keyed(KEY_FEAR).with_weight(0.0))
            .expect("push");
        assert_eq!(set.score(&board(0.0, 1.0, 0.0)), 0.0);
    }

    #[test]
    fn compensation_keeps_a_many_consideration_behaviour_competitive() {
        let mut two = UtilitySet::new();
        two.push(Consideration::keyed(KEY_GRUDGE)).expect("push");
        two.push(Consideration::keyed(KEY_GRUDGE)).expect("push");
        let mut five = UtilitySet::new();
        for _ in 0..5 {
            five.push(Consideration::keyed(KEY_GRUDGE)).expect("push");
        }
        let blackboard = board(0.0, 0.8, 0.0);
        assert!(
            five.score(&blackboard) > two.score(&blackboard) * 0.5,
            "compensation did not close the gap"
        );
        five.compensation = false;
        assert!(five.score(&blackboard) < 0.4);
    }

    #[test]
    fn the_highest_score_wins_and_ties_go_to_registration_order() {
        let mut a = UtilitySet::new();
        a.push(Consideration::keyed(KEY_GRUDGE)).expect("push");
        let mut b = UtilitySet::new();
        b.push(Consideration::keyed(KEY_HUNGER)).expect("push");
        let table = UtilityTable::new(vec![
            UtilityEntry::new(BehaviorId::new(1), "first", a),
            UtilityEntry::new(BehaviorId::new(2), "second", b),
        ])
        .expect("table");
        let blackboard = board(0.0, 0.6, 0.6);
        let selection = table.select(&blackboard).expect("a winner");
        assert_eq!(selection.id, BehaviorId::new(1));
        assert_eq!(
            selection.runner_up.map(|(id, _)| id),
            Some(BehaviorId::new(2))
        );
    }

    #[test]
    fn inhibition_cancels_rather_than_competes() {
        let table = table();
        let calm = table.select(&board(0.0, 0.9, 0.1)).expect("a winner");
        assert_eq!(calm.id, BehaviorId::new(1));
        let afraid = table.select(&board(0.95, 0.9, 0.1)).expect("a winner");
        assert_eq!(afraid.id, BehaviorId::new(2), "fear did not cancel chat");
        let explanation = table.explain(&board(0.95, 0.9, 0.1));
        assert!(
            explanation
                .candidates
                .iter()
                .any(|candidate| candidate.inhibited)
        );
    }

    #[test]
    fn when_everything_is_inhibited_nothing_is_chosen() {
        let mut set = UtilitySet::new();
        set.push(Consideration::keyed(KEY_GRUDGE)).expect("push");
        let mut table = UtilityTable::new(vec![UtilityEntry::new(BehaviorId::new(1), "chat", set)])
            .expect("table");
        table.inhibit(Inhibition {
            target: BehaviorId::new(1),
            when: Input::Constant(1.0),
            threshold: 0.5,
        });
        assert!(table.select(&board(0.0, 1.0, 0.0)).is_none());
        assert!(
            table
                .explain(&board(0.0, 1.0, 0.0))
                .text()
                .contains("nothing")
        );
    }

    #[test]
    fn the_explanation_names_the_winner_and_the_arithmetic() {
        let text = table().explain(&board(0.0, 0.7, 0.2)).text();
        assert!(text.contains("chat"), "{text}");
        assert!(text.contains("runner-up"), "{text}");
        assert!(
            text.contains('×'),
            "per-consideration weights are missing: {text}"
        );
    }

    #[test]
    fn duplicate_behaviours_are_refused() {
        let set = UtilitySet::new();
        let entries = vec![
            UtilityEntry::new(BehaviorId::new(1), "a", set.clone()),
            UtilityEntry::new(BehaviorId::new(1), "b", set),
        ];
        assert!(matches!(
            UtilityTable::new(entries).unwrap_err(),
            UtilityError::DuplicateBehavior { .. }
        ));
    }

    #[test]
    fn a_set_cannot_grow_past_its_capacity() {
        let mut set = UtilitySet::new();
        for _ in 0..MAX_CONSIDERATIONS {
            set.push(Consideration::keyed(KEY_GRUDGE)).expect("push");
        }
        assert!(matches!(
            set.push(Consideration::keyed(KEY_GRUDGE)).unwrap_err(),
            UtilityError::TooManyConsiderations { .. }
        ));
    }

    #[test]
    fn an_empty_set_always_wants_to_run() {
        let set = UtilitySet::new();
        assert_eq!(set.score(&board(0.0, 0.0, 0.0)), 1.0);
    }
}
