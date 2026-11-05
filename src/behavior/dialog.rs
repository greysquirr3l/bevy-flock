//! Conversations: authored graphs, validated before they can be played.
//!
//! A dialog graph is content, not code. It is written by a writer, loaded from a
//! data file, and — this is the part that matters — **validated on load**, because
//! the alternative is a writer's typo becoming a soft-lock three hours into a
//! playtest.
//!
//! The validator is deliberately strict. A graph is rejected when a choice points
//! at a node that does not exist, when a node is unreachable from the root, and
//! when a branch can loop forever without reaching an end node. The last one is
//! the expensive check (a depth-first search over the choice graph) and it is the
//! one that catches "the player picks option 2, the merchant repeats himself, and
//! there is no way out".
//!
//! ## Shape
//!
//! ```ron
//! (
//!   root: 0,
//!   nodes: [
//!     (id: 0, speaker: Npc, text: "Salt? Fresh in this morning.",
//!       choices: [(text: "How much?", next: 1), (text: "Later.", next: 2)]),
//!     (id: 1, speaker: Npc, text: "Two coppers a measure.",
//!       choices: [(text: "I'll take two.", next: 2, requires: Some((key: 7, at_least: 2.0)))]),
//!     (id: 2, speaker: Npc, text: "Mind the step.", choices: []),
//!   ],
//! )
//! ```
//!
//! A node with no choices is an end node. Playing a graph is a cursor and a set of
//! choices filtered by the NPC's blackboard, which keeps conversation inside the
//! same deterministic, allocation-free model as the rest of cognition: a session
//! is a few integers, and the text is borrowed from the loaded graph.

use smallvec::SmallVec;
use crate::behavior::blackboard::{Blackboard, BlackboardKey, Value};

/// The most choices one node may offer.
pub const MAX_CHOICES: usize = 6;

/// Who is speaking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Speaker {
    /// The NPC.
    Npc,
    /// The player character.
    Player,
    /// Neither: narration.
    Narrator,
}

/// A condition on a blackboard value.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Condition {
    /// The slot to read.
    pub key: BlackboardKey,
    /// The value it must be at least.
    pub at_least: f32,
}

impl Condition {
    /// Whether the condition holds.
    #[must_use]
    pub fn holds(self, blackboard: &Blackboard) -> bool {
        let value = match blackboard.get(self.key) {
            Some(Value::Float(value)) => value,
            Some(Value::Int(value)) => value as f32,
            Some(Value::Bool(value)) => f32::from(u8::from(value)),
            _ => 0.0,
        };
        value >= self.at_least
    }
}

/// One thing the player can say.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DialogChoice {
    /// What the player says.
    pub text: String,
    /// The node it leads to.
    pub next: u16,
    /// When the choice is offered. `None` means always.
    pub requires: Option<Condition>,
}

impl DialogChoice {
    /// An unconditional choice.
    #[must_use]
    pub fn new(text: impl Into<String>, next: u16) -> Self {
        Self {
            text: text.into(),
            next,
            requires: None,
        }
    }

    /// Offer this choice only when a condition holds.
    #[must_use]
    pub fn requiring(mut self, key: BlackboardKey, at_least: f32) -> Self {
        self.requires = Some(Condition { key, at_least });
        self
    }
}

/// A line of dialog and what can follow it.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DialogNode {
    /// The node's identity, unique within the graph.
    pub id: u16,
    /// Who is speaking.
    pub speaker: Speaker,
    /// What they say.
    pub text: String,
    /// What the player may reply. Empty means the conversation ends here.
    pub choices: Vec<DialogChoice>,
}

impl DialogNode {
    /// An end node: a line with nothing after it.
    #[must_use]
    pub fn ending(id: u16, speaker: Speaker, text: impl Into<String>) -> Self {
        Self {
            id,
            speaker,
            text: text.into(),
            choices: Vec::new(),
        }
    }

    /// Whether the node ends the conversation.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        self.choices.is_empty()
    }
}

/// A conversation, as authored.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DialogGraph {
    /// The node a conversation starts at.
    pub root: u16,
    /// The nodes, in authoring order.
    pub nodes: Vec<DialogNode>,
}

