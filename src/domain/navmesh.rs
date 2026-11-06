//! A polygon navmesh: convex cells, area costs, A\* over the cell graph, and a funnel.
//!
//! A grid is the right shape for a crowd that mostly walks on open ground, and the wrong
//! shape for a level with a doorway: a cell grid cannot say "this corridor is two metres
//! wide" without either over-blocking it or letting everyone clip the frame. A navigation
//! mesh says it once, as a convex polygon, and a string-pulled path through it is the
//! shortest route rather than a staircase.
//!
//! This module is deliberately a *library*, not a baker. It takes convex polygons you already
//! have — from a level editor, a CSG pass, a Recast build, or the game's own geometry —
//! validates them, derives adjacency from shared edges, and answers two questions: where can
//! I stand, and what is the shortest walkable route between two points. Baking is a content
//! pipeline, and a content pipeline belongs to the game.
//!
//! # Shape of the data
//!
//! Vertices are indexed, polygons are convex and wound counter-clockwise seen from above, and
//! adjacency is derived rather than authored: two polygons are neighbours when they share an
//! edge, and the cost of crossing is the length of that edge. A doorway is cheap to cross and
//! a long seam is not, which is what stops a route wandering across a hall it could have cut
//! through. Edges are matched by *position*, not by index, so a triangle soup whose vertices
//! are duplicated per triangle builds the same graph as a welded mesh.
//!
//! # Determinism
//!
//! Costs are integers in millimetres, adjacency is sorted by `(cost, polygon index)`, and the
//! A\* frontier breaks ties on distance-to-goal and then on polygon index. The same query on
//! the same mesh returns the same route, which is what a replay needs (ADR 0002).

use bevy_math::{Vec2, Vec3};
use smallvec::SmallVec;

use crate::domain::nav::{GridNavSource, NavBounds, NavCell, NavError, NavSource, NavVersion};

/// How many distinct areas a mesh can tag its polygons with.
pub const MAX_AREAS: usize = 32;

/// One of up to [`MAX_AREAS`] kinds of ground: road, grass, water, a door only staff may use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct AreaId(pub u8);

impl AreaId {
    /// The area every polygon belongs to until it is tagged.
    pub const DEFAULT: Self = Self(0);

    /// An area from a number, folded into the maskable range.
    #[must_use]
    pub const fn new(area: u8) -> Self {
        Self(area % MAX_AREAS as u8)
    }
}

/// A set of areas an agent may walk on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct AreaMask(pub u32);

impl AreaMask {
    /// Every area: what an agent with no opinion uses.
    #[must_use]
    pub const fn all() -> Self {
        Self(u32::MAX)
    }

    /// No area at all. A query with this mask finds nothing, which is a legitimate way to
    /// switch navigation off for an agent rather than to special-case it at the call site.
    pub const NONE: Self = Self(0);

    /// A mask holding one area.
    #[must_use]
    pub const fn of(area: AreaId) -> Self {
        Self(1 << area.0)
    }

    /// Whether an area is in the set.
    #[must_use]
    pub const fn contains(self, area: AreaId) -> bool {
        self.0 & (1 << area.0) != 0
    }

    /// This mask with an area added.
    #[must_use]
    pub const fn with(self, area: AreaId) -> Self {
        Self(self.0 | (1 << area.0))
    }

    /// This mask with an area removed.
    #[must_use]
    pub const fn without(self, area: AreaId) -> Self {
        Self(self.0 & !(1 << area.0))
    }
}

impl Default for AreaMask {
    fn default() -> Self {
        Self::all()
    }
}

/// A convex polygon of the mesh: a range of the shared vertex array, and an area.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct NavPolygon {
    /// Index of the first vertex.
    pub first: u32,
    /// How many vertices, at least three.
    pub len: u8,
    /// What kind of ground it is.
    pub area: AreaId,
}

impl NavPolygon {
    /// A polygon over a range of vertices, in the default area.
    #[must_use]
    pub const fn new(first: u32, len: u8) -> Self {
        Self {
            first,
            len,
            area: AreaId::DEFAULT,
        }
    }

    /// The same polygon in another area.
    #[must_use]
    pub const fn in_area(mut self, area: AreaId) -> Self {
        self.area = area;
        self
    }

    /// The vertex indices, as a range.
    #[must_use]
    pub const fn range(self) -> core::ops::Range<usize> {
        let start = self.first as usize;
        start..start + self.len as usize
    }
}

/// Why a mesh could not be built.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum NavMeshError {
    /// A mesh with no polygons.
    #[error("a navigation mesh needs at least one polygon")]
    Empty,
    /// A polygon with fewer than three vertices.
    #[error("polygon {index} has {len} vertices, and a polygon needs at least three")]
    DegeneratePolygon {
        /// The polygon.
        index: u32,
        /// How many vertices it claimed.
        len: u8,
    },
    /// A polygon whose vertices run past the end of the vertex array.
    #[error("polygon {index} references vertex {vertex}, and the mesh has {count}")]
    VertexOutOfRange {
        /// The polygon.
        index: u32,
        /// The vertex it asked for.
        vertex: u32,
        /// How many vertices exist.
        count: u32,
    },
    /// A polygon with no area: three vertices on one line.
    #[error("polygon {index} is degenerate: its vertices are collinear")]
    ZeroArea {
        /// The polygon.
        index: u32,
    },
}

/// A crossing into a neighbouring polygon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Crossing {
    /// The neighbour.
    pub polygon: u32,
    /// The cost of crossing the shared edge, in millimetres.
    pub cost: u32,
}

/// A point on a route.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PathPoint {
    /// Where to walk.
    pub position: Vec3,
    /// The polygon the point belongs to.
    pub polygon: u32,
    /// The area of that polygon, so a game can play a footstep for gravel.
    pub area: AreaId,
}

/// A position quantised to a millimetre and packed into a key, for edge matching.
fn point_key(point: Vec3) -> u64 {
    let quantise = |value: f32| -> u64 {
        let scaled = if value.is_finite() {
            value * 1000.0
        } else {
            0.0
        };
        (scaled.round() as i64).wrapping_add(1 << 32) as u64 & 0xffff_ffff
    };
    quantise(point.x) << 32 | quantise(point.z)
}

