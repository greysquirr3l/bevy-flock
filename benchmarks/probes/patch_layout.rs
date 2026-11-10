//! A standalone probe, not a cargo example: it models the grid's layout without depending on the crate, so the two
//! candidate patch shapes can be priced before either is written into the hot type.
//!
//! ```text
//! rustc -O benchmarks/probes/patch_layout.rs -o /tmp/patch_layout && /tmp/patch_layout
//! ```
//!
//! Read it with ADR 0064's step 2 section and the "Why a flat layout cannot be patched" section of
//! `docs/dev/tuning/performance.md`. Nothing here is a benchmark of the crate: it is a benchmark of two data layouts,
//! single-threaded, with the shapes the grid produces (one body per four bytes of order, uniform cells, the counts a
//! uniform scatter gives).

//! Prices three ways to absorb bodies that moved between cells: a full rebuild, patching a flat CSR
//! layout, and patching a slotted layout that carries per-cell slack.
//!
//! Shapes mirror the real grid: one body per 4 bytes of order, uniform cells, and the cell counts a
//! uniform scatter produces. Single-threaded throughout, because a patch of k bodies is not worth a
//! dispatch and every candidate would pay the same one.

use std::time::Instant;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 32) as u32
    }
    fn below(&mut self, n: u32) -> u32 {
        if n == 0 {
            0
        } else {
            self.next() % n
        }
    }
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v.get(v.len() / 2).copied().unwrap_or(0.0)
}

/// Bodies laid out in cells, either flat (no slack) or slotted (slack per cell).
struct Layout {
    cells: usize,
    /// Start of each cell's region; `start[cells]` is the end of the last region.
    start: Vec<u32>,
    /// Occupancy: the flat layout has `len[c] == start[c + 1] - start[c]`; the slotted one does not.
    len: Vec<u32>,
    /// Body indices (into `cell_of`), grouped by cell and sorted by index inside a cell.
    order: Vec<u32>,
    /// The cell each body sits in.
    cell_of: Vec<u32>,
}

impl Layout {
    fn build(cells: usize, bodies: usize, scatter: &[u32], slotted: bool) -> Self {
        let mut counts = vec![0_u32; cells];
        for cell in scatter {
            if let Some(slot) = counts.get_mut(*cell as usize) {
                *slot += 1;
            }
        }
        let mut start = vec![0_u32; cells + 1];
        let mut running = 0_u32;
        for cell in 0..cells {
            start[cell] = running;
            let count = counts.get(cell).copied().unwrap_or(0);
            // Twenty-five percent slack plus a floor, so a cell that gains a body has somewhere to put it.
            let room = if slotted { count + count / 4 + 4 } else { count };
            running += room;
        }
        start[cells] = running;
        let mut len = vec![0_u32; cells];
        let mut order = vec![0_u32; running as usize + 8];
        let mut cursor: Vec<u32> = start.iter().take(cells).copied().collect();
        for (body, cell) in scatter.iter().enumerate() {
            let c = *cell as usize;
            let at = cursor.get(c).copied().unwrap_or(0);
            if let Some(slot) = order.get_mut(at as usize) {
                *slot = body as u32;
            }
            if let Some(slot) = cursor.get_mut(c) {
                *slot += 1;
            }
            if let Some(slot) = len.get_mut(c) {
                *slot += 1;
            }
        }
        Self { cells, start, len, order, cell_of: scatter.to_vec() }
    }

    fn bytes(&self) -> usize {
        (self.start.len() + self.len.len() + self.order.len() + self.cell_of.len()) * 4
    }

    /// Patch a flat layout: shift the used tail for each removal and each insert, then repair every offset.
    ///
    /// This is the shape the grid uses today. Bodies are contiguous, so a body arriving in cell 7 moves every body
    /// after it, and the offsets that follow have to be told.
    fn patch_flat(&mut self, movers: &[(u32, u32)]) -> usize {
        let mut shifted = 0_usize;
        for (body, new_cell) in movers {
            let old = self.cell_of.get(*body as usize).copied().unwrap_or(0);
            if old == *new_cell {
                continue;
            }
            let used = self.start.get(self.cells).copied().unwrap_or(0) as usize;
            // Remove: the body leaves its run, and everything after it moves down one slot.
            let start = self.start.get(old as usize).copied().unwrap_or(0) as usize;
            let end = self.start.get(old as usize + 1).copied().unwrap_or(0) as usize;
            if let Some(at) = self
                .order
                .get(start..end)
                .and_then(|run| run.iter().position(|v| v == body))
            {
                let from = start + at;
                self.order.copy_within(from + 1..used, from);
                shifted += 1;
            }
            // Every offset after the removal point is one lower, the end of the table included.
            for cell in old as usize + 1..=self.cells {
                if let Some(slot) = self.start.get_mut(cell) {
                    *slot = slot.saturating_sub(1);
                }
            }
            // Insert at the id-sorted position in the new run, moving everything after it up one slot.
            let used = self.start.get(self.cells).copied().unwrap_or(0) as usize;
            let start = self.start.get(*new_cell as usize).copied().unwrap_or(0) as usize;
            let end = self.start.get(*new_cell as usize + 1).copied().unwrap_or(0) as usize;
            let at = self
                .order
                .get(start..end)
                .map_or(0, |run| run.partition_point(|v| v < body));
            let to = start + at;
            self.order.copy_within(to..used, to + 1);
            if let Some(slot) = self.order.get_mut(to) {
                *slot = *body;
            }
            for cell in *new_cell as usize + 1..=self.cells {
                if let Some(slot) = self.start.get_mut(cell) {
                    *slot += 1;
                }
            }
            if let Some(slot) = self.cell_of.get_mut(*body as usize) {
                *slot = *new_cell;
            }
        }
        shifted
    }

