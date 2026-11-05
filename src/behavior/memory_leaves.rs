//! Leaves that let a behaviour tree read what an NPC remembers.
//!
//! The store itself lives in [`crate::domain::memory`] and is gated behind the
//! `memory` feature, because a crowd of a million does not need it and should not pay
//! for it. These leaves are not gated: they read three integers off the blackboard and
//! do not know where the numbers came from. That is the seam that makes memory
//! optional — the bridge which writes the numbers is gated, the behaviour which reads
//! them is not, so a game that disables memory keeps every tree it authored and simply
//! never sees a familiar face.
//!
//! The three keys are written once per think, not once per leaf evaluation: a tree may
//! ask "is this a grudge?" from four different branches within one tick, and each of
//! those reads should cost a slot lookup and nothing more.
use crate::behavior::blackboard::{Blackboard, BlackboardKey, Value};
use crate::behavior::registry::LeafBehavior;
use crate::behavior::social::KEY_TALK_TO;
use crate::behavior::tree::{LeafContext, Status};
use crate::domain::cost::VirtualCost;

/// How the strongest remembered subject feels about the NPC, signed. Negative is
/// hostile.
pub const KEY_MEMORY_VALENCE: BlackboardKey = BlackboardKey::new(40);

/// How well the NPC knows that subject, `0..=65535`, already decayed.
pub const KEY_MEMORY_FAMILIARITY: BlackboardKey = BlackboardKey::new(41);

/// The encounter-kind flags of that memory.
///
/// Mirrors `domain::memory::EncounterKind::flag`, which lives behind the `memory`
/// feature: the values are a wire format between the bridge and these leaves, so they are
/// restated here rather than imported. A test in `domain::memory` pins the two together.
pub const FLAG_ATTACKED: u16 = 1 << 2;
/// Flag bit for a robbery, mirroring `EncounterKind::Robbed`.
pub const FLAG_ROBBED: u16 = 1 << 3;
/// The encounter-kind flags of that memory, as written by the bridge.
pub const KEY_MEMORY_FLAGS: BlackboardKey = BlackboardKey::new(42);

/// Familiarity at which an NPC treats someone as known rather than as a stranger.
///
/// Around four conversations: enough that a passing glance does not qualify, low enough
/// that a shopkeeper recognises a regular on their second visit.
pub const RECOGNITION_THRESHOLD: u16 = 24;

/// Valence at or below which a memory counts as hostility even without a grudge flag.
pub const GRUDGE_VALENCE: i16 = -300;

/// Valence at or above which a known face is welcome.
pub const WARM_VALENCE: i16 = 200;

/// The memory view a leaf reads, lifted off the blackboard.
///
/// `None` means "no memory of anyone", which is different from "a neutral memory": the
/// first should leave a stranger alone, the second should treat them as a customer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Remembered {
    /// Emotional colouring, signed.
    pub valence: i16,
    /// Decayed familiarity.
    pub familiarity: u16,
    /// Encounter-kind flags.
    pub flags: u16,
}

impl Remembered {
    /// Read the view from a blackboard.
    ///
    /// Familiarity is the gate: without it the other two numbers are meaningless, so a
    /// blackboard that has a valence but no familiarity is treated as remembering
    /// nobody rather than as remembering someone who feels strongly about nothing.
    #[must_use]
    pub fn read(blackboard: &Blackboard) -> Option<Self> {
        let familiarity = whole(blackboard.get(KEY_MEMORY_FAMILIARITY))?;
        if familiarity == 0 {
            return None;
        }
        Some(Self {
            valence: i16::try_from(whole(blackboard.get(KEY_MEMORY_VALENCE)).unwrap_or(0)).ok()?,
            familiarity: u16::try_from(familiarity).ok()?,
            flags: u16::try_from(whole(blackboard.get(KEY_MEMORY_FLAGS)).unwrap_or(0)).ok()?,
        })
    }

    /// Whether this subject did something hostile, by flag rather than by intensity.
    #[must_use]
    pub fn is_grudge(self) -> bool {
        let hostile = FLAG_ATTACKED | FLAG_ROBBED;
        self.flags & hostile != 0 || self.valence <= GRUDGE_VALENCE
    }

    /// Whether this subject is both known and welcome.
    #[must_use]
    pub fn is_friend(self) -> bool {
        self.familiarity >= RECOGNITION_THRESHOLD && self.valence >= WARM_VALENCE
    }

    /// Whether the NPC would recognise this subject at all.
    #[must_use]
    pub fn is_known(self) -> bool {
        self.familiarity >= RECOGNITION_THRESHOLD
    }
}