/// An edge keyed by the positions of its two ends, so duplicate vertices still match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct EdgeKey(u64, u64);

impl EdgeKey {
    fn of(a: Vec3, b: Vec3) -> Self {
        let (a, b) = (point_key(a), point_key(b));
        if a <= b { Self(a, b) } else { Self(b, a) }
    }
}

/// A navigation mesh: convex polygons with derived adjacency.
#[derive(Debug, Clone)]
pub struct NavMesh {
    vertices: Vec<Vec3>,
    polygons: Vec<NavPolygon>,
    adjacency: Vec<SmallVec<[Crossing; 6]>>,
    centroids: Vec<Vec3>,
    area_cost: [u16; MAX_AREAS],
    version: NavVersion,
}

impl NavMesh {
    /// Build a mesh from indexed convex polygons.
    ///
    /// Adjacency is derived from shared edges, so two polygons that touch only at a corner are
    /// *not* neighbours: a route cannot squeeze through the point where four walls meet, which
    /// is the polygon equivalent of not cutting a grid corner.
    ///
    /// # Errors
    ///
    /// [`NavMeshError::Empty`] for no polygons, [`NavMeshError::DegeneratePolygon`] for a
    /// polygon with fewer than three vertices, [`NavMeshError::VertexOutOfRange`] for an index
    /// past the vertex array, and [`NavMeshError::ZeroArea`] for collinear vertices.
    pub fn from_polygons(
        vertices: Vec<Vec3>,
        polygons: Vec<NavPolygon>,
    ) -> Result<Self, NavMeshError> {
        if polygons.is_empty() {
            return Err(NavMeshError::Empty);
        }
        let vertex_count = u32::try_from(vertices.len()).unwrap_or(u32::MAX);
        let mut centroids = Vec::with_capacity(polygons.len());
        for (index, polygon) in polygons.iter().enumerate() {
            let index = u32::try_from(index).unwrap_or(u32::MAX);
            if polygon.len < 3 {
                return Err(NavMeshError::DegeneratePolygon {
                    index,
                    len: polygon.len,
                });
            }
            let range = polygon.range();
            if let Some(last) = range.clone().last()
                && last as u32 >= vertex_count
            {
                return Err(NavMeshError::VertexOutOfRange {
                    index,
                    vertex: last as u32,
                    count: vertex_count,
                });
            }
            if polygon_area(&vertices, &range).abs() < 1.0e-3 {
                return Err(NavMeshError::ZeroArea { index });
            }
            centroids.push(centroid_of(&vertices, &range));
        }

        // Every edge of every polygon, keyed by the positions of its ends.
        let mut edges: Vec<(EdgeKey, u32, u32)> = Vec::new();
        for (index, polygon) in polygons.iter().enumerate() {
            let index = u32::try_from(index).unwrap_or(u32::MAX);
            let range = polygon.range();
            let len = range.len();
            for offset in 0..len {
                let Some(a) = vertices.get(range.start + offset).copied() else {
                    continue;
                };
                let Some(b) = vertices.get(range.start + (offset + 1) % len).copied() else {
                    continue;
                };
                edges.push((EdgeKey::of(a, b), index, millimetres(a.distance(b))));
            }
        }
        edges.sort_unstable();
        let mut adjacency: Vec<SmallVec<[Crossing; 6]>> = vec![SmallVec::new(); polygons.len()];
        for pair in edges.windows(2) {
            let (Some(&(key, left, cost)), Some(&(other_key, right, _))) =
                (pair.first(), pair.get(1))
            else {
                continue;
            };
            if key != other_key || left == right {
                continue;
            }
            if let Some(slot) = adjacency.get_mut(left as usize) {
                slot.push(Crossing {
                    polygon: right,
                    cost,
                });
            }
            if let Some(slot) = adjacency.get_mut(right as usize) {
                slot.push(Crossing {
                    polygon: left,
                    cost,
                });
            }
        }
        for slot in &mut adjacency {
            slot.sort_unstable_by_key(|crossing| (crossing.cost, crossing.polygon));
            slot.dedup_by_key(|crossing| crossing.polygon);
        }

        Ok(Self {
            vertices,
            polygons,
            adjacency,
            centroids,
            // 100 is neutral: a cost of 100 crosses an area for its geometric length.
            area_cost: [100; MAX_AREAS],
            version: NavVersion::FIRST,
        })
    }

    /// Build a mesh from a triangle soup, duplicating vertices per triangle.
    ///
    /// This is the entry point for any triangulator or baked mesh: winding is preserved, and
    /// adjacency is recovered by matching edge positions, so a soup that duplicates every
    /// vertex still produces a connected graph.
    ///
    /// # Errors
    ///
    /// As [`Self::from_polygons`], with a whole-index error reported against the triangle that
    /// supplied it.
    pub fn from_triangles(
        vertices: &[Vec3],
        indices: &[u32],
        area: AreaId,
    ) -> Result<Self, NavMeshError> {
        if indices.len() < 3 {
            return Err(NavMeshError::Empty);
        }
        let count = u32::try_from(vertices.len()).unwrap_or(u32::MAX);
        let mut owned = Vec::with_capacity(indices.len());
        let mut polygons = Vec::with_capacity(indices.len() / 3);
        for chunk in indices.chunks_exact(3) {
            let first = u32::try_from(owned.len()).unwrap_or(u32::MAX);
            for index in chunk {
                let Some(vertex) = vertices.get(*index as usize) else {
                    return Err(NavMeshError::VertexOutOfRange {
                        index: u32::try_from(polygons.len()).unwrap_or(u32::MAX),
                        vertex: *index,
                        count,
                    });
                };
                owned.push(*vertex);
            }
            polygons.push(NavPolygon::new(first, 3).in_area(area));
        }
        Self::from_polygons(owned, polygons)
    }

    /// Set what an area costs to cross, as a percentage of its geometric length.
    ///
    /// 100 is neutral, 200 makes crossing twice as expensive (so a route prefers to go round a
    /// swamp) and 1 makes it almost free (a road). Zero is read as 1, because a free area is an
    /// A\* that cannot refuse a route.
    pub fn set_area_cost(&mut self, area: AreaId, percent: u16) {
        if let Some(slot) = self.area_cost.get_mut(area.0 as usize) {
            *slot = percent.max(1);
            self.version = self.version.next();
        }
    }

