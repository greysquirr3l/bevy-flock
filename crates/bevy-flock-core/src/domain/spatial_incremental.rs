//! The incremental rebuild's harness: an oracle, two scripts, and the invariants in between.
//!
//! ADR 0064 fixes the method before the code exists, which is the point: the oracle is the existing full rebuild, run from
//! the same positions, compared byte for byte on the three arrays a query reads. Nothing here models what the table should
//! look like, because a model would need its own proof and the full path already has a byte-equality matrix behind it.
//!
//! The module is a child of `spatial` so it can read the private buffers. That is deliberate: the alternative is three new
//! accessors on the public API, widening the surface of a hot type to serve its tests.

use super::*;
use crate::testkit::OrDie;

/// A configuration with room for the bodies these tests use and enough cells for several blocks.
const EXTENT: f32 = 512.0;

/// The volume the tests scatter bodies through, leaving a margin so a forced move stays inside it.
const FIELD: f32 = 500.0;

fn config(cell_size: f32) -> SpatialGridConfig {
    SpatialGridConfig {
        min: Vec3::ZERO,
        max: Vec3::splat(EXTENT),
        cell_size,
        max_cells: 1 << 20,
    }
}

/// A small deterministic generator, so a failure replays from the seed alone.
struct Rng(u64);

impl Rng {
    fn next_u32(&mut self) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        num::u64_to_low_u32(self.0 >> 33)
    }

    fn unit(&mut self) -> f32 {
        num::u32_to_f32(self.next_u32()) / num::u32_to_f32(u32::MAX)
    }

    fn field(&mut self) -> f32 {
        self.unit() * FIELD
    }

    fn point(&mut self) -> Vec3 {
        Vec3::new(self.field(), self.field(), self.field())
    }
}

fn crowd(count: usize, seed: u64) -> Vec<GridEntry> {
    let mut rng = Rng(seed);
    let mut entries = Vec::with_capacity(count);
    for index in 0..count {
        entries.push(GridEntry {
            id: NpcId::unique(num::usize_to_u32(index)),
            position: rng.point(),
            velocity: Vec3::ZERO,
        });
    }
    entries
}

fn grid_with(entries: &[GridEntry], config: SpatialGridConfig, workers: usize) -> SpatialGrid {
    let mut grid = SpatialGrid::new(config).or_die().with_threads(workers);
    for entry in entries {
        grid.insert(*entry);
    }
    grid.build();
    grid
}

/// The oracle: the full rebuild ADR 0064 specifies, from the same positions.
fn oracle(entries: &[GridEntry], config: SpatialGridConfig, workers: usize) -> SpatialGrid {
    grid_with(entries, config, workers)
}

/// The table a query reads, compared against a full rebuild of the same positions.
///
/// Not byte for byte, and the reason is the point of the layout: a rebuild derives each cell's capacity from the count it
/// just measured, while a patch keeps the capacity the cell was given at the last rebuild and spends it. Both tables put
/// the same bodies in the same cells in the same order, and both satisfy the invariants, but their offsets differ wherever
/// the slack did. So this compares what a query can observe, cell by cell, and leaves the slack alone (ADR 0064).
fn assert_same_table(actual: &SpatialGrid, expected: &SpatialGrid, what: &str) {
    assert_eq!(
        actual.cells, expected.cells,
        "{what}: a body is in a different cell than a full rebuild puts it in"
    );
    for cell in 0..num::usize_to_u32(actual.cell_count()) {
        assert_eq!(
            actual.cell_window(cell),
            expected.cell_window(cell),
            "{what}: cell {cell}'s run differs from a full rebuild"
        );
    }
    let occupied: u32 = actual.cell_len.iter().copied().sum();
    assert_eq!(
        occupied,
        num::usize_to_u32(actual.len()),
        "{what}: the occupancy is not the population"
    );
    actual.check_invariants().or_die();
}

/// How many run boundaries a tick moved, which is what a borrow costs: one boundary per run it walks.
fn moved_starts(before: &[u32], after: &[u32]) -> usize {
    before
        .iter()
        .zip(after.iter())
        .filter(|(left, right)| left != right)
        .count()
}