    /// Patch a slotted layout: move the body inside its two runs, offsets untouched.
    fn patch_slotted(&mut self, movers: &[(u32, u32)]) -> (usize, usize) {
        let mut moved = 0_usize;
        let mut overflowed = 0_usize;
        for (body, new_cell) in movers {
            let old = self.cell_of.get(*body as usize).copied().unwrap_or(0);
            if old == *new_cell {
                continue;
            }
            let start = self.start.get(old as usize).copied().unwrap_or(0) as usize;
            let len = self.len.get(old as usize).copied().unwrap_or(0) as usize;
            if let Some(at) = self.order.get(start..start + len).and_then(|run| run.iter().position(|v| v == body)) {
                let from = start + at;
                self.order.copy_within(from + 1..start + len, from);
                if let Some(slot) = self.len.get_mut(old as usize) {
                    *slot -= 1;
                }
            }
            let start = self.start.get(*new_cell as usize).copied().unwrap_or(0) as usize;
            let cap = self
                .start
                .get(*new_cell as usize + 1)
                .copied()
                .unwrap_or(0) as usize
                - start;
            let len = self.len.get(*new_cell as usize).copied().unwrap_or(0) as usize;
            if len >= cap {
                overflowed += 1;
                continue;
            }
            let at = self
                .order
                .get(start..start + len)
                .map_or(0, |run| run.partition_point(|v| v < body));
            let to = start + at;
            self.order.copy_within(to..start + len, to + 1);
            if let Some(slot) = self.order.get_mut(to) {
                *slot = *body;
            }
            if let Some(slot) = self.len.get_mut(*new_cell as usize) {
                *slot += 1;
            }
            if let Some(slot) = self.cell_of.get_mut(*body as usize) {
                *slot = *new_cell;
            }
            moved += 1;
        }
        (moved, overflowed)
    }
}

fn movers(rng: &mut Rng, cells: usize, bodies: usize, k: usize) -> Vec<(u32, u32)> {
    (0..k)
        .map(|_| {
            let body = rng.below(bodies as u32);
            let mut cell = rng.below(cells as u32);
            if cell == body % cells as u32 {
                cell = (cell + 1) % cells as u32;
            }
            (body, cell)
        })
        .collect()
}

fn main() {
    for &bodies in &[100_000_usize, 1_000_000] {
        let cells = if bodies >= 1_000_000 { 421_876 } else { 42_188 };
        let mut rng = Rng(0x2545_F491_4F6C_DD1D);
        let scatter: Vec<u32> = (0..bodies).map(|_| rng.below(cells as u32)).collect();

        let flat = Layout::build(cells, bodies, &scatter, false);
        let slot = Layout::build(cells, bodies, &scatter, true);
        let flat_bytes = flat.bytes();
        let slot_bytes = slot.bytes();
        drop(flat);
        drop(slot);

        let mut rebuild = Vec::new();
        for _ in 0..5 {
            let start = Instant::now();
            let built = Layout::build(cells, bodies, &scatter, false);
            std::hint::black_box(built.order.len());
            rebuild.push(start.elapsed().as_secs_f64() * 1_000.0);
        }

        println!(
            "\n== {bodies} bodies, {cells} cells, order {} MB flat / {} MB slotted",
            flat_bytes / 1_048_576,
            slot_bytes / 1_048_576
        );
        println!("{:>8} {:>14} {:>14}", "k", "flat patch ms", "slot patch ms");
        let mut base = Layout::build(cells, bodies, &scatter, false);
        let mut slotted = Layout::build(cells, bodies, &scatter, true);
        for &k in &[1_usize, 64, 1024] {
            let batch = movers(&mut rng, cells, bodies, k);
            let mut flat_samples = Vec::new();
            for _ in 0..3 {
                let start = Instant::now();
                base.patch_flat(&batch);
                flat_samples.push(start.elapsed().as_secs_f64() * 1_000.0);
                // Put the bodies back so the next sample starts from the same layout cost.
                let undo: Vec<(u32, u32)> = batch
                    .iter()
                    .map(|(body, _)| (*body, scatter.get(*body as usize).copied().unwrap_or(0)))
                    .collect();
                base.patch_flat(&undo);
            }
            let mut slot_samples = Vec::new();
            let mut overflow = 0_usize;
            for _ in 0..3 {
                let start = Instant::now();
                let (_, over) = slotted.patch_slotted(&batch);
                overflow += over;
                slot_samples.push(start.elapsed().as_secs_f64() * 1_000.0);
            }
            println!(
                "{k:>8} {:>14.3} {:>14.3}",
                median(flat_samples),
                median(slot_samples)
            );
            if overflow > 0 {
                println!("         (slotted overflowed {overflow} insertion(s); needs a compaction)");
            }
        }
        println!("{:<8} {:>14.3}", "rebuild", median(rebuild));
    }
}