impl DialogGraph {
    /// A graph from a root and its nodes, validated.
    ///
    /// # Errors
    ///
    /// [`DialogError`] describes the first structural problem found: a duplicate
    /// id, a choice pointing nowhere, a node unreachable from the root, a node
    /// whose every branch loops without ever ending, or too many choices.
    pub fn new(root: u16, nodes: Vec<DialogNode>) -> Result<Self, DialogError> {
        let graph = Self { root, nodes };
        graph.validate()?;
        Ok(graph)
    }

    /// Load a graph from RON.
    ///
    /// # Errors
    ///
    /// [`DialogError::Parse`] carries the parser's own position for the writer to
    /// fix, or a validation error if the file parses but does not make sense.
    #[cfg(feature = "serde")]
    pub fn from_ron(source: &str) -> Result<Self, DialogError> {
        let graph: Self = ron::from_str(source).map_err(|error| DialogError::Parse {
            line: error.span.start.line,
            message: error.code.to_string(),
        })?;
        graph.validate()?;
        Ok(graph)
    }

    /// Write the graph back out as RON.
    ///
    /// # Errors
    ///
    /// [`DialogError::Parse`] if the encoder fails, which it cannot for this shape;
    /// the variant is reused so callers have one error type.
    #[cfg(feature = "serde")]
    pub fn to_ron(&self) -> Result<String, DialogError> {
        ron::ser::to_string_pretty(self, ron::ser::PrettyConfig::default()).map_err(|error| {
            DialogError::Parse {
                line: 0,
                message: error.to_string(),
            }
        })
    }

    /// How many nodes the graph has.
    #[must_use]
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Whether the graph has no nodes. A validated graph cannot, but the method
    /// exists so the type is usable where an emptiness check is idiomatic.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// A node by id.
    #[must_use]
    pub fn node(&self, id: u16) -> Option<&DialogNode> {
        self.nodes.iter().find(|node| node.id == id)
    }

    /// The node an id refers to, positionally: the validator guarantees ids are
    /// unique, so an index array is authoritative after validation.
    fn index_of(&self, id: u16) -> Option<usize> {
        self.nodes.iter().position(|node| node.id == id)
    }

    /// Check the graph's structure.
    ///
    /// # Errors
    ///
    /// As [`DialogGraph::new`].
    pub fn validate(&self) -> Result<(), DialogError> {
        if self.nodes.is_empty() {
            return Err(DialogError::Empty);
        }
        // Unique ids.
        let mut seen: Vec<u16> = Vec::with_capacity(self.nodes.len());
        for node in &self.nodes {
            if seen.contains(&node.id) {
                return Err(DialogError::DuplicateNode { id: node.id });
            }
            seen.push(node.id);
            if node.choices.len() > MAX_CHOICES {
                return Err(DialogError::TooManyChoices {
                    node: node.id,
                    count: node.choices.len(),
                    limit: MAX_CHOICES,
                });
            }
        }
        if self.index_of(self.root).is_none() {
            return Err(DialogError::UnknownNode { id: self.root });
        }
        // Every choice resolves.
        for node in &self.nodes {
            for choice in &node.choices {
                if self.index_of(choice.next).is_none() {
                    return Err(DialogError::DanglingChoice {
                        from: node.id,
                        to: choice.next,
                    });
                }
            }
        }
        // Reachability from the root.
        let mut reachable = vec![false; self.nodes.len()];
        let mut stack = vec![self.root];
        while let Some(id) = stack.pop() {
            let Some(index) = self.index_of(id) else {
                continue;
            };
            if reachable.get(index).copied().unwrap_or(true) {
                continue;
            }
            if let Some(slot) = reachable.get_mut(index) {
                *slot = true;
            }
            if let Some(node) = self.nodes.get(index) {
                for choice in &node.choices {
                    stack.push(choice.next);
                }
            }
        }
        if let Some(orphan) = self
            .nodes
            .iter()
            .zip(reachable.iter())
            .find(|(_, reachable)| !**reachable)
            .map(|(node, _)| node.id)
        {
            return Err(DialogError::UnreachableNode { id: orphan });
        }
        // Every reachable node can reach an end node: an authoring loop with no exit
        // is a soft-lock, and a soft-lock in a conversation is unrecoverable.
        let terminals: Vec<usize> = self
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, node)| node.is_terminal())
            .map(|(index, _)| index)
            .collect();
        if terminals.is_empty() {
            return Err(DialogError::NoEnding);
        }
        let mut can_end = vec![false; self.nodes.len()];
        for index in terminals {
            if let Some(slot) = can_end.get_mut(index) {
                *slot = true;
            }
        }
        // Fixpoint: a node can end if any choice leads somewhere that can end.
        loop {
            let mut changed = false;
            for (index, node) in self.nodes.iter().enumerate() {
                if can_end.get(index).copied().unwrap_or(true) {
                    continue;
                }
                for choice in &node.choices {
                    let next_index = self.index_of(choice.next);
                    if next_index
                        .and_then(|next| can_end.get(next).copied())
                        .unwrap_or(false)
                    {
                        if let Some(slot) = can_end.get_mut(index) {
                            *slot = true;
                        }
                        changed = true;
                        break;
                    }
                }
            }
            if !changed {
                break;
            }
        }
        if let Some(stuck) = self
            .nodes
            .iter()
            .zip(can_end.iter())
            .find(|(_, can_end)| !**can_end)
            .map(|(node, _)| node.id)
        {
            return Err(DialogError::CycleWithoutExit { id: stuck });
        }
        Ok(())
    }

    /// Open a conversation.
    #[must_use]
    pub fn open(&self) -> DialogSession {
        DialogSession {
            cursor: self.root,
            visited: 0,
            finished: false,
        }
    }
}

