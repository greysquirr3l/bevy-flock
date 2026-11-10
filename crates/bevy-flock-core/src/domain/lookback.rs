//! Block bases without a barrier: decoupled look-back over published aggregates.
//!
//! The spatial rebuild splits the cell table into blocks and needs, for every block, the number of bodies that sit in
//! every block before it. Doing that with a scan is a `log n` sequence of dependencies; doing it with a barrier per phase
//! is a global rendezvous. Decoupled look-back (Du Levandowski, and the technique behind CUB's `DeviceScan`) needs
//! neither: each block publishes its own total as soon as it has it, and a block that needs its base walks backwards
//! over its predecessors, summing published totals, until it reaches one that has already published an *inclusive*
//! prefix. Then it publishes its own inclusive prefix, so the next block stops after one hop.
//!
//! ```text
//!         block:  0        1        2        3
//!   aggregate:  [ 7 ]    [ 3 ]    [ 9 ]    [ 1 ]
//!   inclusive:  [ 7 ]    [10 ]    [19 ]    [20 ]
//!   base of 3 = inclusive(2) = 19, which resolving 3 walks back to find
//! ```
//!
//! Three properties matter to this crate beyond the speed:
//!
//! - **Determinism.** Every value is an exact integer sum. Publishing order cannot change any of them, so the cell
//!   table the rebuild produces is byte-identical to the serial scan for any thread schedule. That is what
//!   `tests/spatial_parity` (and the byte-equality matrix in `spatial.rs`) asserts.
//! - **No allocation.** The slots are sized once, when the grid is built, and reused. Resolving allocates nothing.
//! - **No `unsafe`.** The shared state is a `&[AtomicU32]`; each block declares itself either not ready, an aggregate,
//!   or an inclusive prefix with two tag bits, so a slot conveys its own state and no separate flag array is needed.
//!
//! A block's value field is 30 bits, so a single block may hold up to 1,073,741,823 bodies. The crate's
//! [`MAX_POPULATION`](crate::error::MAX_POPULATION) is ten million, an order of magnitude of headroom; a total that
//! exceeds the field saturates on publish rather than wrapping, which keeps a corrupt number from silently relocating
//! every window after it.

use core::sync::atomic::{AtomicU32, Ordering};

/// The state of one block's slot, occupying the top two bits of the word.
///
/// Three states, not two, because a block can know its own total long before it knows its prefix: publishing the
/// aggregate first is what lets the block behind it start summing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    /// Nothing has been published yet; a reader must wait or walk further back.
    NotReady,
    /// The block's own total is published, but not the sum of everything up to it.
    Aggregate,
    /// The inclusive prefix through this block is published; a walk back can stop here.
    Inclusive,
}

/// Bits 30 and 31 carry the state, bits 0 to 29 the value.
const VALUE_BITS: u32 = 30;
/// Mask for the value field.
const VALUE_MASK: u32 = (1 << VALUE_BITS) - 1;
/// The largest value a slot can hold.
pub const MAX_SLOT_VALUE: u32 = VALUE_MASK;
/// `Aggregate` tag.
const TAG_AGGREGATE: u32 = 1 << VALUE_BITS;
/// `Inclusive` tag.
const TAG_INCLUSIVE: u32 = 2 << VALUE_BITS;

/// Spins before a waiting block yields its core.
///
/// A block's count is microseconds, so the usual wait is shorter than this. The yield exists for the case where more
/// workers run than there are cores, where a pure spin would keep the thread that owes an aggregate from being
/// scheduled at all.
const SPINS_BEFORE_YIELD: u32 = 64;

/// Split a packed word into its state and its value.
fn unpack(packed: u32) -> (State, u32) {
    let value = packed & VALUE_MASK;
    let state = match packed & !VALUE_MASK {
        TAG_AGGREGATE => State::Aggregate,
        TAG_INCLUSIVE => State::Inclusive,
        _ => State::NotReady,
    };
    (state, value)
}