/// Read a slot as a whole number, whether the bridge wrote an integer or a float.
///
/// The bridge writes integers because memory levels are counts, but a game is free to
/// publish its own approximation as a float, and a leaf that only understood `Int`
/// would silently read that as "no memory".
fn whole(value: Option<Value>) -> Option<i64> {
    match value? {
        Value::Int(raw) => Some(raw),
        Value::Float(raw) if raw.is_finite() => Some(raw as i64),
        Value::Bool(_) | Value::Npc(_) | Value::Position(_) | Value::Float(_) => None,
    }
}

/// The leaf identifiers a tree uses to ask what an NPC remembers.
///
/// Registered together because they only make sense together: a tree that greets a friend
/// and shuns a grudge is the whole point, and a game that registered one and forgot the
/// other would ship a town with no memory of either.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryLeaves {
    /// Succeeds when the NPC recognises the person in front of them.
    pub recognise: crate::behavior::tree::LeafId,
    /// Succeeds when that person did the NPC harm.
    pub shun_grudge: crate::behavior::tree::LeafId,
    /// Succeeds and opens a conversation when that person is welcome.
    pub greet_friend: crate::behavior::tree::LeafId,
}

impl MemoryLeaves {
    /// Register the three leaves under their canonical names.
    ///
    /// The names are the ones [`crate::behavior::registry::FlockRegistry::leaf_name`]
    /// reports, so a debug overlay or a log reads the same word the author wrote.
    pub fn register(
        registry: &mut crate::behavior::registry::FlockRegistry,
    ) -> Result<Self, crate::error::RegistrationError> {
        Ok(Self {
            recognise: registry.register_leaf("recognise_known", RecogniseKnown)?,
            shun_grudge: registry.register_leaf("shun_grudge", ShunGrudge)?,
            greet_friend: registry.register_leaf("greet_friend", GreetFriend)?,
        })
    }
}

/// Succeed when the NPC recognises the person it is looking at.
///
/// The cheapest of the three: one slot read, no write. Use it as the guard on a branch
/// that only makes sense for a familiar face — a shopkeeper's "back again?" line rather
/// than their "what'll it be?".
#[derive(Debug, Clone, Copy, Default)]
pub struct RecogniseKnown;

impl LeafBehavior for RecogniseKnown {
    fn tick(&self, ctx: &mut LeafContext<'_>) -> Status {
        match Remembered::read(ctx.blackboard) {
            Some(view) if view.is_known() => Status::Success,
            _ => Status::Failure,
        }
    }

    fn cost_hint(&self) -> VirtualCost {
        VirtualCost::new(2)
    }

    fn label(&self) -> &'static str {
        "recognise_known"
    }
}

/// Succeed when the remembered person did this NPC harm.
///
/// Writes [`crate::behavior::social::KEY_WARY_OF`]'s sibling, [`KEY_TALK_TO`], only when
/// the branch is taken; a grudge that ends the conversation is expressed by the branch
/// the tree author writes, not by this leaf.
#[derive(Debug, Clone, Copy, Default)]
pub struct ShunGrudge;

impl LeafBehavior for ShunGrudge {
    fn tick(&self, ctx: &mut LeafContext<'_>) -> Status {
        let hostile = Remembered::read(ctx.blackboard).is_some_and(Remembered::is_grudge);
        if !hostile {
            return Status::Failure;
        }
        // A grudge is not a conversation: the NPC turns away, which the tree can turn
        // into a retreat target or a refusal line.
        let _ = ctx.blackboard.remove(KEY_TALK_TO);
        Status::Success
    }

    fn cost_hint(&self) -> VirtualCost {
        VirtualCost::new(3)
    }

    fn label(&self) -> &'static str {
        "shun_grudge"
    }
}

/// Succeed and open a conversation when the remembered person is welcome.
///
/// Sets [`KEY_TALK_TO`], the same key [`crate::behavior::social::GreetKnown`] sets, so a
/// bark emitter or a dialogue system does not need to care whether the greeting came
/// from noticing someone or from remembering them.
#[derive(Debug, Clone, Copy, Default)]
pub struct GreetFriend;

impl LeafBehavior for GreetFriend {
    fn tick(&self, ctx: &mut LeafContext<'_>) -> Status {
        let welcome = Remembered::read(ctx.blackboard).is_some_and(Remembered::is_friend);
        if !welcome {
            return Status::Failure;
        }
        let _ = ctx.blackboard.set(KEY_TALK_TO, Value::Bool(true));
        Status::Success
    }

    fn abort(&self, ctx: &mut LeafContext<'_>) {
        let _ = ctx.blackboard.remove(KEY_TALK_TO);
    }

    fn cost_hint(&self) -> VirtualCost {
        VirtualCost::new(3)
    }