#[test]
fn a_still_crowd_takes_the_fast_path_and_matches_a_full_rebuild() {
    let config = config(8.0);
    let entries = crowd(20_000, 0x5EED_1234);
    let mut grid = grid_with(&entries, config, 6);
    let reference = oracle(&entries, config, 6);
    assert_same_table(&grid, &reference, "after the first build");
    grid.check_invariants().or_die();

    for tick in 0..32 {
        let outcome = grid.build_incremental();
        assert_eq!(
            outcome,
            RebuildOutcome::Unchanged,
            "tick {tick}: a still crowd paid for a rebuild"
        );
        assert_same_table(&grid, &reference, "still crowd");
        grid.check_invariants().or_die();
    }
}

#[test]
fn one_crosser_per_tick_reports_one_and_matches_a_full_rebuild() {
    let config = config(8.0);
    let entries = crowd(4_000, 7);
    let mut grid = grid_with(&entries, config, 6);

    for tick in 0..24 {
        let index = tick % grid.len();
        if let Some(entry) = grid.entries_mut().get_mut(index) {
            // A move of 24 units across 8-unit cells crosses at least two boundaries and stays inside the field,
            // because every body starts below it and the step reverses near the top.
            entry.position.x = if entry.position.x < 256.0 {
                entry.position.x + 24.0
            } else {
                entry.position.x - 24.0
            };
        }
        let outcome = grid.build_incremental();
        assert_eq!(
            outcome,
            RebuildOutcome::Patched { changed: 1 },
            "tick {tick}: one body crossed, so exactly one change was expected"
        );
        let reference = oracle(grid.entries(), config, 6);
        assert_same_table(&grid, &reference, "one crosser");
        grid.check_invariants().or_die();
    }
}

#[test]
fn every_body_crossing_reports_the_population_and_matches_a_full_rebuild() {
    let config = config(8.0);
    let entries = crowd(3_000, 0x00B0_D1E5);
    let mut grid = grid_with(&entries, config, 6);
    // Every body leaves its cell for one cell, which is the move no slack rule can absorb: an even redistribution is
    // patchable now that a cell keeps room for a pair
    // (`a_wider_slack_absorbs_the_arrivals_the_default_refuses`), but a whole crowd arriving in a single run cannot be,
    // whatever the table was laid out with.
    let gathering = Vec3::new(260.0, 260.0, 260.0);
    for entry in grid.entries_mut() {
        entry.position = gathering;
    }
    let outcome = grid.build_incremental();
    assert_eq!(
        outcome,
        RebuildOutcome::Rebuilt {
            changed: grid.len(),
            cause: RebuildCause::NarrowRun,
        },
        "every body landed in one cell, so every body changed, and three thousand slots is further than \
         the borrow may walk"
    );
    let reference = oracle(grid.entries(), config, 6);
    assert_same_table(&grid, &reference, "every body crossing");
    grid.check_invariants().or_die();
}

#[test]
fn an_entry_list_that_changes_length_rebuilds_everything() {
    let config = config(8.0);
    let entries = crowd(1_500, 0x1CE);
    let mut grid = grid_with(&entries, config, 6);
    grid.insert(GridEntry {
        id: NpcId::unique(999_999),
        position: Vec3::splat(256.0),
        velocity: Vec3::ZERO,
    });
    let outcome = grid.build_incremental();
    assert_eq!(
        outcome,
        RebuildOutcome::Rebuilt {
            changed: grid.len(),
            cause: RebuildCause::Population,
        },
        "a longer entry list has no table to keep, so every body is new to it"
    );
    let reference = oracle(grid.entries(), config, 6);
    assert_same_table(&grid, &reference, "longer entry list");
    grid.check_invariants().or_die();
}

#[test]
fn the_fast_path_holds_at_every_worker_count() {
    let config = config(6.0);
    let entries = crowd(12_000, 0xA11CE);
    for workers in [1_usize, 2, 3, 8] {
        let mut grid = grid_with(&entries, config, workers);
        for tick in 0..6 {
            assert_eq!(
                grid.build_incremental(),
                RebuildOutcome::Unchanged,
                "workers {workers}, tick {tick}: a still crowd paid for a rebuild"
            );
        }
        let reference = oracle(&entries, config, workers);
        assert_same_table(&grid, &reference, "still crowd");
        grid.check_invariants().or_die();

        if let Some(entry) = grid.entries_mut().first_mut() {
            entry.position.y += 18.0;
        }
        assert_eq!(
            grid.build_incremental(),
            RebuildOutcome::Patched { changed: 1 },
            "workers {workers}: one body crossed, so exactly one change was expected"
        );
        let reference = oracle(grid.entries(), config, workers);
        assert_same_table(&grid, &reference, "one crosser");
        grid.check_invariants().or_die();
    }
}