/// Pack a value with its state tag, saturating a value that does not fit the field.
fn pack(state: State, value: u32) -> u32 {
    let tag = match state {
        State::NotReady => return 0,
        State::Aggregate => TAG_AGGREGATE,
        State::Inclusive => TAG_INCLUSIVE,
    };
    tag | value.min(VALUE_MASK)
}

/// Per-block totals and prefixes for one rebuild, shared without a lock.
///
/// Owned by the grid, sized once, reset per rebuild. Every method takes `&self`; the arrays are atomics, so a worker
/// that has just counted its blocks publishes them and a worker that needs a base resolves it, with no borrow of the
/// grid's other buffers involved.
#[derive(Debug)]
pub struct BlockPrefix {
    /// One packed word per block.
    slots: Vec<AtomicU32>,
    /// How many blocks the slots describe; kept because `slots` may be longer than the live block count.
    blocks: usize,
}

impl BlockPrefix {
    /// Allocate slots for `blocks` blocks, all `NotReady`.
    ///
    /// This is the only allocating call, and the grid makes it once and then reuses the buffer through
    /// [`Self::reset`], so a steady-frame rebuild never reaches the allocator.
    #[must_use]
    pub fn new(blocks: usize) -> Self {
        Self {
            slots: (0..blocks).map(|_| AtomicU32::new(0)).collect(),
            blocks,
        }
    }

    /// Re-arm the buffer for `blocks` blocks, without giving its memory back.
    ///
    /// Stores `NotReady` into every live slot rather than reallocating, which is a `memset` of `4 × blocks` bytes: 412
    /// bytes at the default 103 blocks, against the 1.7 MB of cell table the rebuild is already touching.
    pub fn reset(&mut self, blocks: usize) {
        if self.slots.len() < blocks {
            self.slots.resize_with(blocks, || AtomicU32::new(0));
        }
        for slot in self.slots.iter().take(blocks) {
            slot.store(0, Ordering::Relaxed);
        }
        self.blocks = blocks;
    }

    /// The number of live blocks.
    #[must_use]
    pub fn len(&self) -> usize {
        self.blocks
    }

    /// Whether there are no live blocks.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.blocks == 0
    }

    /// Publish a block's own total, which is what unblocks the walk back from behind it.
    ///
    /// `Relaxed` is enough on its own: the value carries no dependency on other memory. The `Acquire` on the reading
    /// side is what makes a published aggregate visible together with anything the publishing worker wrote before it.
    pub fn publish_total(&self, block: usize, total: u32) {
        if let Some(slot) = self.slots.get(block) {
            slot.store(pack(State::Aggregate, total), Ordering::Relaxed);
        }
    }

    /// The number of bodies in every block before `block`, resolving and publishing as needed.
    ///
    /// This is the call a worker makes before scanning its own first block, and it is where the barrier used to be.
    pub fn base_of(&self, block: usize) -> u32 {
        if block == 0 {
            return 0;
        }
        self.resolve(block - 1)
    }

    /// The inclusive prefix through `block`, resolving it if no worker has yet.
    ///
    /// Walks backwards over predecessors that have published only aggregates, summing as it goes, and stops at the
    /// first inclusive prefix. Every step is a data dependency on one word, not a rendezvous with the other workers,
    /// which is the whole point: a worker that arrives early does a little more summing, and one that arrives late does
    /// none.
    pub fn resolve(&self, block: usize) -> u32 {
        if block >= self.blocks {
            return 0;
        }
        // `u64` for the accumulator so a chain of maximal values cannot wrap before it is saturated on publish.
        let mut sum = 0_u64;
        let mut index = block;
        let mut spins = 0_u32;
        loop {
            let Some(slot) = self.slots.get(index) else {
                return 0;
            };
            let (state, value) = unpack(slot.load(Ordering::Acquire));
            match state {
                State::Inclusive => {
                    sum = sum.saturating_add(u64::from(value));
                    break;
                }
                State::Aggregate => {
                    sum = sum.saturating_add(u64::from(value));
                    if index == 0 {
                        break;
                    }
                    index -= 1;
                }
                // The predecessor has counted nothing yet. Spin briefly, then yield: a pure spin deadlocks the moment
                // more workers run than there are cores, because the worker that owes the aggregate never gets
                // scheduled. Found the hard way, by a test using eight threads on six cores.
                State::NotReady => {
                    spins = spins.saturating_add(1);
                    if spins < SPINS_BEFORE_YIELD {
                        core::hint::spin_loop();
                    } else {
                        std::thread::yield_now();
                    }
                }
            }
        }
        let inclusive = u32::try_from(sum)
            .unwrap_or(MAX_SLOT_VALUE)
            .min(MAX_SLOT_VALUE);
        if let Some(slot) = self.slots.get(block) {
            slot.store(pack(State::Inclusive, inclusive), Ordering::Release);
        }
        inclusive
    }

    /// The population the blocks describe, resolving the last block if needed.
    ///
    /// Zero when there are no blocks, which is the empty grid rather than an error: a rebuild with nothing to place has
    /// no total to speak of.
    #[must_use]
    pub fn total(&self) -> u32 {
        match self.blocks.checked_sub(1) {
            Some(last) => self.resolve(last),
            None => 0,
        }
    }

    /// The state of one slot, for tests that need to see a publish and a resolve apart.
    #[cfg(test)]
    fn state_of(&self, block: usize) -> State {
        self.slots
            .get(block)
            .map(|slot| unpack(slot.load(Ordering::Acquire)).0)
            .unwrap_or(State::NotReady)
    }
}

