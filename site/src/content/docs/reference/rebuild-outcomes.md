---
title: Rebuild outcomes
description: What the spatial index did with a tick, and how to read it from your own game.
---

The spatial index has a cheap tick and an expensive one. On most ticks no body crosses a cell boundary, so the tables
the queries read are still correct and the work ends after recomputing cell numbers. On a tick where bodies do cross,
the index either absorbs the move into the existing table or counts everything again. The library reports which of the
three happened, because "the rebuild got slower" and "the crowd re-formed rather than moved" are different problems.

## The three outcomes

`RebuildOutcome` is what `SpatialGrid::build_incremental` returns.

| outcome | what it means |
| --- | --- |
| `Unchanged` | No body changed cell. Nothing was recounted, rescanned or replaced. |
| `Patched { changed }` | `changed` bodies moved between cells and the table absorbed them. The runs read back as if the table had been rebuilt: same cells, same order inside a cell, same starts and lengths. |
| `Rebuilt { changed, cause }` | The patch was declined and the tables were counted again. `changed` is how many bodies sit in a different cell than they did at the previous rebuild. |

A patched grid and a rebuilt grid are equal run by run rather than byte for byte. A rebuild lays the slack out from a
fresh count, and a patch keeps the slack where the last rebuild left it. Queries cannot see the difference; the memory
layout can.

## The four causes

`RebuildCause` says why a patch was declined. Each cause has a different fix.

| cause | what it means | what usually causes it |
| --- | --- | --- |
| `Population` | The table described a different population, so there was nothing to diff against. | A first build, an empty grid, or an entry list that changed length. |
| `TooManyMovers` | More bodies changed cell than the mover list holds, so the list was abandoned rather than truncated. | The crowd re-formed rather than moved: a spawn wave, a scene change. |
| `StaleTable` | A mover was not in the run its previous cell named, so the table was already inconsistent with the entries. | A bug, or a table edited from outside the grid. |
| `NarrowRun` | A run had no spare slot, and the runs after it could not lend one inside the borrow budget. | Arrivals clustering into one cell. More slack, or a bigger cell. |

## Reading it from a game

The Bevy adapter keeps the last outcome in a resource and counts the two kinds of tick in the existing statistics.

```rust
use bevy_flock::prelude::{FlockGridReport, FlockStats, RebuildOutcome};

fn report(app: &bevy_flock::prelude::App) {
    let outcome = app.world().resource::<FlockGridReport>().0;
    let stats = app.world().resource::<FlockStats>();
    match outcome {
        RebuildOutcome::Unchanged => {}
        RebuildOutcome::Patched { changed } => {
            println!("{changed} bodies crossed a cell boundary");
        }
        RebuildOutcome::Rebuilt { changed, cause } => {
            println!("rebuilt: {changed} bodies moved, cause {cause:?}");
        }
    }
    println!(
        "{} patched ticks, {} rebuilt ticks",
        stats.patched_ticks, stats.rebuilt_ticks
    );
}
```

A crowd that patches is a crowd that is being simulated rather than re-sorted. A crowd that rebuilds every tick is not:
something in it moves every body every tick, or the population changes every tick, and the cause says which of the two
it is. `RebuildCause` is the difference between a frame budget problem and a design problem.

The plugin's own spatial grid is built from `BoidsConfig::grid`, so the cell size and the cell budget are set there:

```rust
use bevy_flock::prelude::{BoidsConfig, SpatialGridConfig};

let config = BoidsConfig::default()
    .with_grid(SpatialGridConfig::default().with_cell_budget(1 << 21));
```

## When to check the index yourself

`SpatialGrid::check_invariants` audits a finished grid: runs contiguous inside their cells, the runs a permutation of the
entries, and the cursor at the end of each run. It allocates, so it is a debugging and test tool rather than something to
call in a frame. The sort check applies to a grid built from identity-ordered entries; a grid whose entries are in slot
order has runs in arrival order, which is the grid's contract rather than a fault in it.
