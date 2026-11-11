//! A small typed store for per-NPC working memory.
//!
//! Keys are explicit constants rather than strings: a typo in a string key is a
//! silent behaviour bug, a typo in a `BlackboardKey` constant is a compile error.
//! Capacity is fixed, so an NPC's working memory is a stack allocation and can
//! never grow with the number of behaviours a game registers.

use bevy_math::Vec3;

use crate::domain::ids::NpcId;

/// The most values one NPC can hold at once.
pub const BLACKBOARD_CAPACITY: usize = 16;

/// A working-memory slot identifier.
///
/// The serialised form is the bare slot number. A newtype that serialises as its
/// own name would make every authored file spell out a Rust type, and the
/// `key: 7` in the dialog documentation is what an author writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(transparent))]
pub struct BlackboardKey(u16);

impl BlackboardKey {
    /// Define a key.
    ///
    /// Two keys with the same value address the same slot; a game should define
    /// its keys once as constants rather than inline.
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

/// A value an NPC can remember.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Value {
    /// A flag.
    Bool(bool),
    /// A whole number.
    Int(i64),
    /// A scalar.
    Float(f32),
    /// A reference to another NPC.
    Npc(NpcId),
    /// A world position.
    Position(Vec3),
}

impl Value {
    /// Read as a boolean, if it is one.
    #[must_use]
    pub const fn as_bool(self) -> Option<bool> {
        match self {
            Self::Bool(value) => Some(value),
            _ => None,
        }
    }

    /// Read as an integer, if it is one.
    #[must_use]
    pub const fn as_int(self) -> Option<i64> {
        match self {
            Self::Int(value) => Some(value),
            _ => None,
        }
    }

    /// Read as a float, if it is one.
    #[must_use]
    pub const fn as_float(self) -> Option<f32> {
        match self {
            Self::Float(value) => Some(value),
            _ => None,
        }
    }

    /// Read as an NPC reference, if it is one.
    #[must_use]
    pub const fn as_npc(self) -> Option<NpcId> {
        match self {
            Self::Npc(value) => Some(value),
            _ => None,
        }
    }

    /// Read as a position, if it is one.
    #[must_use]
    pub const fn as_position(self) -> Option<Vec3> {
        match self {
            Self::Position(value) => Some(value),
            _ => None,
        }
    }
}

/// Fixed-capacity key/value store, insertion-ordered.
#[derive(Debug, Clone, PartialEq)]
pub struct Blackboard {
    keys: [u16; BLACKBOARD_CAPACITY],
    values: [Option<Value>; BLACKBOARD_CAPACITY],
    len: usize,
}

impl Blackboard {
    /// An empty blackboard.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            keys: [0; BLACKBOARD_CAPACITY],
            values: [None; BLACKBOARD_CAPACITY],
            len: 0,
        }
    }

    /// How many slots are filled.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Whether nothing is stored.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Store a value, replacing any existing value for the same key.
    ///
    /// Returns `false` when the blackboard is full and the key is new, which
    /// lets a caller treat overflow as a behaviour bug rather than silently
    /// losing state.
    pub fn set(&mut self, key: BlackboardKey, value: Value) -> bool {
        if let Some(slot) = self
            .position(key)
            .and_then(|index| self.values.get_mut(index))
        {
            *slot = Some(value);
            return true;
        }
        if self.len >= BLACKBOARD_CAPACITY {
            return false;
        }
        if let (Some(key_slot), Some(value_slot)) =
            (self.keys.get_mut(self.len), self.values.get_mut(self.len))
        {
            *key_slot = key.get();
            *value_slot = Some(value);
            self.len += 1;
            return true;
        }
        false
    }

    /// Read a value.
    #[must_use]
    pub fn get(&self, key: BlackboardKey) -> Option<Value> {
        self.position(key)
            .and_then(|index| self.values.get(index).copied().flatten())
    }

    /// Remove a value.
    pub fn remove(&mut self, key: BlackboardKey) -> Option<Value> {
        let index = self.position(key)?;
        let removed = self.values.get(index).copied().flatten()?;
        // Preserve insertion order by shifting the tail down; the array is tiny
        // and ordering keeps debug output stable. `index < len` here, so the
        // range is always in bounds.
        self.keys.copy_within(index + 1..self.len, index);
        self.values.copy_within(index + 1..self.len, index);
        self.len -= 1;
        if let Some(slot) = self.values.get_mut(self.len) {
            *slot = None;
        }
        Some(removed)
    }

    /// Empty the blackboard.
    pub fn clear(&mut self) {
        self.values.fill(None);
        self.len = 0;
    }

    /// Iterate over the stored pairs in insertion order.
    pub fn iter(&self) -> impl Iterator<Item = (BlackboardKey, Value)> + '_ {
        (0..self.len).filter_map(move |index| {
            let key = self.keys.get(index).copied()?;
            let value = self.values.get(index).copied().flatten()?;
            Some((BlackboardKey::new(key), value))
        })
    }

    fn position(&self, key: BlackboardKey) -> Option<usize> {
        (0..self.len).find(|index| self.keys.get(*index).copied() == Some(key.get()))
    }
}

impl Default for Blackboard {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::num;

    const A: BlackboardKey = BlackboardKey::new(1);
    const B: BlackboardKey = BlackboardKey::new(2);

    #[test]
    fn stores_and_reads_back() {
        let mut board = Blackboard::new();
        assert!(board.set(A, Value::Bool(true)));
        assert_eq!(board.get(A).and_then(Value::as_bool), Some(true));
        assert_eq!(board.len(), 1);
    }

    #[test]
    fn overwriting_a_key_does_not_grow_the_board() {
        let mut board = Blackboard::new();
        board.set(A, Value::Int(1));
        board.set(A, Value::Int(2));
        assert_eq!(board.len(), 1);
        assert_eq!(board.get(A).and_then(Value::as_int), Some(2));
    }

    #[test]
    fn overflow_is_reported_not_silent() {
        let mut board = Blackboard::new();
        for index in 0..BLACKBOARD_CAPACITY {
            assert!(board.set(
                BlackboardKey::new(num::usize_to_u16(index)),
                Value::Int(num::usize_to_i64(index))
            ));
        }
        assert!(!board.set(BlackboardKey::new(999), Value::Int(0)));
        assert_eq!(board.len(), BLACKBOARD_CAPACITY);
    }

    #[test]
    fn removal_preserves_the_order_of_the_rest() {
        let mut board = Blackboard::new();
        board.set(A, Value::Int(1));
        board.set(B, Value::Int(2));
        assert_eq!(board.remove(A).and_then(Value::as_int), Some(1));
        assert_eq!(board.len(), 1);
        let pairs: Vec<(u16, i64)> = board
            .iter()
            .filter_map(|(key, value)| Some((key.get(), value.as_int()?)))
            .collect();
        assert_eq!(pairs, vec![(B.get(), 2)]);
        assert!(board.get(A).is_none());
    }

    #[test]
    fn clear_empties_without_leaking() {
        let mut board = Blackboard::new();
        board.set(A, Value::Bool(true));
        board.clear();
        assert!(board.is_empty());
        assert!(board.get(A).is_none());
    }

    #[test]
    fn typed_accessors_reject_the_wrong_variant() {
        assert!(Value::Bool(true).as_int().is_none());
        assert!(Value::Float(1.0).as_bool().is_none());
        assert_eq!(
            Value::Position(Vec3::new(1.0, 2.0, 3.0)).as_position(),
            Some(Vec3::new(1.0, 2.0, 3.0))
        );
    }
}