#[cfg(test)]
mod tests {
    use super::{BlockPrefix, MAX_SLOT_VALUE, State};
    use std::sync::mpsc;
    use std::time::Duration;

    /// A deterministic sequence, so a failing case is reproducible from its seed.
    fn totals(blocks: usize, seed: u64) -> Vec<u32> {
        let mut state = seed | 1;
        (0..blocks)
            .map(|_| {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1);
                u32::try_from((state >> 33) % 5_000).unwrap_or(0)
            })
            .collect()
    }

    fn serial_prefix(totals: &[u32]) -> Vec<u32> {
        let mut running = 0_u32;
        totals
            .iter()
            .map(|total| {
                running = running.saturating_add(*total);
                running
            })
            .collect()
    }

    /// A buffer whose blocks have all published their own totals.
    fn published(totals: &[u32]) -> BlockPrefix {
        let buffer = BlockPrefix::new(totals.len());
        for (block, total) in totals.iter().enumerate() {
            buffer.publish_total(block, *total);
        }
        buffer
    }

    #[test]
    fn every_block_resolves_to_its_serial_prefix() {
        for blocks in [1_usize, 2, 3, 7, 64, 103, 1024] {
            let totals = totals(blocks, blocks as u64);
            let expected = serial_prefix(&totals);
            let buffer = published(&totals);
            for (block, want) in expected.iter().enumerate() {
                assert_eq!(buffer.resolve(block), *want, "blocks {blocks} at {block}");
            }
            assert_eq!(buffer.total(), expected.last().copied().unwrap_or(0));
        }
    }

    #[test]
    fn a_base_is_the_prefix_of_the_block_before() {
        let totals = totals(9, 42);
        let expected = serial_prefix(&totals);
        let buffer = published(&totals);
        // `base_of(k)` is the exclusive prefix of block `k`: the sum of every block before it.
        assert_eq!(buffer.base_of(0), 0);
        for block in 1..=9 {
            let want = expected.get(block - 1).copied().unwrap_or(0);
            assert_eq!(buffer.base_of(block), want, "base of {block}");
        }
    }

    #[test]
    fn a_zero_total_still_publishes_a_visible_aggregate() {
        // Zero is also the not-ready word, so an aggregate of zero has to be distinguishable from silence.
        let buffer = BlockPrefix::new(2);
        assert_eq!(buffer.state_of(0), State::NotReady);
        buffer.publish_total(0, 0);
        assert_eq!(buffer.state_of(0), State::Aggregate);
        buffer.publish_total(1, 5);
        assert_eq!(buffer.resolve(1), 5);
    }

    #[test]
    fn resolving_waits_for_a_predecessor_and_then_gets_the_right_base() {
        let buffer = BlockPrefix::new(5);
        buffer.publish_total(0, 4);
        buffer.publish_total(1, 6);
        let (tx, rx) = mpsc::channel();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                // Blocks 2, 3 and 4 have not counted yet, so this must wait inside `resolve`.
                let resolved = buffer.resolve(4);
                let _ = tx.send(resolved);
            });
            // The predecessor arrives late, which is the schedule the barrier used to paper over.
            std::thread::sleep(Duration::from_millis(20));
            buffer.publish_total(2, 1);
            buffer.publish_total(3, 2);
            buffer.publish_total(4, 3);
        });
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)), Ok(16));
    }

    #[test]
    fn an_out_of_range_block_resolves_to_zero_rather_than_panicking() {
        let buffer = BlockPrefix::new(4);
        assert_eq!(buffer.resolve(4), 0);
        assert_eq!(buffer.base_of(99), buffer.resolve(98));
        assert_eq!(buffer.len(), 4);
        assert!(!buffer.is_empty());
        assert!(BlockPrefix::new(0).is_empty());
        assert_eq!(BlockPrefix::new(0).total(), 0);
    }

    #[test]
    fn a_value_wider_than_the_field_saturates_instead_of_wrapping() {
        let buffer = BlockPrefix::new(2);
        buffer.publish_total(0, MAX_SLOT_VALUE);
        buffer.publish_total(1, MAX_SLOT_VALUE);
        assert_eq!(buffer.resolve(1), MAX_SLOT_VALUE);
    }

    #[test]
    fn concurrent_workers_agree_with_the_serial_prefix_whatever_the_order() {
        // The property that matters: publishing order cannot change a value, so the result is schedule independent.
        for seed in [1_u64, 17, 4_242, 99_991] {
            let blocks = 257;
            let totals = totals(blocks, seed);
            let expected = serial_prefix(&totals);
            let buffer = BlockPrefix::new(blocks);
            let expected = &expected;
            let per = blocks.div_ceil(8).max(1);
            std::thread::scope(|scope| {
                for worker in 0..8 {
                    let chunk = totals.chunks(per).nth(worker).unwrap_or(&[]);
                    let offset = worker * per;
                    let buffer = &buffer;
                    scope.spawn(move || {
                        for (index, total) in chunk.iter().enumerate() {
                            buffer.publish_total(offset + index, *total);
                        }
                        // Resolving in the same pass, in an order the scheduler picks, is the race being tested.
                        for (index, _) in chunk.iter().enumerate() {
                            let block = offset + index;
                            let got = buffer.resolve(block);
                            let want = expected.get(block).copied().unwrap_or(0);
                            assert_eq!(got, want, "seed {seed} block {block}");
                        }
                    });
                }
            });
            for (block, want) in expected.iter().enumerate() {
                assert_eq!(
                    buffer.resolve(block),
                    *want,
                    "seed {seed} after join at {block}"
                );
            }
        }
    }

    #[test]
    fn reset_rearms_the_buffer_and_keeps_its_memory() {
        let mut buffer = BlockPrefix::new(8);
        buffer.publish_total(3, 11);
        assert_eq!(buffer.state_of(3), State::Aggregate);
        buffer.reset(8);
        assert_eq!(buffer.state_of(3), State::NotReady);
        buffer.reset(16);
        assert_eq!(buffer.len(), 16);
        assert_eq!(buffer.state_of(3), State::NotReady);
        buffer.publish_total(15, 2);
        assert_eq!(buffer.state_of(15), State::Aggregate);
        for block in 0..15 {
            buffer.publish_total(block, 1);
        }
        assert_eq!(buffer.resolve(15), 17);
    }
}