    /// What an area costs, as a percentage.
    #[must_use]
    pub fn area_cost(&self, area: AreaId) -> u16 {
        self.area_cost.get(area.0 as usize).copied().unwrap_or(100)
    }

    /// How many polygons.
    #[must_use]
    pub fn polygon_count(&self) -> usize {
        self.polygons.len()
    }

    /// How many vertices.
    #[must_use]
    pub fn vertex_count(&self) -> usize {
        self.vertices.len()
    }

    /// The polygons.
    #[must_use]
    pub fn polygons(&self) -> &[NavPolygon] {
        &self.polygons
    }

    /// The neighbours of a polygon, sorted by `(cost, index)`.
    #[must_use]
    pub fn neighbours_of(&self, polygon: u32) -> &[Crossing] {
        self.adjacency
            .get(polygon as usize)
            .map_or(&[], SmallVec::as_slice)
    }

    /// The centre of a polygon.
    #[must_use]
    pub fn centroid(&self, polygon: u32) -> Option<Vec3> {
        self.centroids.get(polygon as usize).copied()
    }

    /// The version stamp, bumped whenever an area cost changes.
    #[must_use]
    pub const fn version(&self) -> NavVersion {
        self.version
    }

    /// Bytes the mesh occupies, for a memory report.
    #[must_use]
    pub fn bytes(&self) -> usize {
        self.vertices.len() * core::mem::size_of::<Vec3>()
            + self.polygons.len() * core::mem::size_of::<NavPolygon>()
            + self
                .adjacency
                .iter()
                .map(|slot| slot.len() * core::mem::size_of::<Crossing>())
                .sum::<usize>()
            + self.centroids.len() * core::mem::size_of::<Vec3>()
    }

    /// The polygon containing a point, if any.
    ///
    /// A point on a seam resolves to the lowest index, so the answer never depends on which
    /// polygon happened to be tested first.
    #[must_use]
    pub fn polygon_at(&self, point: Vec3) -> Option<u32> {
        let mut best: Option<u32> = None;
        for (index, polygon) in self.polygons.iter().enumerate() {
            if self.contains(*polygon, point) {
                let index = u32::try_from(index).unwrap_or(u32::MAX);
                best = Some(best.map_or(index, |found| found.min(index)));
            }
        }
        best
    }

    /// The nearest polygon to a point, by centroid: what a click on a wall resolves to.
    #[must_use]
    pub fn nearest_polygon(&self, point: Vec3) -> Option<u32> {
        let mut best: Option<(f32, u32)> = None;
        for (index, centroid) in self.centroids.iter().enumerate() {
            let distance = centroid.distance(point);
            if distance.is_finite() && best.is_none_or(|(known, _)| distance < known) {
                best = Some((distance, u32::try_from(index).unwrap_or(u32::MAX)));
            }
        }
        best.map(|(_, index)| index)
    }

    /// Whether a point is inside one polygon, tested in the ground plane.
    fn contains(&self, polygon: NavPolygon, point: Vec3) -> bool {
        let range = polygon.range();
        let len = range.len();
        if len < 3 {
            return false;
        }
        let point = Vec2::new(point.x, point.z);
        let mut sign = 0_i8;
        for offset in 0..len {
            let Some(a) = self.vertices.get(range.start + offset).copied() else {
                return false;
            };
            let Some(b) = self.vertices.get(range.start + (offset + 1) % len).copied() else {
                return false;
            };
            let edge = Vec2::new(b.x, b.z) - Vec2::new(a.x, a.z);
            let relative = point - Vec2::new(a.x, a.z);
            let cross = edge.x * relative.y - edge.y * relative.x;
            if cross.abs() <= 1.0e-6 {
                continue;
            }
            let side = if cross > 0.0 { 1 } else { -1 };
            if sign == 0 {
                sign = side;
            } else if sign != side {
                return false;
            }
        }
        true
    }

    /// The shortest walkable route between two points, or `None` when there is none.
    ///
    /// The route is string-pulled through the polygon corridor, so it is a handful of waypoints
    /// rather than one per polygon: an agent steers to each in turn. The first point is the
    /// caller's own position and the last is the destination, so a route can be handed straight
    /// to a steering target.
    #[must_use]
    pub fn path(&self, from: Vec3, to: Vec3, mask: AreaMask) -> Option<Vec<PathPoint>> {
        let start = self
            .polygon_at(from)
            .or_else(|| self.nearest_polygon(from))?;
        let goal = self.polygon_at(to).or_else(|| self.nearest_polygon(to))?;
        if !self.walkable(start, mask) || !self.walkable(goal, mask) {
            return None;
        }
        let corridor = self.solve_corridor(start, goal, mask)?.0;
        let portals = self.portals(&corridor);
        let points = self.funnel(&portals, &corridor, from, to);
        let mut route = Vec::with_capacity(points.len());
        for (position, polygon) in points {
            let area = self
                .polygons
                .get(polygon as usize)
                .map_or(AreaId::DEFAULT, |polygon| polygon.area);
            route.push(PathPoint {
                position,
                polygon,
                area,
            });
        }
        Some(route)
    }

    /// The polygon sequence a route would take, without string-pulling.
    ///
    /// This is the search's own answer, which is what a debug overlay draws and what an AI
    /// reads when it wants to know *which district* a route crosses rather than which points.
    #[must_use]
    pub fn corridor_between(&self, from: Vec3, to: Vec3, mask: AreaMask) -> Option<Vec<u32>> {
        let start = self
            .polygon_at(from)
            .or_else(|| self.nearest_polygon(from))?;
        let goal = self.polygon_at(to).or_else(|| self.nearest_polygon(to))?;
        if !self.walkable(start, mask) || !self.walkable(goal, mask) {
            return None;
        }
        self.solve_corridor(start, goal, mask)
            .map(|(route, _)| route)
    }

    /// What the corridor search charges for a route, in millimetres adjusted by area cost.
    ///
    /// Useful for deciding whether two points are "close" in travel terms rather than straight
    /// line, and the number a test asserts when it claims an expensive area was avoided.
    #[must_use]
    pub fn route_cost(&self, from: Vec3, to: Vec3, mask: AreaMask) -> Option<u32> {
        let start = self
            .polygon_at(from)
            .or_else(|| self.nearest_polygon(from))?;
        let goal = self.polygon_at(to).or_else(|| self.nearest_polygon(to))?;
        if !self.walkable(start, mask) || !self.walkable(goal, mask) {
            return None;
        }
        self.solve_corridor(start, goal, mask).map(|(_, cost)| cost)
    }

