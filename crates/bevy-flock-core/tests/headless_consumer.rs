//! A consumer that is not the facade.
//!
//! Phase 6's last gate asks a question the facade cannot answer about itself: can someone who has never heard of Bevy build
//! a spatial index, insert a crowd, query it and get deterministic answers? This file is that someone. It is an integration
//! test, so it can only use `bevy-flock-core`'s public API — no `crate::` shortcuts, no test-only hooks — and the crate's
//! dependency graph contains no `bevy_ecs`, `bevy_app` or `bevy_transform`, which is what `cargo tree -p bevy-flock-core`
//! shows.
//!
//! If this file stops compiling because something reached for the engine, the boundary moved and ADR 0058 is wrong.
use bevy_flock_core::ConfigError;
use bevy_flock_core::domain::ids::NpcId;
use bevy_flock_core::domain::spatial::{
    GridEntry, Neighbour, SpatialGrid, SpatialGridConfig,
};
use bevy_math::Vec3;

/// A deterministic scatter with no RNG and no wall clock: two builds of the same seed must agree exactly.
fn grid() -> Result<SpatialGrid, ConfigError> {
    let mut grid = SpatialGrid::new(SpatialGridConfig::default())?;
    for index in 0..512_u16 {
        let step = f32::from(index);
        grid.insert(GridEntry {
            id: NpcId::unique(u32::from(index)),
            position: Vec3::new(
                (step * 0.37) % 500.0,
                (step * 1.7) % 500.0,
                (step * 0.3) % 500.0,
            ),
            velocity: Vec3::ZERO,
        });
    }
    grid.build();
    Ok(grid)
}

/// Build, index and query a crowd with no `App`, no plugin and no `World`.
#[test]
fn a_crowd_can_be_indexed_and_queried_without_an_app() -> Result<(), ConfigError> {
    let first = grid()?;
    let second = grid()?;

    assert_eq!(first.entries().len(), 512, "every body is in the index");
    assert_eq!(
        first.cell_count(),
        second.cell_count(),
        "the same bodies over the same volume must produce the same grid"
    );
    assert_eq!(first.block_count(), second.block_count());

    // Determinism, checked the way a replay would check it: the same query against two separately built indexes.
    let mut left: Vec<Neighbour> = Vec::new();
    let mut right: Vec<Neighbour> = Vec::new();
    // Query where a body actually is: an arbitrary point can legitimately have nothing within 32 m, and a test that
    // assumes otherwise measures the scatter rather than the index.
    let centre = first
        .entries()
        .first()
        .map_or(Vec3::ZERO, |entry| entry.position);
    first.radius(centre, 32.0, &mut left);
    second.radius(centre, 32.0, &mut right);
    assert_eq!(left.len(), right.len(), "the same query must find the same bodies");

    // A radius query is the reason the index exists: it looks at a neighbourhood, not the whole crowd.
    assert!(!left.is_empty(), "the crew is dense enough to find something");
    assert!(
        left.len() < first.entries().len(),
        "a radius query must not walk the whole population"
    );

    // And the cells are a property of the volume, not of the bodies.
    let cell = first.cell_of(centre);
    assert_eq!(cell, second.cell_of(centre));
    let cells = u32::try_from(first.cell_count()).unwrap_or(u32::MAX);
    assert!(cell < cells);
    let _ = first.cell_center(cell);
    Ok(())
}
