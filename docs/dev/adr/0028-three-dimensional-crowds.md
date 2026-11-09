# ADR 0028, Three-dimensional crowds

**Status:** Accepted (2026-11-04)
**Supersedes the planar assumption in:** ADR 0026 (crowd steering), ADR 0027 (spawn placement)

## The problem

ADR 0026 and ADR 0027 both shipped a two-dimensional crowd and said so. The kernel stored `[f32;
2]` positions, flattened cells with a constant vertical index derived from a plane height, and
scanned one layer of cells. `Placement` could scatter a crowd in `x` and `z` and nowhere else.

That is a real limitation, not a tidy simplification:

- a stairwell, a balcony, a multi-floor market or a hangar needs `y`;
- a swarm, a flock in the air, a shoal, fireworks debris, a sci-fi city with sky traffic, all are
  three-dimensional crowds and all are the same equations;
- and the failure mode is quiet. A game puts NPCs at different heights, the kernel buckets them by
  their vertical *neighbourhood constant*, neighbours that differ in `y` are never examined, and the
  crowd steers as though everyone were on a plane. Nothing errors. The crowd simply behaves as if it
  cannot see half of itself.

Both decisions ended with the same sentence: this is a 2D decision, and the 3D version waits for
the same piece of work. This is that piece of work.

## The decision

The crowd kernel and the spawn placement are three-dimensional. There is one code path, not a
planar path with a volumetric escape hatch.

**The kernel** (`src/application/crowd.rs`): positions, velocities, targets and desired velocities
are `[f32; 3]`; cells come from all three axes with the grid's own floor-and-clamp and the grid's
own flattening (`(z · dims.y + y) · dims.x + x`); the scan walks a box of cells `x ± r, y ± r, z ±
r` and tests true 3D distance; separation, alignment and cohesion accumulate on all three axes; the
desired velocity keeps its vertical component. `Crowd::configure` loses its `plane` argument, and
`configure_from_grid_on_plane` disappears with it.

**The placement** (`src/domain/spawn.rs`): two new variants, `UniformVolume` (uniform in a box) and
`UniformSphere` (volume-uniform, `r = R·∛u`, direction uniform on the sphere). `Fixed` already took
`Vec3` and now means what it says. `Spiral` and `UniformDisc` stay planar by definition and their
centres carry a height, so "a crowd on this floor" is still one line.

**Nothing else had to change.** `SpatialGrid` was already three-dimensional, dims `[x, y, z]`,
`cell_of` on all three axes, a radius query that tests 3D distance, and `integrate_flock` was
already integrating a `Vec3`. The planar assumption lived in exactly two places, and this ADR
removes both.

## What it costs

| 10,000 agents | Planar (ADR 0026) | 3D (this ADR) |
| --- | --- | --- |
| `flock_frame/10000` | 7.71 ms | **9.18 ms** |
| `flock_frame_large/100000` | 73.5 ms | 88–92 ms |
| `flock_frame/1` | 1.01 ms | 0.99 ms |

About 19% on a planar crowd, and it is arithmetic rather than bookkeeping: three components per
distance, three per accumulator, three per stored vector. The cell structure does not pay, the
grid was always volumetric.

I tried to give the planar case its 19% back with a single-layer fast path (detect that every agent
shares a vertical cell, then scan one layer), measured it, and **removed it**: it changed nothing,
because the cost is in the per-neighbour arithmetic and memory traffic, not in the empty cell
lookups the loop already skips. Shipping a measured no-op with a subtle invariant attached is worse
than paying a 19% tax that is visible in the benchmark table. The numbers above are the tax, stated
as a number rather than as "negligible".

The budget still holds with room: 9.18 ms is 1.8× inside 16.6 ms at 10,000 agents, and the band
path (ADR 0018–0021) remains the answer above that.

## Consequences

- `Crowd` is a breaking change for anyone who used it directly: `push` takes `[f32; 3]`,
  `desired` returns `[f32; 3]`, `configure` takes three arguments. The crate is pre-release and the
  kernel is an implementation detail behind `steer_flock`, but the changelog says so.
- `Placement::validate` now checks the vertical extent of every variant, so a box that pokes out of
  the top of the world is refused before it spawns, the same trap ADR 0027 closed horizontally.
- The test suite gains the assertions that would have caught the planar assumption:
  `a_crowd_above_and_below_pulls_vertically` (a stack 4 m apart moves along `y`),
  `a_crowd_that_fills_the_volume_uses_more_than_one_layer` (a volumetric crowd is not "isolated"
  and occupies many cells), and the end-to-end
  `a_volume_crowd_spawns_at_height_and_stays_there` (heights survive spawning *and* nine frames of
  steering, the way a planar assumption comes back is a crowd flattened by the first pass).
- `a_sphere_is_volume_uniform_rather_than_radius_uniform` holds the sampling to the volume, not the
  radius: half the radius is an eighth of the volume, so about 12.5% of the crowd belongs inside it.
  Sampling `r` linearly would put half there and give a swarm with an empty middle.

## What was not done

The grid is a uniform lattice. That is right for crowds of similar size and wrong for a city where
one cell holds a stadium and the next holds a corridor, the coarsening in ADR 0003 handles the
volume, not the variance. A hierarchical grid (an octree, or a two-level lattice) is the next
structural change if a customer's world is that uneven; it is not needed to be three-dimensional.