#[test]
fn the_audit_catches_a_table_it_should_not_have_accepted() {
    let config = config(8.0);
    let entries = crowd(500, 0x00C0_FFEE);

    let mut doubled = grid_with(&entries, config, 2);
    assert_eq!(doubled.check_invariants(), Ok(()));
    // Into a run, not past the end: the slots between runs are slack, and a body index in slack is a body index nothing
    // reads.
    let populated = doubled
        .cell_len
        .iter()
        .position(|len| *len > 0)
        .unwrap_or(0);
    let start = doubled.cell_start.get(populated).copied().unwrap_or(0) as usize;
    if let Some(slot) = doubled.order.get_mut(start) {
        *slot = u32::MAX;
    }
    assert_eq!(
        doubled.check_invariants(),
        Err(GridInvariant::NotAPermutation),
        "an order naming a body that does not exist is not a permutation"
    );

    let mut misplaced = grid_with(&entries, config, 2);
    if let Some(slot) = misplaced.cell_cursor.first_mut() {
        *slot = slot.saturating_add(1);
    }
    assert_eq!(
        misplaced.check_invariants(),
        Err(GridInvariant::CursorMisplaced),
        "a cursor past its run's end would overwrite the next cell's bodies"
    );

    let mut short = grid_with(&entries, config, 2);
    let last = short.cell_start.len().saturating_sub(1);
    if let Some(slot) = short.cell_start.get_mut(last) {
        *slot = slot.saturating_sub(1);
    }
    assert_eq!(
        short.check_invariants(),
        Err(GridInvariant::PopulationMismatch),
        "runs that stop one body short do not tile the order"
    );
}

#[test]
fn a_rebuild_over_an_empty_grid_is_stable() {
    let config = config(8.0);
    let mut grid = SpatialGrid::new(config).or_die().with_threads(4);
    // The first call builds, even though there is nothing to place: the constructor sizes the window table, and an
    // "already built" verdict read from buffer lengths alone would skip the cursor that has never been written.
    assert_eq!(
        grid.build_incremental(),
        RebuildOutcome::Rebuilt {
            changed: 0,
            cause: RebuildCause::Population
        },
        "a grid that has never been built has a table to prepare, bodies or not"
    );
    grid.check_invariants().or_die();
    assert_eq!(
        grid.build_incremental(),
        RebuildOutcome::Unchanged,
        "an empty grid that has been built has nothing left to do"
    );
    grid.check_invariants().or_die();
    grid.insert(GridEntry {
        id: NpcId::unique(1),
        position: Vec3::splat(10.0),
        velocity: Vec3::ZERO,
    });
    assert_eq!(
        grid.build_incremental(),
        RebuildOutcome::Rebuilt {
            changed: 1,
            cause: RebuildCause::Population
        },
        "the first body in an empty grid is a change"
    );
    grid.check_invariants().or_die();
    let window = grid.cell_window(grid.cells.first().copied().unwrap_or(0));
    assert_eq!(window.len(), 1, "the one body is in exactly one cell");
}
#[test]
fn swapping_bodies_between_cells_is_patched_and_matches_a_full_rebuild() {
    // The patch's own arithmetic, in the regime it is for: a crowd dense enough that each cell owns several bodies, and a
    // tick where bodies arrive in cells that are losing bodies at the same time. A swap keeps every cell's occupancy
    // fixed, so the slack a patch spends is exactly the slot the departure freed, and the outcome is deterministic.
    let config = config(8.0);
    let entries = crowd(6_000, 0x5151);
    let mut grid = grid_with(&entries, config, 6);
    let partner = 301;
    for tick in 0..8 {
        let len = grid.len();
        let mut expected = 0_usize;
        for index in (0..len).step_by(2).take(200) {
            let other = index.saturating_add(partner) % len.max(1);
            if other <= index {
                continue;
            }
            let (first, second) = {
                let entries = grid.entries();
                let (Some(first), Some(second)) = (entries.get(index), entries.get(other)) else {
                    continue;
                };
                (*first, *second)
            };
            let before = (grid.cell_of(first.position), grid.cell_of(second.position));
            if before.0 == before.1 {
                continue;
            }
            expected = expected.saturating_add(2);
            if let Some(entry) = grid.entries_mut().get_mut(index) {
                entry.position = second.position;
            }
            if let Some(entry) = grid.entries_mut().get_mut(other) {
                entry.position = first.position;
            }
        }
        assert!(expected > 0, "tick {tick}: the script swapped nobody");
        assert_eq!(
            grid.build_incremental(),
            RebuildOutcome::Patched { changed: expected },
            "tick {tick}: {expected} bodies changed cell, so a patch of that size was expected"
        );
        let reference = oracle(grid.entries(), config, 6);
        assert_same_table(&grid, &reference, "swapped bodies");
    }
}