    fn walkable(&self, polygon: u32, mask: AreaMask) -> bool {
        self.polygons
            .get(polygon as usize)
            .is_some_and(|polygon| mask.contains(polygon.area))
    }

    /// A\* over the polygon graph, returning the polygon sequence and what it costs.
    fn solve_corridor(&self, start: u32, goal: u32, mask: AreaMask) -> Option<(Vec<u32>, u32)> {
        let count = self.polygons.len();
        let mut cost = vec![u32::MAX; count];
        let mut came_from = vec![u32::MAX; count];
        let mut costs = vec![0_u32; count];
        let goal_point = self.centroid(goal)?;
        if let Some(slot) = cost.get_mut(start as usize) {
            *slot = 0;
        }
        let heuristic = |polygon: u32| -> u32 {
            let straight = self
                .centroid(polygon)
                .map_or(0, |point| millimetres(point.distance(goal_point)));
            // Affordable: this is build-time or query-time, not per-agent-per-frame, and the
            // division keeps the heuristic in the same units as the edge costs.
            (straight / 10) * u32::from(self.area_cost_of(polygon)) / 100
        };
        let mut frontier: std::collections::BinaryHeap<std::cmp::Reverse<(u32, u32)>> =
            std::collections::BinaryHeap::new();
        frontier.push(std::cmp::Reverse((heuristic(start), start)));
        while let Some(std::cmp::Reverse((_, polygon))) = frontier.pop() {
            if polygon == goal {
                let mut route = vec![goal];
                let mut current = goal;
                while current != start {
                    current = *came_from.get(current as usize)?;
                    if current == u32::MAX {
                        return None;
                    }
                    route.push(current);
                }
                route.reverse();
                let total = costs.get(goal as usize).copied().unwrap_or(0);
                return Some((route, total));
            }
            let known = *cost.get(polygon as usize)?;
            let mut nexts: SmallVec<[Crossing; 6]> = SmallVec::new();
            nexts.extend_from_slice(self.neighbours_of(polygon));
            for crossing in nexts {
                if !self.walkable(crossing.polygon, mask) {
                    continue;
                }
                let step = crossing.cost * u32::from(self.area_cost_of(crossing.polygon)) / 100;
                let candidate = known.saturating_add(step);
                let Some(slot) = cost.get_mut(crossing.polygon as usize) else {
                    continue;
                };
                if candidate < *slot {
                    *slot = candidate;
                    if let Some(back) = came_from.get_mut(crossing.polygon as usize) {
                        *back = polygon;
                    }
                    if let Some(total) = costs.get_mut(crossing.polygon as usize) {
                        *total = candidate;
                    }
                    frontier.push(std::cmp::Reverse((
                        candidate.saturating_add(heuristic(crossing.polygon)),
                        crossing.polygon,
                    )));
                }
            }
        }
        None
    }

    fn area_cost_of(&self, polygon: u32) -> u16 {
        self.polygons
            .get(polygon as usize)
            .map_or(100, |polygon| self.area_cost(polygon.area))
    }

    /// The portal sequence through a corridor, as `(left, right)` vertex indices in the
    /// walking direction.
    fn portals(&self, corridor: &[u32]) -> Vec<(u32, u32)> {
        let mut portals = Vec::with_capacity(corridor.len());
        for pair in corridor.windows(2) {
            let (Some(&from), Some(&to)) = (
                self.polygons
                    .get(pair.first().copied().unwrap_or(0) as usize),
                self.polygons
                    .get(pair.get(1).copied().unwrap_or(0) as usize),
            ) else {
                continue;
            };
            if let Some(edge) = self.shared_edge(from, to) {
                portals.push(edge);
            }
        }
        portals
    }

    /// The edge two adjacent polygons share, wound as it appears in the first polygon.
    ///
    /// Both polygons are counter-clockwise, so the shared edge runs the other way in the
    /// neighbour. Returning the first polygon's winding puts the left end on the walker's left,
    /// which is what the funnel assumes.
    fn shared_edge(&self, from: NavPolygon, to: NavPolygon) -> Option<(u32, u32)> {
        let from_range = from.range();
        let to_range = to.range();
        let from_len = from_range.len();
        let to_len = to_range.len();
        let to_points: Vec<Vec3> = (0..to_len)
            .filter_map(|offset| self.vertices.get(to_range.start + offset).copied())
            .collect();
        for offset in 0..from_len {
            let a = from_range.start + offset;
            let b = from_range.start + (offset + 1) % from_len;
            let (Some(pa), Some(pb)) =
                (self.vertices.get(a).copied(), self.vertices.get(b).copied())
            else {
                continue;
            };
            // Both ends must be vertices of the neighbour, and consecutively so: a shared
            // corner is not a shared edge.
            let mut both = false;
            for index in 0..to_len {
                let (Some(pc), Some(pd)) = (
                    self.vertices.get(to_range.start + index).copied(),
                    self.vertices
                        .get(to_range.start + (index + 1) % to_len)
                        .copied(),
                ) else {
                    continue;
                };
                if (same_point(pc, pa) && same_point(pd, pb))
                    || (same_point(pc, pb) && same_point(pd, pa))
                {
                    both = true;
                    break;
                }
            }
            if both && to_points.iter().any(|point| same_point(*point, pa)) {
                return Some((
                    u32::try_from(a).unwrap_or(u32::MAX),
                    u32::try_from(b).unwrap_or(u32::MAX),
                ));
            }
        }
        None
    }

