//! Navigation links through the whole pipeline: a ladder changes the route, and the field
//! says so.
//!
//! The grid here is a 16x16 room split by a wall with one gap, which is the cheapest level
//! that can tell a shortcut from a detour: without the link the only way round is the gap,
//! with it there is a ladder through the wall.

use bevy_flock::domain::flow::{FlowTarget, solve};
use bevy_flock::domain::links::{LinkTag, NavLink};
use bevy_flock::domain::nav::{GridNavSource, NavCell, NavSource};
use bevy_flock::testkit::OrDie;
use bevy_math::Vec3;

const LADDER: LinkTag = LinkTag::new(1);

/// A room split by a wall at x = 8, with a gap at the top row.
fn room() -> GridNavSource {
    let mut grid = GridNavSource::open(Vec3::ZERO, 16, 16, 1.0).or_die();
    grid.add_obstacle(NavCell::new(8, 4), NavCell::new(8, 15))
        .or_die();
    grid
}

#[test]
fn a_link_found_nothing_beats_the_long_way_round() {
    let grid = room();
    // Start left of the wall, target right of it, both level with the blocked part.
    let field = solve(&grid, FlowTarget::at_cell(&grid, NavCell::new(13, 10)));
    let around = field.cost_at(NavCell::new(3, 10)).or_die();

    // A ladder straight through the wall at the same rows.
    let mut linked = room();
    linked
        .add_link(NavLink::new(
            NavCell::new(7, 10),
            NavCell::new(9, 10),
            20,
            LADDER,
        ))
        .or_die();
    let field = solve(&linked, FlowTarget::at_cell(&linked, NavCell::new(13, 10)));
    let through = field.cost_at(NavCell::new(3, 10)).or_die();

    assert!(
        through < around,
        "the ladder did not shorten the route: {through} against {around}"
    );
    assert_ne!(through, u32::MAX, "the ladder made the start unreachable");
}

#[test]
fn the_field_reports_the_tag_of_the_link_it_used() {
    let mut grid = room();
    grid.add_link(NavLink::new(
        NavCell::new(7, 10),
        NavCell::new(9, 10),
        20,
        LADDER,
    ))
    .or_die();
    let field = solve(&grid, FlowTarget::at_cell(&grid, NavCell::new(13, 10)));

    assert!(field.has_links(), "a field over a linked source has links");
    // The tag is reported where the agent must stand to take the link, not where it lands:
    // that is the cell a tree branches on and the cell an animation has to play at.
    let climb = field.link_tag_at(NavCell::new(7, 10)).or_die();
    assert_eq!(climb, LADDER);
    assert_eq!(
        field.link_tag_at(NavCell::new(9, 10)),
        None,
        "the landing cell should not claim a traversal it does not have to make"
    );
    // A cell that has nothing to do with the ladder reports nothing.
    assert_eq!(field.link_tag_at(NavCell::new(2, 2)), None);
}

#[test]
fn a_field_over_a_source_without_links_pays_nothing() {
    let grid = room();
    let field = solve(&grid, FlowTarget::at_cell(&grid, NavCell::new(13, 10)));
    assert!(!field.has_links());
    let unlinked_bytes = field.bytes();

    let mut linked = room();
    linked
        .add_link(NavLink::new(
            NavCell::new(7, 10),
            NavCell::new(9, 10),
            20,
            LADDER,
        ))
        .or_die();
    let linked_field = solve(&linked, FlowTarget::at_cell(&linked, NavCell::new(13, 10)));
    // Two bytes per cell, and not a byte more.
    assert_eq!(
        linked_field.bytes() - unlinked_bytes,
        16 * 16 * core::mem::size_of::<u16>()
    );
}

#[test]
fn a_link_that_costs_too_much_is_ignored_by_the_search() {
    // The long way round is 4 cells up plus 6 across: about 100 in step cost. A ladder
    // priced above that must lose.
    let mut grid = room();
    grid.add_link(NavLink::new(
        NavCell::new(7, 10),
        NavCell::new(9, 10),
        5_000,
        LADDER,
    ))
    .or_die();
    let linked_field = solve(&grid, FlowTarget::at_cell(&grid, NavCell::new(13, 10)));
    let plain = room();
    let plain_field = solve(&plain, FlowTarget::at_cell(&plain, NavCell::new(13, 10)));
    assert_eq!(
        linked_field.cost_at(NavCell::new(3, 10)),
        plain_field.cost_at(NavCell::new(3, 10)),
        "an expensive ladder changed a route it should have lost"
    );
}

#[test]
fn adding_a_link_invalidates_a_cached_field() {
    let mut grid = room();
    let before = grid.version();
    grid.add_link(NavLink::new(
        NavCell::new(7, 10),
        NavCell::new(9, 10),
        20,
        LADDER,
    ))
    .or_die();
    assert_ne!(grid.version(), before, "the version did not move");
}

#[test]
fn a_two_way_link_shortens_the_route_in_both_directions() {
    let mut grid = room();
    grid.add_link(NavLink::two_way(
        NavCell::new(7, 10),
        NavCell::new(9, 10),
        20,
        LADDER,
    ))
    .or_die();
    let rightward = solve(&grid, FlowTarget::at_cell(&grid, NavCell::new(13, 10)));
    let leftward = solve(&grid, FlowTarget::at_cell(&grid, NavCell::new(2, 10)));
    assert!(rightward.cost_at(NavCell::new(2, 10)).unwrap_or(u32::MAX) < 300);
    assert!(leftward.cost_at(NavCell::new(13, 10)).unwrap_or(u32::MAX) < 300);
    assert_eq!(leftward.link_tag_at(NavCell::new(9, 10)), Some(LADDER));
}