    fn label(&self) -> &'static str {
        "greet_friend"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::cost::VirtualCost;

    fn board(view: Option<Remembered>) -> Blackboard {
        let mut blackboard = Blackboard::new();
        if let Some(view) = view {
            let _ = blackboard.set(
                KEY_MEMORY_FAMILIARITY,
                Value::Int(i64::from(view.familiarity)),
            );
            let _ = blackboard.set(KEY_MEMORY_VALENCE, Value::Int(i64::from(view.valence)));
            let _ = blackboard.set(KEY_MEMORY_FLAGS, Value::Int(i64::from(view.flags)));
        }
        blackboard
    }

    fn context(blackboard: &mut Blackboard) -> LeafContext<'_> {
        LeafContext {
            blackboard,
            tick: crate::domain::tick::Tick::ZERO,
            npc: crate::domain::ids::NpcId::unique(1),
        }
    }

    #[test]
    fn a_stranger_is_not_recognised_and_costs_one_read() {
        let mut blackboard = board(None);
        assert_eq!(
            RecogniseKnown.tick(&mut context(&mut blackboard)),
            Status::Failure
        );
        assert!(Remembered::read(&blackboard).is_none());
    }

    #[test]
    fn a_faded_memory_reads_as_no_memory_rather_than_as_neutral() {
        let mut blackboard = board(Some(Remembered {
            valence: 500,
            familiarity: 0,
            flags: 0,
        }));
        // Valence without familiarity is not a memory; treating it as one would make
        // every NPC fond of a stranger.
        assert!(Remembered::read(&blackboard).is_none());
        assert!(blackboard.remove(KEY_MEMORY_VALENCE).is_some());
    }

    #[test]
    fn a_known_face_is_recognised() {
        let mut blackboard = board(Some(Remembered {
            valence: 0,
            familiarity: RECOGNITION_THRESHOLD,
            flags: 0,
        }));
        assert_eq!(
            RecogniseKnown.tick(&mut context(&mut blackboard)),
            Status::Success
        );
    }

    /// The leaves restate the flag bits rather than import them, so the two definitions
    /// have to agree. This is the test that makes the duplication safe.
    #[cfg(feature = "memory")]
    #[test]
    fn the_restated_flag_bits_match_the_encounter_kinds() {
        use crate::domain::memory::EncounterKind;
        assert_eq!(FLAG_ATTACKED, EncounterKind::Attacked.flag());
        assert_eq!(FLAG_ROBBED, EncounterKind::Robbed.flag());
    }

    #[test]
    fn a_grudge_is_recognised_by_flag_even_when_the_valence_is_positive() {
        let mut blackboard = board(Some(Remembered {
            valence: 0,
            familiarity: 40,
            flags: FLAG_ROBBED,
        }));
        assert_eq!(
            ShunGrudge.tick(&mut context(&mut blackboard)),
            Status::Success
        );
    }

    #[test]
    fn a_grudge_is_recognised_by_intensity_without_a_flag() {
        let mut blackboard = board(Some(Remembered {
            valence: GRUDGE_VALENCE,
            familiarity: 40,
            flags: 0,
        }));
        assert_eq!(
            ShunGrudge.tick(&mut context(&mut blackboard)),
            Status::Success
        );
    }

    #[test]
    fn a_mere_acquaintance_is_not_a_friend() {
        let mut blackboard = board(Some(Remembered {
            valence: WARM_VALENCE,
            familiarity: RECOGNITION_THRESHOLD - 1,
            flags: 0,
        }));
        assert_eq!(
            GreetFriend.tick(&mut context(&mut blackboard)),
            Status::Failure
        );
    }

    #[test]
    fn a_friend_opens_a_conversation_through_the_same_key_a_stranger_greeting_uses() {
        let mut blackboard = board(Some(Remembered {
            valence: WARM_VALENCE,
            familiarity: 60,
            flags: 0,
        }));
        assert_eq!(
            GreetFriend.tick(&mut context(&mut blackboard)),
            Status::Success
        );
        assert_eq!(blackboard.get(KEY_TALK_TO), Some(Value::Bool(true)));
        GreetFriend.abort(&mut context(&mut blackboard));
        assert_eq!(blackboard.get(KEY_TALK_TO), None);
    }

    #[test]
    fn the_leaves_declare_their_costs_inside_the_measured_catalogue_range() {
        // ADR 0009: anything above ten micro-ticks is a think, not a leaf, and the
        // scheduler rations those separately.
        for cost in [
            RecogniseKnown.cost_hint(),
            ShunGrudge.cost_hint(),
            GreetFriend.cost_hint(),
        ] {
            assert!(cost <= VirtualCost::new(10), "{cost:?}");
        }
    }
}