#[test]
fn a_flood_of_movers_takes_the_rebuild_instead() {
    // Past the patch's bound the list of movers is not even kept, so a tick that moves everyone has to rebuild. The bound
    // exists because the patch's bookkeeping is linear in the moves while the rebuild is not.
    let config = config(8.0);
    let entries = crowd(5_000, 0xF100D);
    assert!(
        entries.len() > PATCH_MAX_MOVERS,
        "the case under test is above the bound"
    );
    let mut grid = grid_with(&entries, config, 6);
    for entry in grid.entries_mut() {
        entry.position.y = if entry.position.y < 256.0 {
            entry.position.y + 32.0
        } else {
            entry.position.y - 32.0
        };
    }
    assert_eq!(
        grid.build_incremental(),
        RebuildOutcome::Rebuilt {
            changed: grid.len(),
            cause: RebuildCause::TooManyMovers,
        },
        "every body moved four cells, which is more than a patch may carry"
    );
    let reference = oracle(grid.entries(), config, 6);
    assert_same_table(&grid, &reference, "flood of movers");
}

#[test]
fn a_cell_without_slack_borrows_the_slot_from_the_run_after_it() {
    // The failure mode the borrow exists for: a run with no room left, which is what a clustered arrival makes. Before the
    // budget this tick rebuilt; now the arrival takes a slot from the next run's spare. What has to stay true is that the
    // table is what a rebuild would have produced, cell by cell, because a segment inside a run is what the borrow moves.
    let config = config(16.0);
    let mut grid = SpatialGrid::new(config).or_die().with_threads(4);
    let crowded = Vec3::new(64.0, 64.0, 64.0);
    let elsewhere = Vec3::new(400.0, 400.0, 400.0);
    for index in 0..200_u32 {
        grid.insert(GridEntry {
            id: NpcId::unique(index),
            position: crowded,
            velocity: Vec3::ZERO,
        });
    }
    grid.build();
    grid.check_invariants().or_die();
    // Sixty of them arrive in a cell that holds nobody, and therefore owns one slot.
    for index in 0..60_u32 {
        if let Some(entry) = grid.entries_mut().get_mut(index as usize) {
            entry.position = elsewhere;
        }
    }
    let before = grid.cell_start.clone();
    assert_eq!(
        grid.build_incremental(),
        RebuildOutcome::Patched { changed: 60 },
        "a run with one spare slot borrows the other fifty-nine"
    );
    let arrival = usize::try_from(grid.cell_of(elsewhere)).unwrap_or(0);
    let grown = grid
        .cell_start
        .get(arrival.saturating_add(1))
        .copied()
        .unwrap_or(0)
        .saturating_sub(grid.cell_start.get(arrival).copied().unwrap_or(0))
        .saturating_sub(
            before
                .get(arrival.saturating_add(1))
                .copied()
                .unwrap_or(0)
                .saturating_sub(before.get(arrival).copied().unwrap_or(0)),
        );
    // The cell owned nothing, so the floor gave it three slots: three arrivals fit and fifty-seven are borrowed.
    assert_eq!(
        grid.cell_len.get(arrival).copied(),
        Some(60),
        "every arrival should have landed in the one cell"
    );
    assert!(
        grown >= 57,
        "the receiving run should have borrowed every slot past its floor, it borrowed {grown}"
    );
    assert!(
        moved_starts(&before, &grid.cell_start) >= 1,
        "a borrow disturbs the runs after the one that asked"
    );
    let reference = oracle(grid.entries(), config, 4);
    assert_same_table(&grid, &reference, "overflowing cell");
}
#[test]
fn slack_now_decides_how_far_the_borrow_walks_rather_than_whether_it_happens() {
    // Before the borrow budget this was the test of the slack rule: two hundred bodies in a cell give it 51 spare slots at
    // the default and 101 at half, so sixty arrivals were refused by one rule and absorbed by the other. The borrow
    // removed the refusal, so what the rule decides now is the walking distance: at the default the tick moves nine
    // boundaries, at half it moves none. Both answers have to leave a correct table.
    let config = config(16.0);
    let first = Vec3::new(64.0, 64.0, 64.0);
    let second = Vec3::new(400.0, 400.0, 400.0);
    for (slack, absorbed) in [(25_u8, false), (50, true)] {
        let mut grid = SpatialGrid::new(config)
            .or_die()
            .with_threads(4)
            .with_slack_percent(slack);
        for index in 0..400_u32 {
            grid.insert(GridEntry {
                id: NpcId::unique(index),
                position: if index < 200 { first } else { second },
                velocity: Vec3::ZERO,
            });
        }
        grid.build();
        // Sixty bodies leave the first cell and arrive in the second, which is already full to its occupancy.
        for index in 0..60_u32 {
            if let Some(entry) = grid.entries_mut().get_mut(index as usize) {
                entry.position = second;
            }
        }
        let before = grid.cell_start.clone();
        let outcome = grid.build_incremental();
        assert_eq!(
            outcome,
            RebuildOutcome::Patched { changed: 60 },
            "{slack}% slack: sixty arrivals should fit somewhere"
        );
        let walked = moved_starts(&before, &grid.cell_start);
        assert_eq!(
            walked > 0,
            !absorbed,
            "{slack}% slack: expected {} boundaries to move, {walked} did",
            i32::from(!absorbed)
        );
        grid.check_invariants().or_die();
        let reference = oracle(grid.entries(), config, 4);
        assert_same_table(&grid, &reference, "slack trial");
    }
}