    /// String-pull a portal sequence into a route: the classic funnel, with the caller's
    /// position and the destination as degenerate portals at each end.
    ///
    /// The funnel is a state machine over the portals — an apex and two side indices — so the
    /// machine lives in [`FunnelSides`] and this function is the route it produces. The two sides
    /// are mirrors of each other, which is why they are two methods on one type rather than two
    /// copies of the same loop body.
    fn funnel(
        &self,
        portals: &[(u32, u32)],
        corridor: &[u32],
        from: Vec3,
        to: Vec3,
    ) -> Vec<(Vec3, u32)> {
        let flat = |point: Vec3| Vec2::new(point.x, point.z);
        let unwrap_point = |index: u32| -> Vec3 {
            self.vertices
                .get(index as usize)
                .copied()
                .unwrap_or(Vec3::ZERO)
        };
        let polygon_at = |index: usize| -> u32 {
            corridor
                .get(index)
                .copied()
                .unwrap_or_else(|| corridor.last().copied().unwrap_or(0))
        };

        let mut left: Vec<Vec2> = Vec::with_capacity(portals.len() + 2);
        let mut right: Vec<Vec2> = Vec::with_capacity(portals.len() + 2);
        let mut left_point_positions: Vec<Vec3> = Vec::with_capacity(portals.len() + 2);
        let mut right_point_positions: Vec<Vec3> = Vec::with_capacity(portals.len() + 2);
        left.push(flat(from));
        right.push(flat(from));
        left_point_positions.push(from);
        right_point_positions.push(from);
        for (l, r) in portals {
            let (lp, rp) = (unwrap_point(*l), unwrap_point(*r));
            left.push(flat(lp));
            right.push(flat(rp));
            left_point_positions.push(lp);
            right_point_positions.push(rp);
        }
        left.push(flat(to));
        right.push(flat(to));
        left_point_positions.push(to);
        right_point_positions.push(to);

        let mut route: Vec<(Vec3, u32)> = vec![(from, polygon_at(0))];
        let mut sides = FunnelSides::default();
        for index in 1..left.len() {
            if let Some(corner) = sides.step_right(index, &left, &right) {
                // The right side pinched: the left corner becomes the new apex, and both sides
                // restart at the portal that pinched it. Restarting at the new apex instead of at
                // `index` would leave the funnel reading a portal it just left.
                route.push((
                    left_point_positions.get(corner).copied().unwrap_or(from),
                    polygon_at(index),
                ));
                continue;
            }
            if let Some(corner) = sides.step_left(index, &left, &right) {
                route.push((
                    right_point_positions.get(corner).copied().unwrap_or(from),
                    polygon_at(index),
                ));
            }
        }

        let apex_point = left.get(sides.apex).copied().unwrap_or_else(|| flat(from));
        let goal = flat(to);
        if let (Some(left_point), Some(right_point)) =
            (left.get(sides.left), right.get(sides.right))
        {
            if cross_2d(apex_point, *left_point, goal) >= 0.0 && left_point != &apex_point {
                route.push((
                    left_point_positions
                        .get(sides.left)
                        .copied()
                        .unwrap_or(from),
                    polygon_at(left.len()),
                ));
            }
            if cross_2d(apex_point, *right_point, goal) <= 0.0 && right_point != &apex_point {
                route.push((
                    right_point_positions
                        .get(sides.right)
                        .copied()
                        .unwrap_or(from),
                    polygon_at(right.len()),
                ));
            }
        }
        route.push((to, polygon_at(corridor.len())));
        route.dedup_by(|a, b| (a.0 - b.0).length() < 1.0e-3);
        route
    }

    /// The ground-plane bounding box of a polygon, for stamping a cell footprint.
    fn polygon_bounds(&self, polygon: NavPolygon) -> Option<(Vec3, Vec3)> {
        let range = polygon.range();
        let first = self.vertices.get(range.start).copied()?;
        let mut min = first;
        let mut max = first;
        for index in range {
            let Some(point) = self.vertices.get(index).copied() else {
                continue;
            };
            min.x = min.x.min(point.x);
            min.z = min.z.min(point.z);
            max.x = max.x.max(point.x);
            max.z = max.z.max(point.z);
        }
        Some((min, max))
    }
}

/// The funnel's state: the apex and the two side indices, all into the same portal lists.
///
/// Both sides start at the caller's position, which is index zero.
#[derive(Debug, Clone, Copy, Default)]
struct FunnelSides {
    /// The portal the funnel is currently taut against.
    apex: usize,
    /// The left side's portal.
    left: usize,
    /// The right side's portal.
    right: usize,
}

impl FunnelSides {
    /// Fold portal `index` into the right side.
    ///
    /// Returns the corner that becomes the new apex when the right side pinched, in which case the
    /// caller emits a route point and restarts both sides at `index`.
    fn step_right(&mut self, index: usize, left: &[Vec2], right: &[Vec2]) -> Option<usize> {
        let apex_point = *left.get(self.apex)?;
        let left_point = *left.get(self.left)?;
        let right_point = *right.get(self.right)?;
        let new_right = *right.get(index)?;
        if cross_2d(apex_point, right_point, new_right) > 0.0 {
            return None;
        }
        if right_point == apex_point || cross_2d(apex_point, left_point, new_right) > 0.0 {
            self.right = index;
            return None;
        }
        let corner = self.left;
        self.apex = corner;
        self.left = index;
        self.right = index;
        Some(corner)
    }

    /// Fold portal `index` into the left side; the mirror of [`Self::step_right`].
    fn step_left(&mut self, index: usize, left: &[Vec2], right: &[Vec2]) -> Option<usize> {
        let apex_point = *left.get(self.apex)?;
        let left_point = *left.get(self.left)?;
        let right_point = *right.get(self.right)?;
        let new_left = *left.get(index)?;
        if cross_2d(apex_point, left_point, new_left) < 0.0 {
            return None;
        }
        if left_point == apex_point || cross_2d(apex_point, right_point, new_left) < 0.0 {
            self.left = index;
            return None;
        }
        let corner = self.right;
        self.apex = corner;
        self.left = index;
        self.right = index;
        Some(corner)
    }
}

/// Twice the signed area of a polygon, projected onto the ground plane.
fn polygon_area(vertices: &[Vec3], range: &core::ops::Range<usize>) -> f32 {
    let len = range.len();
    let mut total = 0.0;
    for offset in 0..len {
        let Some(a) = vertices.get(range.start + offset).copied() else {
            continue;
        };
        let Some(b) = vertices.get(range.start + (offset + 1) % len).copied() else {
            continue;
        };
        total += a.x * b.z - b.x * a.z;
    }
    total * 0.5
}