/// A conversation in progress.
///
/// Four `u16`-sized fields: a session lives in the blackboard's worth of state, not
/// in an allocation per NPC, which is what makes conversations possible for a
/// thousand NPCs at once if a game wants them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DialogSession {
    cursor: u16,
    visited: u32,
    finished: bool,
}

impl DialogSession {
    /// The node the conversation is on.
    #[must_use]
    pub const fn cursor(&self) -> u16 {
        self.cursor
    }

    /// How many lines have been said.
    #[must_use]
    pub const fn visited(&self) -> u32 {
        self.visited
    }

    /// Whether the conversation has ended.
    #[must_use]
    pub const fn is_finished(&self) -> bool {
        self.finished
    }

    /// The choices currently on offer, filtered by the blackboard.
    #[must_use]
    pub fn choices<'a>(
        &self,
        graph: &'a DialogGraph,
        blackboard: &Blackboard,
    ) -> SmallVec<[&'a DialogChoice; MAX_CHOICES]> {
        let mut out = SmallVec::new();
        let Some(node) = graph.node(self.cursor) else {
            return out;
        };
        for choice in &node.choices {
            if choice.requires.is_none_or(|condition| condition.holds(blackboard)) {
                out.push(choice);
            }
        }
        out
    }

    /// Take a choice by its position in [`DialogSession::choices`].
    ///
    /// # Errors
    ///
    /// [`DialogError::NoSuchChoice`] when the index is out of range for the
    /// currently offered choices — which is the realistic failure when a game keeps
    /// a choice index across a frame in which the blackboard changed.
    pub fn choose(
        &mut self,
        graph: &DialogGraph,
        blackboard: &Blackboard,
        index: usize,
    ) -> Result<u16, DialogError> {
        let offered = self.choices(graph, blackboard);
        let Some(choice) = offered.get(index) else {
            return Err(DialogError::NoSuchChoice { index });
        };
        self.cursor = choice.next;
        self.visited = self.visited.saturating_add(1);
        let terminal = graph.node(self.cursor).is_none_or(DialogNode::is_terminal);
        self.finished = terminal;
        Ok(self.cursor)
    }

    /// End the conversation without choosing.
    pub fn close(&mut self) {
        self.finished = true;
    }

    /// The current line, if the graph still has the node.
    #[must_use]
    pub fn line<'a>(&self, graph: &'a DialogGraph) -> Option<&'a DialogNode> {
        graph.node(self.cursor)
    }
}