#[test]
fn the_floor_still_decides_whether_a_pair_in_an_empty_cell_has_to_borrow() {
    // The case no percentage can cover, which is why the floor exists: an empty cell owns one slot plus its floor, so a
    // pair arriving together fits inside the cell only if the floor is at least two, and is invisible to
    // `with_slack_percent` because a quarter of zero is zero. The borrow means the pair no longer forces a rebuild when
    // the floor is zero; what the floor still buys is that no neighbour is disturbed at all.
    let config = config(16.0);
    let occupied = Vec3::new(64.0, 64.0, 64.0);
    let empty = Vec3::new(400.0, 400.0, 400.0);
    for (floor, absorbed) in [(0_u32, false), (DEFAULT_SLACK_FLOOR, true)] {
        let mut grid = SpatialGrid::new(config).or_die().with_slack_floor(floor);
        for index in 0..8_u32 {
            grid.insert(GridEntry {
                id: NpcId::unique(index),
                position: occupied,
                velocity: Vec3::ZERO,
            });
        }
        grid.build();
        // Two bodies leave the occupied cell and arrive in one that held nothing.
        for index in 0..2_u32 {
            if let Some(entry) = grid.entries_mut().get_mut(index as usize) {
                entry.position = empty;
            }
        }
        let before = grid.cell_start.clone();
        let outcome = grid.build_incremental();
        assert_eq!(
            outcome,
            RebuildOutcome::Patched { changed: 2 },
            "a floor of {floor} should leave a pair somewhere to land"
        );
        let walked = moved_starts(&before, &grid.cell_start);
        assert_eq!(
            walked > 0,
            !absorbed,
            "a floor of {floor}: expected {} boundaries to move, {walked} did",
            i32::from(!absorbed)
        );
        grid.check_invariants().or_die();
        let reference = oracle(grid.entries(), config, 6);
        assert_same_table(&grid, &reference, "empty cell arrival");
    }
}

/// Three cells in a row along x, each holding one body, with the slack rules switched off so a run is exactly as wide as
/// its count plus one and a full run is easy to make.
fn three_cells_along_x(config: SpatialGridConfig) -> SpatialGrid {
    let mut grid = SpatialGrid::new(config)
        .or_die()
        .with_slack_percent(0)
        .with_slack_floor(0);
    for (index, x) in [8.0_f32, 24.0, 40.0].into_iter().enumerate() {
        grid.insert(GridEntry {
            id: NpcId::unique(num::usize_to_u32(index)),
            position: Vec3::new(x, 8.0, 8.0),
            velocity: Vec3::ZERO,
        });
    }
    // Two more in cell 0, so it owns one spare slot and no more.
    for index in 3..5_u32 {
        grid.insert(GridEntry {
            id: NpcId::unique(index),
            position: Vec3::new(8.0, 8.0, 8.0),
            velocity: Vec3::ZERO,
        });
    }
    grid.build();
    grid.check_invariants().or_die();
    grid
}