/// The average of a polygon's vertices.
fn centroid_of(vertices: &[Vec3], range: &core::ops::Range<usize>) -> Vec3 {
    let mut sum = Vec3::ZERO;
    let mut count = 0_u32;
    for offset in 0..range.len() {
        if let Some(point) = vertices.get(range.start + offset).copied() {
            sum += point;
            count += 1;
        }
    }
    sum / count.max(1) as f32
}

/// A distance in millimetres, which is the mesh's integer cost unit.
fn millimetres(distance: f32) -> u32 {
    if !distance.is_finite() || distance <= 0.0 {
        return 0;
    }
    u32::try_from((distance * 1000.0).round() as i64).unwrap_or(u32::MAX)
}

/// Whether two positions are the same point to a millimetre.
fn same_point(a: Vec3, b: Vec3) -> bool {
    point_key(a) == point_key(b)
}

/// The 2D cross product of `b - a` and `c - a`, positive when `c` is to the left.
fn cross_2d(a: Vec2, b: Vec2, c: Vec2) -> f32 {
    (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x)
}

/// A polygon mesh presented to the crate as a grid, so a flow field can be built over it.
///
/// The crowd steers on flow fields and a level's walkable surface is a mesh; this adapter is
/// where those two facts meet. At bake time it stamps each polygon's cell footprint into a
/// bitmap, and from then on it is an ordinary [`NavSource`] — deterministic, cacheable, and a
/// byte a cell. The resolution is the game's choice: 0.5 m suits a person, 2 m a vehicle.
#[derive(Debug, Clone)]
pub struct NavMeshSource {
    mesh: NavMesh,
    grid: GridNavSource,
}

impl NavMeshSource {
    /// Stamp a mesh into a grid covering `bounds`.
    ///
    /// A cell is walkable when its centre is inside a polygon. Cells outside every polygon —
    /// including the sliver between a doorway and its frame — are not, which is what keeps a
    /// crowd on the mesh rather than near it.
    ///
    /// # Errors
    ///
    /// [`NavError`] as [`NavBounds::new`], plus [`NavError::BitmapLength`] if the stamped
    /// bitmap and the bounds disagree, which would be a bug here rather than in the caller's
    /// data.
    pub fn bake(mesh: NavMesh, bounds: NavBounds) -> Result<Self, NavError> {
        let mut walkable = vec![false; bounds.cell_count()];
        for polygon in mesh.polygons() {
            let Some((min, max)) = mesh.polygon_bounds(*polygon) else {
                continue;
            };
            let (min_x, min_y) = clamp_cell(bounds, min);
            let (max_x, max_y) = clamp_cell(bounds, max);
            for y in min_y..=max_y {
                for x in min_x..=max_x {
                    let cell = NavCell::new(x, y);
                    let centre = Vec3::new(
                        bounds.origin.x + (f32::from(x) + 0.5) * bounds.cell_size,
                        bounds.origin.y,
                        bounds.origin.z + (f32::from(y) + 0.5) * bounds.cell_size,
                    );
                    if !bounds.contains(cell) || !mesh.contains(*polygon, centre) {
                        continue;
                    }
                    if let Some(slot) = walkable.get_mut(cell.index(bounds.width)) {
                        *slot = true;
                    }
                }
            }
        }
        let grid = GridNavSource::from_bitmap(bounds, walkable)?;
        Ok(Self { mesh, grid })
    }

    /// The mesh this source was baked from.
    #[must_use]
    pub const fn mesh(&self) -> &NavMesh {
        &self.mesh
    }

    /// How many cells came out walkable.
    #[must_use]
    pub fn walkable_cells(&self) -> usize {
        self.grid.walkable().iter().filter(|cell| **cell).count()
    }

    /// Route on the mesh: the string-pulled path a game animates an agent along.
    #[must_use]
    pub fn path(&self, from: Vec3, to: Vec3, mask: AreaMask) -> Option<Vec<PathPoint>> {
        self.mesh.path(from, to, mask)
    }
}

/// The cell containing a point, clamped into a grid, for a bounding-box stamp.
fn clamp_cell(bounds: NavBounds, point: Vec3) -> (u16, u16) {
    let clamp = |value: f32, limit: u16| -> u16 {
        if !value.is_finite() {
            return 0;
        }
        let last = limit.saturating_sub(1);
        let capped = f32::from(last);
        if value <= 0.0 {
            0
        } else if value >= capped {
            last
        } else {
            (value as u16).min(last)
        }
    };
    let x = ((point.x - bounds.origin.x) / bounds.cell_size).floor();
    let y = ((point.z - bounds.origin.z) / bounds.cell_size).floor();
    (clamp(x, bounds.width), clamp(y, bounds.height))
}

impl NavSource for NavMeshSource {
    fn bounds(&self) -> NavBounds {
        self.grid.bounds_of()
    }

    fn is_walkable(&self, cell: NavCell) -> bool {
        self.grid.is_walkable(cell)
    }

    fn neighbours(&self, cell: NavCell, out: &mut SmallVec<[(NavCell, u32); 8]>) {
        self.grid.neighbours(cell, out);
    }

    fn cell_of(&self, position: Vec3) -> Option<NavCell> {
        self.grid.cell_of(position)
    }

    fn center_of(&self, cell: NavCell) -> Vec3 {
        self.grid.center_of(cell)
    }