/// Why a graph was rejected, or a session refused an action.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DialogError {
    /// No nodes at all.
    #[error("a dialog graph needs at least one node")]
    Empty,
    /// Two nodes sharing an id.
    #[error("dialog node {id} is defined twice")]
    DuplicateNode {
        /// The duplicated id.
        id: u16,
    },
    /// A choice pointing at a node that does not exist.
    #[error("node {from} has a choice that points at missing node {to}")]
    DanglingChoice {
        /// The node the choice is on.
        from: u16,
        /// The missing destination.
        to: u16,
    },
    /// A node the conversation can never show.
    #[error("node {id} is unreachable from the root")]
    UnreachableNode {
        /// The orphan.
        id: u16,
    },
    /// A branch that can loop without ever ending.
    #[error("node {id} can loop forever without reaching an ending")]
    CycleWithoutExit {
        /// A node in the trap.
        id: u16,
    },
    /// No node ends the conversation.
    #[error("a dialog graph needs at least one node with no choices")]
    NoEnding,
    /// The root is not a node.
    #[error("the root node {id} does not exist")]
    UnknownNode {
        /// The missing id.
        id: u16,
    },
    /// More choices than a node may offer.
    #[error("node {node} offers {count} choices, the limit is {limit}")]
    TooManyChoices {
        /// The node.
        node: u16,
        /// How many it offers.
        count: usize,
        /// The limit.
        limit: usize,
    },
    /// A session asked for a choice it is not offering.
    #[error("choice {index} is not currently offered")]
    NoSuchChoice {
        /// The requested index.
        index: usize,
    },
    /// A file that would not parse.
    #[error("dialog graph failed to parse at line {line}: {message}")]
    Parse {
        /// The line the parser stopped on.
        line: usize,
        /// The parser's complaint.
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(raw: u16) -> BlackboardKey {
        BlackboardKey::new(raw)
    }

    fn merchant() -> DialogGraph {
        DialogGraph::new(
            0,
            vec![
                DialogNode {
                    id: 0,
                    speaker: Speaker::Npc,
                    text: "Salt? Fresh in this morning.".to_owned(),
                    choices: vec![
                        DialogChoice::new("How much?", 1),
                        DialogChoice::new("You owe me.", 3).requiring(key(1), 1.0),
                        DialogChoice::new("Later.", 2),
                    ],
                },
                DialogNode {
                    id: 1,
                    speaker: Speaker::Npc,
                    text: "Two coppers a measure.".to_owned(),
                    choices: vec![DialogChoice::new("I'll take two.", 2)],
                },
                DialogNode::ending(2, Speaker::Npc, "Mind the step."),
                DialogNode {
                    id: 3,
                    speaker: Speaker::Player,
                    text: "You still owe me.".to_owned(),
                    choices: vec![DialogChoice::new("Forget it.", 2)],
                },
            ],
        )
        .expect("a valid graph")
    }

    #[test]
    fn a_valid_graph_plays_to_an_ending() {
        let graph = merchant();
        let blackboard = Blackboard::new();
        let mut session = graph.open();
        assert_eq!(session.choices(&graph, &blackboard).len(), 2);
        assert_eq!(session.choose(&graph, &blackboard, 0), Ok(1));
        assert_eq!(session.choose(&graph, &blackboard, 0), Ok(2));
        assert!(session.is_finished());
        assert_eq!(session.visited(), 2);
    }

    #[test]
    fn a_condition_hides_a_choice_until_it_holds() {
        let graph = merchant();
        let mut blackboard = Blackboard::new();
        let session = graph.open();
        assert_eq!(session.choices(&graph, &blackboard).len(), 2);
        blackboard.set(key(1), Value::Float(1.0));
        assert_eq!(session.choices(&graph, &blackboard).len(), 3);
    }

    #[test]
    fn choosing_something_not_offered_is_refused() {
        let graph = merchant();
        let blackboard = Blackboard::new();
        let mut session = graph.open();
        assert_eq!(
            session.choose(&graph, &blackboard, 7),
            Err(DialogError::NoSuchChoice { index: 7 })
        );
    }

    #[test]
    fn a_dangling_choice_is_rejected() {
        let error = DialogGraph::new(
            0,
            vec![DialogNode {
                id: 0,
                speaker: Speaker::Npc,
                text: "Hello.".to_owned(),
                choices: vec![DialogChoice::new("Bye.", 9)],
            }],
        )
        .unwrap_err();
        assert_eq!(error, DialogError::DanglingChoice { from: 0, to: 9 });
    }

    #[test]
    fn an_unreachable_node_is_rejected() {
        let error = DialogGraph::new(
            0,
            vec![
                DialogNode::ending(0, Speaker::Npc, "Hello."),
                DialogNode::ending(1, Speaker::Npc, "Nobody hears this."),
            ],
        )
        .unwrap_err();
        assert_eq!(error, DialogError::UnreachableNode { id: 1 });
    }

    #[test]
    fn a_duplicate_id_is_rejected() {
        let error = DialogGraph::new(
            0,
            vec![
                DialogNode::ending(0, Speaker::Npc, "One."),
                DialogNode::ending(0, Speaker::Npc, "Two."),
            ],
        )
        .unwrap_err();
        assert_eq!(error, DialogError::DuplicateNode { id: 0 });
    }

    #[test]
    fn a_loop_without_an_ending_is_rejected() {
        // 0 -> 1 -> 0, and no terminal node at all: the classic soft-lock.
        let error = DialogGraph::new(
            0,
            vec![
                DialogNode {
                    id: 0,
                    speaker: Speaker::Npc,
                    text: "Hmm.".to_owned(),
                    choices: vec![DialogChoice::new("Go on.", 1)],
                },
                DialogNode {
                    id: 1,
                    speaker: Speaker::Npc,
                    text: "Where was I?".to_owned(),
                    choices: vec![DialogChoice::new("Back.", 0)],
                },
            ],
        )
        .unwrap_err();
        assert_eq!(error, DialogError::NoEnding);
    }

    #[test]
    fn a_local_loop_with_an_exit_is_allowed() {
        // 0 <-> 1 with an exit at 1: a conversation a player can circle for a while.
        let graph = DialogGraph::new(
            0,
            vec![
                DialogNode {
                    id: 0,
                    speaker: Speaker::Npc,
                    text: "Ask away.".to_owned(),
                    choices: vec![DialogChoice::new("Who are you?", 1)],
                },
                DialogNode {
                    id: 1,
                    speaker: Speaker::Npc,
                    text: "A merchant.".to_owned(),
                    choices: vec![
                        DialogChoice::new("Again.", 0),
                        DialogChoice::new("Goodbye.", 2),
                    ],
                },
                DialogNode::ending(2, Speaker::Npc, "Safe roads."),
            ],
        )
        .expect("a loop with an exit is fine");
        assert_eq!(graph.len(), 3);
    }

    #[test]
    fn a_node_in_a_trap_with_a_far_ending_is_caught() {
        // 0 -> 1 -> 2 -> 1 (trap) while 0 -> 3 ends. Node 1 can never end.
        let error = DialogGraph::new(
            0,
            vec![
                DialogNode {
                    id: 0,
                    speaker: Speaker::Npc,
                    text: "Choose.".to_owned(),
                    choices: vec![DialogChoice::new("Wrong way.", 1), DialogChoice::new("Leave.", 3)],
                },
                DialogNode {
                    id: 1,
                    speaker: Speaker::Npc,
                    text: "One.".to_owned(),
                    choices: vec![DialogChoice::new("Next.", 2)],
                },
                DialogNode {
                    id: 2,
                    speaker: Speaker::Npc,
                    text: "Two.".to_owned(),
                    choices: vec![DialogChoice::new("Back.", 1)],
                },
                DialogNode::ending(3, Speaker::Npc, "Out."),
            ],
        )
        .unwrap_err();
        assert_eq!(error, DialogError::CycleWithoutExit { id: 1 });
    }

    #[test]
    fn too_many_choices_are_rejected() {
        let choices: Vec<DialogChoice> = (0..=MAX_CHOICES as u16)
            .map(|index| DialogChoice::new(format!("Option {index}"), 1))
            .collect();
        let error = DialogGraph::new(
            0,
            vec![
                DialogNode {
                    id: 0,
                    speaker: Speaker::Npc,
                    text: "Many doors.".to_owned(),
                    choices,
                },
                DialogNode::ending(1, Speaker::Npc, "One room."),
            ],
        )
        .unwrap_err();
        assert!(matches!(error, DialogError::TooManyChoices { .. }));
    }

    #[cfg(feature = "serde")]
    #[test]
    fn a_graph_round_trips_through_ron() {
        let graph = merchant();
        let text = graph.to_ron().expect("serialise");
        let loaded = DialogGraph::from_ron(&text).expect("load");
        assert_eq!(loaded, graph);
    }

    #[cfg(feature = "serde")]
    #[test]
    fn a_malformed_file_reports_a_line() {
        let error = DialogGraph::from_ron("(root: 0, nodes: [ (id: 0,").unwrap_err();
        assert!(matches!(error, DialogError::Parse { .. }));
    }
}