#[test]
fn a_borrow_walks_past_a_full_run_and_stops_at_the_first_with_room() {
    // Cell 0 owns one spare slot; three arrivals need three. The first fits, the second borrows from cell 1 (which then
    // has no room of its own), and the third has to walk past cell 1 to cell 2. Three boundaries move, two of them in the
    // same tick, and the result still has to be the table a rebuild would have laid out.
    let config = config(16.0);
    let mut grid = three_cells_along_x(config);
    // Bodies at x = 56, 72, 88 sit in cells 3, 4, 5 and arrive in cell 0 together.
    for (index, x) in [(5_u32, 56.0_f32), (6, 72.0), (7, 88.0)] {
        grid.insert(GridEntry {
            id: NpcId::unique(index),
            position: Vec3::new(x, 8.0, 8.0),
            velocity: Vec3::ZERO,
        });
    }
    grid.build();
    let before = grid.cell_start.clone();
    for (index, _) in [(5_u32, 0), (6, 0), (7, 0)] {
        if let Some(entry) = grid.entries_mut().get_mut(index as usize) {
            entry.position = Vec3::new(8.0, 8.0, 8.0);
        }
    }
    let outcome = grid.build_incremental();
    assert_eq!(
        outcome,
        RebuildOutcome::Patched { changed: 3 },
        "three arrivals into a run with one spare slot should borrow twice, not rebuild"
    );
    let walked = moved_starts(&before, &grid.cell_start);
    assert!(
        walked >= 2,
        "the third arrival should have walked past the full run after it, and moved {walked} boundaries"
    );
    grid.check_invariants().or_die();
    let reference = oracle(grid.entries(), config, 6);
    assert_same_table(&grid, &reference, "cascading borrow");
}

#[test]
fn a_donor_wider_than_the_budget_is_left_to_the_rebuild() {
    // The budget is in elements shifted, because that is what a borrow costs: the donor's tail is copied one slot up. A
    // donor holding more elements than the budget cannot be borrowed from at any depth, and a tick that would move more
    // than a rebuild costs is the tick that should rebuild.
    let config = config(16.0);
    let mut grid = SpatialGrid::new(config)
        .or_die()
        .with_slack_percent(0)
        .with_slack_floor(0);
    grid.insert(GridEntry {
        id: NpcId::unique(0),
        position: Vec3::new(8.0, 8.0, 8.0),
        velocity: Vec3::ZERO,
    });
    let wide = num::usize_to_u32(BORROW_MAX_ELEMENTS).saturating_add(16);
    for index in 0..wide {
        grid.insert(GridEntry {
            id: NpcId::unique(index.saturating_add(1)),
            position: Vec3::new(24.0, 8.0, 8.0),
            velocity: Vec3::ZERO,
        });
    }
    grid.build();
    grid.check_invariants().or_die();
    // Two bodies arrive in cell 0, which owns one spare slot, so the second one asks cell 1 for a slot.
    for index in 1..3_u32 {
        if let Some(entry) = grid.entries_mut().get_mut(index as usize) {
            entry.position = Vec3::new(8.0, 8.0, 8.0);
        }
    }
    assert_eq!(
        grid.build_incremental(),
        RebuildOutcome::Rebuilt {
            changed: 2,
            cause: RebuildCause::NarrowRun,
        },
        "a donor wider than the budget should hand the tick to the rebuild"
    );
    grid.check_invariants().or_die();
    let reference = oracle(grid.entries(), config, 6);
    assert_same_table(&grid, &reference, "over-budget borrow");
}