    fn version(&self) -> NavVersion {
        // The bake is a function of the mesh and the bounds, so the mesh's version is the
        // grid's version.
        self.mesh.version()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::flow::{FlowTarget, solve};
    use crate::testkit::{GetOrDie, OrDie, OrDieErr};

    /// Two 4 m quads side by side, sharing the edge at x = 4. The second is in area 1.
    fn pair() -> NavMesh {
        let vertices = vec![
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(4.0, 0.0, 0.0),
            Vec3::new(4.0, 0.0, 4.0),
            Vec3::new(0.0, 0.0, 4.0),
            // The second quad duplicates its shared edge: adjacency is matched by position,
            // which is what makes a triangulated or welded level behave the same.
            Vec3::new(4.0, 0.0, 0.0),
            Vec3::new(8.0, 0.0, 0.0),
            Vec3::new(8.0, 0.0, 4.0),
            Vec3::new(4.0, 0.0, 4.0),
        ];
        let polygons = vec![
            NavPolygon::new(0, 4),
            NavPolygon::new(4, 4).in_area(AreaId::new(1)),
        ];
        NavMesh::from_polygons(vertices, polygons).or_die()
    }

    /// An L: a 2 m arm and a 4 m arm meeting at a right angle.
    fn ell() -> NavMesh {
        let vertices = vec![
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(2.0, 0.0, 0.0),
            Vec3::new(2.0, 0.0, 2.0),
            Vec3::new(0.0, 0.0, 2.0),
            Vec3::new(2.0, 0.0, 0.0),
            Vec3::new(6.0, 0.0, 0.0),
            Vec3::new(6.0, 0.0, 2.0),
            Vec3::new(2.0, 0.0, 2.0),
        ];
        NavMesh::from_polygons(vertices, vec![NavPolygon::new(0, 4), NavPolygon::new(4, 4)])
            .or_die()
    }

    #[test]
    fn two_polygons_sharing_an_edge_are_neighbours() {
        let mesh = pair();
        assert_eq!(mesh.polygon_count(), 2);
        assert_eq!(mesh.neighbours_of(0).len(), 1);
        assert_eq!(mesh.neighbours_of(0).at(0).polygon, 1);
        assert_eq!(mesh.neighbours_of(1).at(0).polygon, 0);
        // The shared edge is 4 m long, which is 4 000 mm.
        assert_eq!(mesh.neighbours_of(0).at(0).cost, 4_000);
    }

    #[test]
    fn polygons_that_only_touch_at_a_corner_are_not_neighbours() {
        let vertices = vec![
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(2.0, 0.0, 0.0),
            Vec3::new(2.0, 0.0, 2.0),
            Vec3::new(0.0, 0.0, 2.0),
            Vec3::new(2.0, 0.0, 2.0),
            Vec3::new(4.0, 0.0, 2.0),
            Vec3::new(4.0, 0.0, 4.0),
            Vec3::new(2.0, 0.0, 4.0),
        ];
        let mesh =
            NavMesh::from_polygons(vertices, vec![NavPolygon::new(0, 4), NavPolygon::new(4, 4)])
                .or_die();
        assert!(mesh.neighbours_of(0).is_empty(), "a corner is not a door");
    }

    #[test]
    fn a_route_through_two_polygons_is_a_straight_line() {
        let mesh = pair();
        let route = mesh
            .path(
                Vec3::new(1.0, 0.0, 2.0),
                Vec3::new(7.0, 0.0, 2.0),
                AreaMask::all(),
            )
            .or_die();
        let last = route.last().or_die();
        assert!((last.position.x - 7.0).abs() < 0.001, "{route:?}");
        assert!(
            route.len() <= 3,
            "a straight crossing needed waypoints: {route:?}"
        );
    }

    #[test]
    fn a_masked_area_cannot_be_used_as_an_endpoint() {
        let mesh = pair();
        // The goal is inside polygon 1, which is in area 1 and masked out.
        assert!(
            mesh.path(
                Vec3::new(1.0, 0.0, 2.0),
                Vec3::new(7.0, 0.0, 2.0),
                AreaMask::of(AreaId::DEFAULT),
            )
            .is_none()
        );
    }

    #[test]
    fn a_route_does_not_cut_the_corner_of_an_l() {
        let mesh = ell();
        let route = mesh
            .path(
                Vec3::new(0.5, 0.0, 1.0),
                Vec3::new(5.5, 0.0, 1.0),
                AreaMask::all(),
            )
            .or_die();
        for point in &route {
            assert!(
                mesh.polygon_at(point.position).is_some(),
                "a waypoint left the mesh: {point:?}"
            );
        }
        // Every leg of the route must also stay on the mesh: sample the midpoints.
        for pair in route.windows(2) {
            let middle = (pair.at(0).position + pair.at(1).position) * 0.5;
            assert!(
                mesh.polygon_at(middle).is_some(),
                "a leg left the mesh: {:?} to {:?}",
                pair.at(0).position,
                pair.at(1).position
            );
        }
    }

    /// A ring of six quads: left low/high, middle low/high, right low/high. The middle low
    /// quad is the shortcut and the expensive one.
    fn ring() -> NavMesh {
        let quad = |x: f32, z: f32| {
            vec![
                Vec3::new(x, 0.0, z),
                Vec3::new(x + 2.0, 0.0, z),
                Vec3::new(x + 2.0, 0.0, z + 2.0),
                Vec3::new(x, 0.0, z + 2.0),
            ]
        };
        let mut vertices = Vec::new();
        for (x, z) in [
            (0.0, 0.0),
            (0.0, 2.0),
            (2.0, 0.0),
            (2.0, 2.0),
            (4.0, 0.0),
            (4.0, 2.0),
        ] {
            vertices.extend(quad(x, z));
        }
        let polygons = vec![
            NavPolygon::new(0, 4),
            NavPolygon::new(4, 4),
            NavPolygon::new(8, 4).in_area(AreaId::new(1)),
            NavPolygon::new(12, 4),
            NavPolygon::new(16, 4),
            NavPolygon::new(20, 4),
        ];
        NavMesh::from_polygons(vertices, polygons).or_die()
    }

    #[test]
    fn an_expensive_area_is_routed_around_when_there_is_another_way() {
        let mut mesh = ring();
        let from = Vec3::new(1.0, 0.0, 1.0);
        let to = Vec3::new(5.0, 0.0, 1.0);
        let mask = AreaMask::all();

        // Neutral: the shortcut through the middle quad wins.
        assert_eq!(
            mesh.corridor_between(from, to, mask).or_die(),
            vec![0, 2, 4]
        );
        let direct = mesh.route_cost(from, to, mask).or_die();
        assert_eq!(direct, 4_000, "two 2 m crossings at neutral cost");

        // Price the shortcut above the detour and the search takes the long way: four crossings
        // of 2 m each, which is cheaper than two at nine times the price.
        mesh.set_area_cost(AreaId::new(1), 900);
        assert_eq!(
            mesh.corridor_between(from, to, mask).or_die(),
            vec![0, 1, 3, 5, 4]
        );
        let detour = mesh.route_cost(from, to, mask).or_die();
        assert_eq!(detour, 8_000);
        assert!(detour < 2 * 2_000 * 9, "the detour did not win on cost");
    }

    #[test]
    fn a_mask_refuses_an_area_that_a_cost_only_ranks() {
        let mesh = ring();
        let from = Vec3::new(1.0, 0.0, 1.0);
        let to = Vec3::new(5.0, 0.0, 1.0);
        // Masking the expensive quad leaves exactly the detour, and a route still exists.
        let masked = AreaMask::all().without(AreaId::new(1));
        assert_eq!(
            mesh.corridor_between(from, to, masked).or_die(),
            vec![0, 1, 3, 5, 4]
        );
        // Masking both ways through the ring leaves nothing.
        let closed = masked.without(AreaId::new(0)).without(AreaId::new(1));
        assert!(mesh.corridor_between(from, to, closed).is_none());
    }

    #[test]
    fn a_degenerate_input_is_refused() {
        let collinear = vec![
            Vec3::ZERO,
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(2.0, 0.0, 0.0),
        ];
        assert!(matches!(
            NavMesh::from_polygons(collinear, vec![NavPolygon::new(0, 3)]).or_die_err(),
            NavMeshError::ZeroArea { index: 0 }
        ));
        assert!(matches!(
            NavMesh::from_polygons(vec![Vec3::ZERO], vec![NavPolygon::new(0, 2)]).or_die_err(),
            NavMeshError::DegeneratePolygon { index: 0, len: 2 }
        ));
        assert!(matches!(
            NavMesh::from_polygons(vec![], vec![]).or_die_err(),
            NavMeshError::Empty
        ));
    }

    #[test]
    fn a_vertex_index_past_the_array_is_refused() {
        let vertices = vec![
            Vec3::ZERO,
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(0.0, 0.0, 1.0),
        ];
        assert!(matches!(
            NavMesh::from_polygons(vertices, vec![NavPolygon::new(1, 3)]).or_die_err(),
            NavMeshError::VertexOutOfRange {
                index: _,
                vertex: _,
                count: _
            }
        ));
    }

    #[test]
    fn a_triangle_soup_builds_the_same_graph_as_a_welded_mesh() {
        // Two triangles sharing an edge but duplicating every vertex, which is what a
        // triangulator emits.
        let vertices = [
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(2.0, 0.0, 0.0),
            Vec3::new(2.0, 0.0, 2.0),
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(2.0, 0.0, 2.0),
            Vec3::new(0.0, 0.0, 2.0),
        ];
        let indices = [0, 1, 2, 3, 4, 5];
        let mesh = NavMesh::from_triangles(&vertices, &indices, AreaId::DEFAULT).or_die();
        assert_eq!(mesh.polygon_count(), 2);
        assert_eq!(
            mesh.neighbours_of(0).len(),
            1,
            "the duplicated vertices broke adjacency"
        );
    }

    #[test]
    fn a_point_resolves_to_the_lowest_polygon_on_a_seam() {
        let mesh = pair();
        assert_eq!(mesh.polygon_at(Vec3::new(4.0, 0.0, 2.0)), Some(0));
        assert_eq!(mesh.polygon_at(Vec3::new(30.0, 0.0, 2.0)), None);
        assert_eq!(mesh.nearest_polygon(Vec3::new(30.0, 0.0, 2.0)), Some(1));
    }

    #[test]
    fn a_baked_source_is_walkable_where_the_mesh_is_and_nowhere_else() {
        let bounds = NavBounds::new(Vec3::ZERO, 16, 8, 0.5).or_die();
        let source = NavMeshSource::bake(pair(), bounds).or_die();
        // 8 m by 4 m of mesh at 0.25 m^2 a cell is 128 cells; the seams cost a few.
        let walkable = source.walkable_cells();
        assert!(
            (110..=128).contains(&walkable),
            "unexpected walkable area: {walkable}"
        );
        // Cell (0, 0) is the corner of the mesh and *is* walkable; the far side of the grid is
        // outside every polygon and must not be.
        assert!(source.is_walkable(NavCell::new(0, 0)));
        assert!(
            !source.is_walkable(NavCell::new(20, 12)),
            "outside the mesh"
        );
        assert!(source.is_walkable(NavCell::new(9, 4)));
    }

    #[test]
    fn a_baked_source_solves_a_flow_field_across_the_mesh() {
        let bounds = NavBounds::new(Vec3::ZERO, 16, 8, 0.5).or_die();
        let source = NavMeshSource::bake(pair(), bounds).or_die();
        let target = NavCell::new(14, 4);
        let field = solve(&source, FlowTarget::at_cell(&source, target));
        assert!(field.reachable_cells() > 50);
        let start = NavCell::new(1, 4);
        assert!(field.cost_at(start).is_some_and(|cost| cost != u32::MAX));
    }

    #[test]
    fn area_masks_add_and_remove() {
        let mask = AreaMask::all().without(AreaId::new(3));
        assert!(mask.contains(AreaId::new(2)));
        assert!(!mask.contains(AreaId::new(3)));
        assert!(mask.with(AreaId::new(3)).contains(AreaId::new(3)));
        assert!(!AreaMask::NONE.contains(AreaId::DEFAULT));
    }

    #[test]
    fn a_route_is_the_same_twice() {
        let mesh = ell();
        let query = |mesh: &NavMesh| {
            mesh.path(
                Vec3::new(0.5, 0.0, 1.0),
                Vec3::new(5.5, 0.0, 1.0),
                AreaMask::all(),
            )
        };
        assert_eq!(query(&mesh), query(&mesh));
    }

    #[test]
    fn a_mesh_with_no_route_between_two_islands_returns_none() {
        let vertices = vec![
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(2.0, 0.0, 0.0),
            Vec3::new(2.0, 0.0, 2.0),
            Vec3::new(0.0, 0.0, 2.0),
            Vec3::new(10.0, 0.0, 0.0),
            Vec3::new(12.0, 0.0, 0.0),
            Vec3::new(12.0, 0.0, 2.0),
            Vec3::new(10.0, 0.0, 2.0),
        ];
        let mesh =
            NavMesh::from_polygons(vertices, vec![NavPolygon::new(0, 4), NavPolygon::new(4, 4)])
                .or_die();
        assert!(
            mesh.path(
                Vec3::new(1.0, 0.0, 1.0),
                Vec3::new(11.0, 0.0, 1.0),
                AreaMask::all()
            )
            .is_none()
        );
    }
}
