# 0048, Links as search edges, and a navmesh for games that want polygons

- **Status:** Accepted
- **Date:** 2026-11-05
- **Related:** ADR 0040 (ports), ADR 0046 (obstacle source)

## Context

A flow field over a grid expresses "walk there". It cannot express "this ledge is a shortcut",
"the ladder costs four seconds" or "the door is one-way", so level designers either flatten their
levels or bury traversal in behaviour trees where the path solver cannot see it. Unity and Unreal
both ship traversal links as first-class navigation data; the competitive research listed this as
a gap.

## Decision

### Links are edges in the search, not post-processing

`domain::links::NavLink { from, to, cost, tag }` with `LinkTag(u16)` so a game can read back what
kind of traversal a route uses. `LinkTable` keeps the links in two arrays, sorted by `from` then
`to`, and by `to` then `from`, so a forward expansion and a backward expansion are both a binary
search over a contiguous slice, and neither needs a map.

The flow solver relaxes links as ordinary edges: when it expands a cell it appends
`links_into(cell)` alongside the walkable neighbours. A link is therefore never a shortcut bolted
onto a finished field; it can be the *reason* a field reaches a cell at all, which is the whole
point of a ladder.

Determinism: the table is sorted at insert, and the tie-break is part of the sort key
(`insert_sorted` partitions on strictly-less so same-key edges are ordered by destination and
cost). A table built in a different insertion order iterates identically, which
`iteration_order_does_not_depend_on_insertion_order` asserts.

Links are part of the source, so `GridNavSource::add_link` and `with_links` bump the source's
version through the same path obstacle edits use; a cached field for the old table is dropped
rather than served stale.

### The navmesh is an optional polygon source

`domain::navmesh` (feature `navmesh`) is a polygon navmesh with area costs and a funnel
(string-pull) path. It exists because a game with hand-built level geometry should not have to
rasterise it into a grid to get a sensible route: the funnel cuts corners a grid cannot.

The funnel follows the standard portal-walking algorithm. Its two restart branches are the part
that is easy to get wrong, when one side pinches, the *opposite* corner becomes the new apex and
both sides restart at the portal that pinched them. Setting the indices to the new apex instead
leaves the funnel reading a portal it has just left, which produces a route that cuts a corner
through a wall. That bug was present in the first draft and is fixed here.

## Consequences

- `docs/book/src/crowds-at-scale.md` walks through a two-floor level whose ladder is a link.
- The `navmesh` feature is additive: no default build pays for it, and `domain::nav` has no
  dependency on polygons.
- `NavSource::links_leaving` / `links_into` default to empty, so a source with no traversal pays
  two virtual calls per expansion and nothing else.
- `FlowField::link_tag_at` lets a behaviour leaf ask "did the route use a ladder, and which one",
  which is what the book example reads.

## Alternatives rejected

- **Post-processing the field to add link shortcuts.** Cannot express a link as the only route,
  and produces fields whose costs are wrong on both sides of the link.
- **Links as teleport triggers in a behaviour tree.** Works until two NPCs need to path to
  different floors, and then the tree has to reimplement A*.
- **A navmesh-only implementation.** Grids are the better answer for a procedural or streaming
  level; polygons are the better answer for hand-built geometry. Both are tenable, so both ship.