#[test]
fn a_stale_table_is_named_rather_than_patched() {
    // The patch trusts the previous table: it looks for each mover in the run its old cell names. A table that lost that
    // body has to be rebuilt, and the outcome has to say so instead of blaming the crowd.
    let config = config(16.0);
    let mut grid = three_cells_along_x(config);
    // Point the remembered cell of one body at a cell whose run does not contain it, which is the state a truncated write
    // leaves behind. The tick then reads that cell as the body's old home, looks for it there, and has to refuse.
    let wrong_cell = grid.cells.get(2).copied().unwrap_or(0);
    if let Some(slot) = grid.cells.get_mut(4) {
        *slot = wrong_cell;
    }
    assert_eq!(
        grid.build_incremental(),
        RebuildOutcome::Rebuilt {
            changed: 1,
            cause: RebuildCause::StaleTable,
        },
        "a run that lost its body is not a run the patch may edit"
    );
    grid.check_invariants().or_die();
    let reference = oracle(grid.entries(), config, 6);
    assert_same_table(&grid, &reference, "stale table");
}

/// The same bodies, the same coordinates, a different arrival order: a game spawns from whatever events it has, so the
/// table may not depend on which order that was.
///
/// Two spawn orders over the same bodies: which cell a body lands in, and how many bodies a cell holds, may not depend on
/// the order they arrived in. What *does* depend on it is the order inside a cell, because placement writes each run in
/// arrival order rather than sorting it, and that order is a query's tie-break. Both halves are pinned here.
///
/// The comparison is by identity rather than by index, because `cells` is keyed by insertion index and the two grids index
/// the same body differently. What has to agree is which bodies share a cell and what order they appear in, which is what a
/// query reads.
#[test]
fn arrival_order_decides_the_order_inside_a_cell_and_nothing_else() {
    fn build(order: impl Iterator<Item = usize>, bodies: &[(u32, Vec3)]) -> SpatialGrid {
        let mut grid = SpatialGrid::new(config(64.0)).or_die();
        for index in order {
            let Some((id, position)) = bodies.get(index).copied() else {
                continue;
            };
            grid.insert(GridEntry {
                id: NpcId::unique(id),
                position,
                velocity: Vec3::ZERO,
            });
        }
        grid.build();
        grid
    }

    /// The bodies of every cell, in the order the run holds them, as identities.
    fn per_cell(grid: &SpatialGrid) -> Vec<Vec<u32>> {
        (0..grid.cell_count())
            .map(|cell| {
                let start = grid.cell_start.get(cell).copied().unwrap_or(0);
                let len = grid.cell_len.get(cell).copied().unwrap_or(0);
                let start = usize::try_from(start).unwrap_or(usize::MAX);
                let end = start.saturating_add(usize::try_from(len).unwrap_or(0));
                grid.order
                    .get(start..end)
                    .map(|run| {
                        run.iter()
                            .filter_map(|slot| {
                                let index = usize::try_from(*slot).ok()?;
                                Some(grid.entries.get(index)?.id.to_bits())
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            })
            .collect()
    }

    let bodies: Vec<(u32, Vec3)> = (0..48_u32)
        .map(|index| {
            let lane: f32 = match index % 3 {
                0 => 9.0,
                1 => 101.0,
                _ => 233.0,
            };
            let depth: f32 = if index % 4 == 0 { 17.0 } else { 149.0 };
            let height: f32 = if index % 5 == 0 { 33.0 } else { 301.0 };
            (index, Vec3::new(lane, depth, height))
        })
        .collect();

    let forward = build(0..bodies.len(), &bodies);
    let backward = build((0..bodies.len()).rev(), &bodies);

    assert_eq!(
        forward.cell_start, backward.cell_start,
        "the window table is indexed by cell, so arrival order cannot reach it"
    );
    assert_eq!(
        forward.cell_len, backward.cell_len,
        "the same bodies in the same cells have the same run lengths"
    );

    let mut forward_sets = per_cell(&forward);
    let mut backward_sets = per_cell(&backward);
    for run in &mut forward_sets {
        run.sort_unstable();
    }
    for run in &mut backward_sets {
        run.sort_unstable();
    }
    assert_eq!(
        forward_sets, backward_sets,
        "which bodies share a cell is a function of ids and positions"
    );

    // The order inside a cell is not: `place` writes each run in arrival order, so reversing the spawn reverses the runs.
    // If this ever starts passing, placement grew a sort and the doc on `SpatialGrid::insert` is now wrong.
    let runs = per_cell(&forward);
    assert!(
        runs.iter().any(|run| run.len() > 1),
        "the fixture needs at least one shared cell for this to mean anything"
    );
    assert_ne!(
        per_cell(&forward),
        per_cell(&backward),
        "arrival order is supposed to be visible inside a cell"
    );
}
