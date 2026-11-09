# Competitive analysis: crowd and NPC tooling in other engines

This page compares `bevy-flock` 0.1 against the crowd and NPC tooling that Unity, Unreal,
Godot, CryEngine, Havok and the surrounding Bevy crates document, at the versions named in
each subsection.

## What other engines ship

### Unity: Entities 1.0–1.5, AI Navigation 1.1–2.0, Entities Graphics 1.5.0, Behavior 1.0.16

Unity's DOTS runtime is Entities, an ECS implementation that organises state as entities and
components and is normally combined with Entities Graphics, Unity Physics or Netcode for
Entities rather than used as a complete NPC framework [1]. On top of it sit two separate
navigation layers. A baked NavMesh represents walkable space as connected convex polygons,
global pathfinding maps start and destination onto those polygons and searches the polygon
graph with A*, and local movement follows and adjusts the resulting corridor [2]. The NavMesh
Agent exposes destination following, acceleration, area masks, OffMesh Links and avoidance,
with an avoidance-quality setting that can be reduced for high agent counts [3]; the agent's
reported velocity is documented as coming from "the crowd simulation" [4].

Local avoidance in AI Navigation 2.0 is reciprocal velocity obstacles, evaluated against
other agents and against NavMesh edges, with the resulting position constrained back onto the
NavMesh [2]. Unity splits obstacle handling by kind: moving obstacles are left to local
avoidance, stationary blockers are handled by carving the NavMesh, carving is documented as
computationally expensive, and local avoidance is explicitly unable to route around a trap or
a fully blocking obstacle [2]. Behaviour authoring is a shipped package rather than a
framework: Behavior 1.0.16 for Unity 6000.0 provides top-to-bottom graph authoring, prebuilt
movement, detection and decision nodes, reusable subgraphs, C# integration, event-driven
observer aborts and Play Mode debugging [5]. The cited pages do not document utility scoring,
GOAP, environment queries, persistent social memory, a world director, dialogue graphs,
barks, formations or a tiered cognition scheduler as part of these packages [2][5].

Documented performance work in this stack is mostly about runtime layout and scheduling.
Baking runs at editor time and writes runtime data into Entity Scenes rather than converting
authoring objects during play [6]. The C# Job System is used throughout, with `Entities.ForEach`
and `Job.WithCode` for multithreaded logic and `IJobChunk.Schedule`/`ScheduleParallel` for
chunk-oriented work, while ECS tracks component read and write dependencies to decide which
jobs may run concurrently [7]. Entities Graphics draws DOTS meshes with instancing and LOD [8],
and Entities 1.4 documents asynchronous scene streaming with sections, meta entities and
instancing as the controls [9]. On the navigation side, Unity documents corridor adjustment
instead of recomputing a global route for small movements, voxel-size tradeoffs during baking
where finer voxels cost processing time, and carving cost [2]. CullingGroup provides
visibility and distance-band culling for groups of bounding spheres, but the page does not
document it as an AI cognition scheduler [10].

### Unreal Engine 5: Mass Entity, MassGameplay, MassCrowd, StateTree, ZoneGraph

Mass Entity is Unreal's data-oriented runtime: data-only Fragments are composed into
Archetypes held in memory Chunks, stateless Processors run batch EntityQueries, and
composition changes are deferred through a command buffer [11]. MassGameplay supplies the
gameplay layer: spawning, a choice between full Actors, low-resolution Actors, instanced
static meshes and no representation at all, automatic Actor pooling and recycling,
visual/simulation/replication LOD, StateTree integration, signals, movement and SmartObject
integration [12]. MassCrowd tracks Mass entities wandering a ZoneGraph and exposes lane
tracking, branching, wait areas and slots, density-weighted lane choice, crowd spawning,
crowd representation and LOD, plus replication [16].

Of the engines surveyed, Unreal documents the most complete behaviour-authoring stack.
Behaviour Trees use Blackboards [18], StateTree is a hierarchical state machine that mixes
behaviour-tree selectors with states and transitions [19], EQS selects environment items
through Generators, Contexts and Tests [20], and AI Perception supplies damage, hearing,
prediction, sight, team and touch stimuli with age and expiration plus known-versus-currently-
perceived queries [21].
Avoidance is deliberately plural: RVO and the Detour Crowd Manager are separate methods of the
navigation system [14], and Mass Avoidance is a third, force-based hash-grid system [17].
ZoneGraph and Mass Avoidance are marked Experimental in the current documentation, and Mass
Replication is documented as Experimental from UE 5.1 [22].

The performance story is LOD in three separate senses plus data layout. Simulation LOD emits
High, Medium, Low and Off, supports chunk grouping and variable-frequency updates, and visual
LOD adds distance and frustum grouping [12]. Detour Crowd uses adaptive RVO sampling with
visibility and topology path-corridor optimisations [14], and Mass Avoidance queries nearby
and closest entities through a navigation obstacle hash grid [17]. Mass Representation keeps
instanced static meshes cheap while pooling Actors in and out, with ISM animation documented
as Experimental [12]. The City Sample combines Mass AI with World Partition on-demand loading
and Data Layers, which the documentation presents as a sample integration rather than a
general NPC-count guarantee [13].

### Godot 4.7 with LimboAI 1.8.x

Godot's navigation is a server with optional agent nodes. NavigationAgent2D and 3D handle
pathfinding and path following while leaving movement to the parent actor's code [23][24],
and the same agents provide RVO avoidance with neighbour distance, maximum neighbours, time
horizons, radius, speed, layers and masks, and priority, with an explicit warning that many
registered agents carry significant cost [23][24]. The NavigationServer batches changes until
physics-frame synchronization and uses a thread pool by default for some functions, including
avoidance [25].

LimboAI is the behaviour layer Godot itself does not ship: editor-authored behaviour-tree
resources, a BTPlayer node, custom tasks, composites, decorators and conditions, reusable
subtrees, a visual debugger, in-game tree visualisation and performance monitors [32][33]. It
adds hierarchical state machines with event-based transitions, nested machines and a BTState
that hosts a tree inside a state [34], and blackboards with per-instance variables, typed
plans with defaults and hints, scopes and explicit tree-to-state-machine variable mapping [35].
`LimboUtility` is a helper API for comparisons, arithmetic, status names and formatting, not a
utility-scoring selector [36].

Godot's performance guidance is engine-level rather than AI-level. Low-level servers and RIDs
are the documented answer when node and scene-system overhead dominates, for workloads in the
tens of thousands of instances per frame [26]. MultiMesh covers thousands of instances and, for
simple objects, up to millions, at the cost of all-or-none visibility [28][29], RenderingServer
bypasses the node system only when that system is the bottleneck rather than the GPU [27], and
visibility ranges provide mesh-level LOD and HLOD-style switching [30]. Threading guidance is
to pre-create threads and treat locking as expensive [31].

### Bevy ecosystem crates: bevy_behave, big-brain, bevy_landmass, vleue_navigator, detour-crowd-rs

The Bevy side is modular rather than a stack. `bevy_behave` implements behaviour trees as
spawned task entities with Sequence, Fallback/Selector, Invert, While and IfThen nodes,
triggers, waits, subtree composition and event-based status reports, and its README tops out
at Bevy 0.19 [37]. `big-brain` is data-driven utility AI: Scorers produce Scores, Thinkers pick
Actions through picker policies, actions have continuous state and cancellation, evaluation is
parallelisable, and Bevy Reflect is used for debugging; the documented compatibility is Bevy
0.16 with later work on 0.17 [38][39].

Navigation is split across two crates. `bevy_landmass` turns navmeshes into desired movement
through archipelagos, islands, agents, path steps, agent states, pause markers and animation
links, in 2D and 3D [40][41]. `vleue_navigator` 0.17 supports Bevy 0.20 and provides Polyanya
navmesh pathfinding, prebuilt navmeshes, navmesh generation from obstacle components and live
navmesh updates, with steering behaviours explicitly listed as still to implement [42].
Avoidance comes from `detour-crowd-rs`, a standalone pure-Rust DetourCrowd port with RVO,
adaptive velocity-space sampling, corridor navigation, boundary and separation steering,
explicit agent states and a hashed proximity grid [43]. Bevy 0.20 itself supplies the ECS and
its parallelism model, not an NPC system [44][45].

### Bevy 0.16–0.20 performance guidance

Bevy's documented ECS guidance is access-driven. Table storage is described as fast and
cache-friendly for iteration while sparse sets favour insertion and removal [46], queries
should use narrow filters and minimal mutable access with change detection rather than broad
optional-component or `EntityRef` queries in hot paths [47], and `par_iter`/`par_iter_mut` plus
the `contiguous_iter` forms expose dense parallel iteration with no guaranteed ordering or
thread assignment [48]. The task pools, ComputeTaskPool, AsyncComputeTaskPool and IoTaskPool
,  are latency classes, not AI schedulers [49]. AssetServer exposes direct and recursive
dependency load-state checks, not a world-streaming system [50].

Release-level work is rendering-oriented. Bevy 0.16 improved transform propagation with
dirty-subtree skipping and reported 1.1 ms to 0.1 ms on a 127,515-object benchmark [51].
Bevy 0.17 documented BVH culling for virtual geometry and stated that asset streaming was not
yet implemented in that context [52]. Bevy 0.19 documented GPU batching, GPU visibility-range
checks, sparse uploads and more rendering parallelism [53]. The official examples cover
visibility-range LOD switching [54] and automatic instancing of same-mesh, same-material
entities [55]. None of the core material documents a behaviour-tree, utility-AI, GOAP, EQS,
perception, social-memory or crowd-simulation framework [46].

### CryEngine 3.8.6, Star Citizen (2021), Frostbite, Havok Navigation and academic work

CryEngine 3's manual documents AI perception, target tracking, communication, debug views, a
modular behaviour-tree editor and the navigation system [56], and its AI console reference
exposes the operational detail: crowd control in pathfinding, `ai_EnableORCA`, collision-
avoidance ranges, time horizons and obstacle horizons, formations and group debugging,
pathfinder quotas and time limits (`ai_MNMPathFinderQuota`, `ai_PathfinderUpdateTime`,
`ai_PathfindTimeLimit`), per-agent update intervals and navigation-system multithreading [57].
The documented performance controls are exactly those: update intervals, quotas, multithreading,
path beautification and per-agent timing diagnostics [57].

Star Citizen's official 2021 Social AI presentation covers engineering tasks, usable objects,
game tags and logic, animation through Mannequin, simulated broken objects and interaction
routing [58]. It is evidence of a task-and-interaction authoring workflow, not of a published
crowd architecture or population limit [58]. EA's public Frostbite page supports only broad
claims, large-scale multiplayer interactions in dynamic destructible environments, and highly
configurable workflows and runtimes spanning audio, animation, physics, destruction and
rendering, and publishes no AI or crowd manual [59].

Havok Navigation documents large-crowd collision avoidance with emergent lanes and swirling,
ground and volumetric navigation, hierarchical pathfinding, runtime navmesh updates, runtime
stitching, multi-radius navigation, navmesh section streaming with cluster graphs that route
through unloaded sections, and automatic traversal analysis [60]. Its 2026 technical post
describes spreading pathfinding and navmesh work across frames inside a time budget, with an
illustrative 3 ms foreground budget, coherent batch commit points and an explicit list of
tradeoffs: briefly stale routes, shifted emergent behaviour, temporary memory overlap during
streaming, and the need to coordinate thread and streaming policy [61].

The academic record supplies the algorithms. ORCA assigns each agent half the responsibility
for each pairwise collision and solves a low-dimensional linear program, with an open-source
RVO2 implementation and a 25,000-agent demonstration [63]. ClearPath targets data-level SIMD
and thread-level parallelism and reports 25,000 agents at 140 FPS and 1,000 at 4,500 FPS for
its implementation [64]. Aggregate Dynamics represents a dense crowd both discretely and
continuously, other work splits global from local navigation with cell decomposition or a
precomputed roadmap, and one paper reports emergent lane formation from a least-effort model
[62].

### Who ships what

| Stack | Global path planning | Local avoidance | Behaviour authoring | Scaling mechanism |
| --- | --- | --- | --- | --- |
| Unity: Entities 1.4, AI Navigation 2.0, Behavior 1.0.16 | Baked convex-polygon NavMesh, A* corridors, OffMesh Links, area masks [2][3] | RVO against agents and NavMesh edges, priorities, quality levels [2][3][4] | Visual graph, reusable subgraphs, observer aborts, Play Mode debugging [5] | Editor baking, job/chunk parallelism, reduced avoidance quality [6][7][3] |
| Unreal Engine 5: Mass, StateTree, ZoneGraph | ZoneGraph lanes with MassCrowd lane tracking [15][16] | Detour Crowd adaptive RVO; force-based Mass Avoidance (Experimental) [14][17] | Behaviour Trees with Blackboards, StateTree, EQS, AI Perception [18][19][20][21] | Fragment/chunk layout, simulation, visual and replication LOD [11][12] |
| Godot 4.7 + LimboAI 1.8 | NavigationServer maps, regions and path queries [23][24][25] | RVO with neighbour, horizon, layer and priority controls [23] | Editor BT resources, hierarchical state machines, blackboards, visual debugger [32][33][34][35] | Low-level servers, MultiMesh, visibility ranges [26][28][30] |
| CryEngine 3.8.6 | Navigation with quotas and time limits [57] | ORCA-related switches, avoidance ranges and horizons [57] | Modular behaviour trees, perception, target tracking [56][57] | Update intervals, pathfinder quotas, nav multithreading [57] |
| Havok Navigation | Hierarchical routing, streaming, cluster graphs, multi-radius [60] | Large-crowd avoidance with emergent lanes [60] | Not documented in the cited pages [60] | Multithreaded generation, async time-budgeted processing [60][61] |
| Bevy crates | `vleue_navigator` Polyanya navmesh, `bevy_landmass` islands [42][40] | `detour-crowd-rs` RVO and adaptive sampling [43] | `bevy_behave` trees, `big-brain` utility AI [37][38] | Bevy ECS parallelism and weak ordering [44][45] |
| bevy-flock 0.1 | Grid flow fields per destination; polygon navmesh only if the game supplies one through the port | Separation, alignment, cohesion, seek, arrive, no reciprocal avoidance | Compiled trees, utility scoring, guarded state machines, dialogue graphs | Virtual-cost scheduler, four cognition tiers, bands and continuum, cell-major SIMD |

## Where bevy-flock already matches

Budgeted cognition is table stakes, and the crate states its version of it precisely. CryEngine
exposes per-agent update intervals [57], Unity lets you cut avoidance quality as agent counts
rise [3], Unreal ties update frequency to simulation LOD [12], and Havok spreads navigation
work across a frame budget [61]. `bevy-flock` instead charges each logic unit a declared cost
into a start-time fair queue measured in virtual nanoseconds, which bounds the worst-case think
interval by formula rather than by a promotion rule, and the measurements are published: 88.7%
of 10,000 NPCs think inside a 16.6 ms budget each tick at 2.25 µs per think, and 12.1% of
100,000 (ADR 0009, ADR 0017).

Four-tier cognition matches, and exceeds, the tier mechanisms those engines document. Unreal's
simulation LOD emits High, Medium, Low and Off [12]; Godot's visibility ranges are rendering
LOD only [30]; Unity's CullingGroup is visibility and distance bands, not an AI scheduler [10].
`bevy-flock` selects reflex, reactive, deliberative or social logic per NPC with budget
reserved per tier, so a background NPC is cheap to think rather than merely slow to think
(ADR 0017), and past the manifest band the population becomes a density field (ADR 0018).

Behaviour trees as authored, validated data are universal: Unreal's tree and Blackboard pair
[18], Unity Behavior's graph [5], LimboAI's resources [32][33], `bevy_behave`'s task entities
[37]. `bevy-flock` ships the same category with six composites, four decorators, cooldown,
timeout and parallel nodes, allocation-free ticks, and validation at compile time so an empty
composite or a tree deeper than 64 levels is an authoring error rather than a runtime surprise
(ADR 0035). State machines with guards and dwell times cover the StateTree [19] and LimboHSM
[34] role, and utility scoring with curves, inhibition, count compensation and an explain
path covers the `big-brain` scorer/action model [38][39] (ADR 0036).

Neighbour-limited steering with a spatial index is the common shape. Godot's avoidance is
governed by neighbour distance and maximum neighbours [23], detour-crowd-rs uses a hashed
proximity grid instead of pairwise checks [43], and Mass Avoidance queries a navigation
obstacle hash grid [17]. `bevy-flock` queries a deterministic spatial grid for radius and
k-nearest neighbours in `(distance, NpcId)` order and steers from that buffer, at 138 ns for a
radius-8 m query at 1,000 agents and 552 ns at 100,000, with a cell-major SIMD kernel that is
bit-identical to the scalar reference (ADR 0023, ADR 0026).

Groups, formations and perception have counterparts elsewhere. CryEngine documents formations,
groups and crowd-control switches [57], and its AI reference exposes perception ranges,
alertness, target tracking and communication history [57]; Unreal ships AI Perception with six
stimulus types [21]; Unity Behavior supplies detection nodes [5]. `bevy-flock` ships faction
relations, formation slots with a group cap policy, encounter perception recording who noticed
whom and for how long, and a world director that drives mood and events (ADR 0021, ADR 0039).

Headless operation is where the architecture matches the server-side half of these engines.
Unity's rendering path is the Entities Graphics package [8]; Godot's is RenderingServer and
MultiMesh [27][28]; Bevy's is its renderer with automatic instancing [55]. `bevy-flock` has no
renderer dependency at all and exposes physics, replication and rendering as ports, a
`RenderPlan` of sorted, culled instances, a fixed-width `NpcNetState` wire format, and a
per-frame velocity export (ADR 0040), so a dedicated server runs the identical simulation with
`default-features = false`.

Three capabilities in the crate have no documented counterpart in the sources reviewed here:
optional social memory with a bounded slot store, cold archive and a versioned save format
(ADR 0010, ADR 0012); dialogue graphs with facts and validated-at-load conversation state
(ADR 0037); and barks with a bounded queue, per-category cooldowns and a per-frame cap
(ADR 0038). Absence from a vendor's manual is not proof of absence from the engine, so this
is stated as a documentation gap on their side, not a lead on ours.

## Gaps worth closing

Ranked by what a game developer hits first, not by what is easiest to build.

1. **Polygon navmesh pathfinding with area costs and masks.** The crate navigates a grid with
   flow fields to one destination per field, and offers a navigation port a game can implement
   against a baked navmesh (ADR 0040), but it does not bake or query polygon navmeshes itself.
   Unity represents walkable space as connected convex polygons, searches the polygon graph
   with A*, follows the corridor and applies area masks [2][3]; Godot exposes navigation maps
   and regions with path queries [25]; `vleue_navigator` ships Polyanya navmesh pathfinding on
   Bevy 0.20 [42]; `bevy_landmass` derives desired movement from navmesh islands [40]; Havok
   adds hierarchical routing and multi-radius navigation [60]. Without it, every game writes
   the same adapter before its crowd can walk around a building, which is the first thing a
   crowd is asked to do. **Cost: large**, bake pipeline, polygon adjacency, corridor
   following, and a determinism story for tie-breaking, none of which the current grid code
   transfers to.

2. **Reciprocal local avoidance.** Separation, alignment and cohesion keep a crowd from
   collapsing into one point; they do not keep two agents from occupying the same space.
   Unity documents RVO against agents and NavMesh edges with priorities and quality levels
   [2][3][4], Godot documents RVO with time horizons and layers [23], Unreal documents Detour
   Crowd and, separately, force-based Mass Avoidance [14][17], `detour-crowd-rs` ships a
   DetourCrowd port with adaptive velocity-space sampling [43], and ORCA formalises the
   half-responsibility linear program [63]. A crowd library whose avoidance is purely
   force-based produces visible interpenetration in doorways and at corners, which is where
   crowds are most visible. **Cost: medium**, the neighbour buffer and SIMD layout already
   exist; the work is the velocity-obstacle solve, per-agent priority and a determinism
   strategy for sampling.

3. **Navigation links.** Unity documents OffMesh Links for jumps, ladders and ledges [3],
   `bevy_landmass` documents animation links and pause markers [40], and Godot's navigation
   agents report reaching a `NavigationLink3D` as a path segment [23]. A flow field over a grid
   cannot express "this ledge is a shortcut" or "the ladder costs 4 seconds", so level designers
   either flatten their levels or add behaviour-tree hacks for traversal. **Cost: small** , 
   the port already carries the notion of cells and costs; a link table plus a cost injection
   into the Dijkstra is a contained change.

4. **Live navmesh and obstacle updates.** `bevy-flock` already versions cached flow fields so
   a door that opens invalidates fields built against the old layout, which is the same
   problem `vleue_navigator` solves by regenerating a navmesh from obstacle components and
   updating it live [42]. What is missing is a source that rebuilds itself: Unity carves the
   NavMesh for stationary blockers and documents the cost [2], Havok exposes runtime navmesh
   updates and dynamic edges [60]. In practice the game must rebuild and re-stamp its
   `NavSource` by hand, and forgetting to is a silent bug where the crowd walks through a wall.
   **Cost: small**, the versioning and cache invalidation exist; the work is a built-in
   obstacle-driven source and a documented rebuild budget.

5. **Representation LOD and pooling as a contract.** Unreal documents a four-way choice
   between full Actors, low-resolution Actors, instanced static meshes and no representation,
   with automatic Actor pooling and recycling [12]; Godot documents MultiMesh with all-or-none
   visibility and visibility ranges for LOD [28][29][30]; Bevy documents visibility-range LOD
   and automatic instancing in the renderer [54][55]. `bevy-flock` emits a `RenderPlan` and
   leaves the decision entirely to the adapter (ADR 0040), so every game reinvents the
   distance thresholds and the variant switch. **Cost: medium**, the data already exists in
   the plan and in the tiers; what is missing is a documented band-to-variant mapping that
   adapters can share.

6. **Stimulus-based perception.** The crate's encounter perception records who noticed whom
   and for how long, which is a social-memory input rather than a senses model. Unreal
   documents sight, hearing, damage, prediction, team and touch stimuli with age and
   expiration [21], and CryEngine exposes perception ranges, alertness and target tracking
   [57]. Without occlusion, hearing radii and stimulus decay, stealth and guard behaviour ends
   up as a special case inside a behaviour tree. **Cost: medium**, it needs a query against
   the existing spatial grid plus per-NPC stimulus state, and it interacts with the scheduler
   because perception cannot run at reflex cadence for every NPC.

7. **A navigation work budget with coherent commits.** Cognition is budgeted; navigation is
   not. Havok documents spreading pathfinding and navmesh work across frames inside a time
   budget, committing in coherent batches so groups never observe a half-updated navigation
   state, and lists the tradeoffs explicitly, briefly stale routes, shifted emergent timing,
   and temporary memory overlap during streaming [61]. CryEngine documents the same idea as
   pathfinder quotas and time limits [57]. The crate's flow-field solver is already resumable
   with a node budget per call, so the missing part is a per-frame budget across many fields
   and a commit point that observers can rely on. **Cost: medium**, mostly scheduler work,
   but it must be expressed in virtual cost to keep the determinism guarantee intact.

8. **Environment queries in the EQS sense.** Unreal's EQS selects among candidate environment
   items through Generators, Contexts and Tests [20]. Crowds ask this constantly, which
   bench, which cover point, which stall, and today each game writes it against the spatial
   grid. **Cost: medium**, the grid supplies candidates cheaply; the work is a scoring
   vocabulary and a way to keep queries out of the per-tick path.

9. **Visual authoring and live graph debugging.** Unity Behavior ships graph authoring,
   reusable subgraphs and Play Mode debugging [5]; LimboAI ships editor-authored tree
   resources, a visual debugger, in-game tree visualisation and performance monitors [32][33];
   CryEngine documents behaviour-tree debug drawing and failed-action views [57]. The crate
   offers `explain_npc` as a one-call state dump and a debug overlay as data (ADR 0042), which
   is the runtime half of the story, but authoring is still Rust source. **Cost: large**, an
   editor is a product of its own, and Bevy has no first-party node-graph editor to build on.
   Ranked last because Bevy teams are used to authoring in code, and a bad editor is worse than
   none.

## Performance techniques we could still adopt

**Hierarchical or cluster-based routing over a streamed navmesh.** Havok routes through
unloaded sections via cluster graphs and hierarchical pathfinding [60], and Unity's corridor
adjustment avoids recomputing a global route for every small movement [2]. Payoff: routing
stops scaling with world size and the crowd keeps moving through content that is not resident.
Risk: a bake pipeline and a streaming contract that the crate has no place for today, and
non-determinism if section boundaries change between replays.

**Adaptive velocity-space sampling with topology awareness.** Detour Crowd samples the velocity
space adaptively and uses visibility and corridor topology to prune candidates [14]. Payoff:
far fewer stuck agents in dense corridors at the same per-agent cost. Risk: sampling is a
source of nondeterminism, so it must draw from the per-NPC stream, which the crate already
guarantees for everything else (ADR 0002).

**Time-budgeted navigation with batch commits.** Havok's documented pattern is a small
foreground budget, asynchronous work spread across frames, and a coherent commit so observers
never see a partial state, with route staleness named as the price [61]. Payoff: path updates
stop causing frame spikes when a door opens or a chase starts. Risk: routes whose freshness
depends on frame count break replay, so the budget must be virtual nanoseconds like the think
scheduler (ADR 0009).

**Chunk-grouped variable-frequency updates.** Unreal's simulation LOD updates groups of chunks
at different frequencies rather than every entity every frame [12]. Payoff: update cost tracks
what the player can see instead of the population. Risk: neighbours updated on different
cadences see inconsistent steering inputs, which shows up as shimmer at group boundaries.

**Dirty-subtree propagation instead of full recomputation.** Bevy 0.16's transform propagation
skips subtrees with no changed ancestors and took a 127,515-object benchmark from 1.1 ms to
0.1 ms [51]. Payoff: the same trick applies to group and formation hierarchies, where a squad
that has not moved should cost nothing. Risk: dirty flags are exactly the state that a snapshot
must serialise, or a reload silently loses part of the hierarchy.

**Task pools for off-frame planning.** Bevy documents ComputeTaskPool for work needed before
the next frame, AsyncComputeTaskPool for work spanning frames and IoTaskPool for I/O [49], and
Godot's guidance is to pre-create threads because creation and locking are expensive [31].
Payoff: tree compilation, bake work and long path searches leave the frame entirely. The crate
adopted this for steering in ADR 0044 (2.0x at 10,000 agents, 2.8x at 100,000 on six threads), under
a rule worth keeping: per-entity writes only, and any reduction sorted before accumulation, per the
architecture's determinism clause. Risk elsewhere: every new use of the pool has to answer the same
question about which state is canonical, and a pool with one thread is indistinguishable from no
pool at all, which is exactly what the `bevy_tasks` `multi_threaded` feature defaults to.

**Narrow queries, change-detection filters and contiguous iteration.** Bevy documents
change-detection filtering at O(a+n) [47], contiguous dense iteration suitable for vectorisation
[48], and a storage choice between tables and sparse sets [46]. Payoff: unchanged NPCs get
skipped rather than re-evaluated, and dense passes vectorise without hand-rolled layouts. Risk:
the crate uses one `Added<Npc>` filter and no `Changed` filtering and, outside the threaded
steering pass of ADR 0044, no `par_iter` and no contiguous iteration, and missed-change bugs are silent; change detection also has to
stay out of the snapshot's definition of canonical state.

**Sparse-set storage for memory rows.** Bevy's storage guidance favours sparse sets for
insertion and removal [46], which is the shape of social-memory enrolment and eviction. Payoff:
cheaper churn when a player walks through a market and hundreds of rows are created and
dropped. Risk: iteration over memory during a think gets slower, and that is the path that
runs every tick.

**SIMD inside the avoidance pass.** ClearPath targets data-level and thread-level parallelism
for collision avoidance and reports 25,000 agents at 140 FPS for its implementation [64].
Payoff: the four-wide packing already used for steering extends to velocity-obstacle solves,
so avoidance need not eat the margin the kernel bought. Risk: the transposed layout was chosen
for neighbour-major steering (ADR 0030), and an avoidance solve is gather-heavy, so the win
may not survive contact with the data.

## References

[1]: https://docs.unity3d.com/Packages/com.unity.entities@1.4/manual/index.html "Entities package 1.4 manual"

[2]: https://docs.unity3d.com/Packages/com.unity.ai.navigation@2.0/manual/NavInnerWorkings.html "AI Navigation 2.0: Inner Workings of the Navigation System"

[3]: https://docs.unity3d.com/Packages/com.unity.ai.navigation@1.1/manual/NavMeshAgent.html "AI Navigation 1.1: NavMesh Agent component reference"

[4]: https://docs.unity.com/en-us/engine/6000.6/script-reference/unityengine/ai/navmeshagent/velocity "Unity 6.6: NavMeshAgent.velocity"

[5]: https://docs.unity.com/en-us/engine/6000.0/manual/packages-list/packages-all/pack-safe/com-unity-behavior "Unity 6.0 released package: Behavior 1.0.16"

[6]: https://docs.unity3d.com/Packages/com.unity.entities@1.5/manual/baking-overview.html "Entities 1.5: Baking overview"

[7]: https://docs.unity3d.com/Packages/com.unity.entities@1.0/manual/systems-scheduling-jobs.html "Entities 1.0: Use the job system with Entities"

[8]: https://docs.unity3d.com/6000.0/Documentation/Manual/com.unity.entities.graphics.html "Unity 6.0: Entities Graphics 1.5.0"

[9]: https://docs.unity3d.com/Packages/com.unity.entities@1.4/manual/streaming-scenes.html "Entities 1.4: Scene streaming"

[10]: https://docs.unity3d.com/6000.4/Documentation/Manual/CullingGroupAPI.html "Unity 6.4: CullingGroup API"

[11]: https://dev.epicgames.com/documentation/unreal-engine/overview-of-mass-entity-in-unreal-engine "Overview of Mass Entity in Unreal Engine"

[12]: https://dev.epicgames.com/documentation/unreal-engine/overview-of-mass-gameplay-in-unreal-engine "Overview of Mass Gameplay in Unreal Engine"

[13]: https://dev.epicgames.com/documentation/unreal-engine/city-sample-project-unreal-engine-demonstration "City Sample project in Unreal Engine"

[14]: https://dev.epicgames.com/documentation/unreal-engine/using-avoidance-with-the-navigation-system-in-unreal-engine "Using avoidance with the Navigation System in Unreal Engine"

[15]: https://dev.epicgames.com/documentation/unreal-engine/API/PluginIndex/ZoneGraph "ZoneGraph plugin API index"

[16]: https://dev.epicgames.com/documentation/unreal-engine/API/Plugins/MassCrowd "MassCrowd plugin API"

[17]: https://dev.epicgames.com/documentation/unreal-engine/mass-avoidance-overview-in-unreal-engine "Mass Avoidance overview in Unreal Engine"

[18]: https://dev.epicgames.com/documentation/unreal-engine/behavior-trees-in-unreal-engine "Behavior Trees in Unreal Engine"

[19]: https://dev.epicgames.com/documentation/unreal-engine/statetree-quick-start-guide "StateTree Quick Start Guide"

[20]: https://dev.epicgames.com/documentation/unreal-engine/environment-query-system-in-unreal-engine "Environment Query System in Unreal Engine"

[21]: https://dev.epicgames.com/documentation/unreal-engine/ai-perception-in-unreal-engine "AI Perception in Unreal Engine"

[22]: https://dev.epicgames.com/documentation/unreal-engine/unreal-engine-5-8-release-notes "Unreal Engine 5.8 release notes"

[23]: https://docs.godotengine.org/en/stable/classes/class_navigationagent3d.html "Godot: NavigationAgent3D class reference"

[24]: https://docs.godotengine.org/en/stable/tutorials/navigation/navigation_using_navigationagents.html "Godot: Using NavigationAgents"

[25]: https://docs.godotengine.org/en/stable/tutorials/navigation/navigation_using_navigationservers.html "Godot: Using NavigationServers"

[26]: https://docs.godotengine.org/en/stable/tutorials/performance/using_servers.html "Godot: Using Servers for performance"

[27]: https://docs.godotengine.org/en/stable/classes/class_renderingserver.html "Godot: RenderingServer class reference"

[28]: https://docs.godotengine.org/en/stable/classes/class_multimesh.html "Godot: MultiMesh class reference"

[29]: https://docs.godotengine.org/en/stable/tutorials/performance/using_multimesh.html "Godot: Using MultiMesh"

[30]: https://docs.godotengine.org/en/stable/tutorials/3d/visibility_ranges.html "Godot: Visibility ranges"

[31]: https://docs.godotengine.org/en/stable/tutorials/performance/using_multiple_threads.html "Godot: Using multiple threads"

[32]: https://github.com/limbonaut/limboai "limbonaut/limboai repository"

[33]: https://limboai.readthedocs.io/en/stable/index.html "LimboAI documentation"

[34]: https://limboai.readthedocs.io/en/stable/hierarchical-state-machines/create-hsm.html "LimboAI: Creating a hierarchical state machine"

[35]: https://limboai.readthedocs.io/en/stable/behavior-trees/using-blackboard.html "LimboAI: Using the blackboard"

[36]: https://limboai.readthedocs.io/en/stable/classes/class_limboutility.html "LimboAI: LimboUtility class reference"

[37]: https://github.com/RJ/bevy_behave "bevy_behave repository"

[38]: https://codeberg.org/zkat/big-brain "big-brain repository (Codeberg)"

[39]: https://github.com/zkat/big-brain "big-brain repository (GitHub)"

[40]: https://docs.rs/bevy_landmass/latest/bevy_landmass/ "bevy_landmass API documentation"

[41]: https://github.com/andriyDev/bevy_landmass "andriyDev/bevy_landmass repository"

[42]: https://github.com/vleue/vleue_navigator "vleue_navigator repository"

[43]: https://github.com/bhubbard/detour-crowd-rs "detour-crowd-rs repository"

[44]: https://bevy.org/learn/quick-start/getting-started/ecs/ "Bevy: ECS quick start"

[45]: https://bevy.org/news/bevy-0-20/ "Bevy 0.20 release notes"

[46]: https://docs.rs/bevy_ecs/0.20.0/bevy_ecs/ "bevy_ecs 0.20.0 crate documentation"

[47]: https://docs.rs/bevy_ecs/0.20.0/bevy_ecs/system/struct.Query.html "bevy_ecs 0.20.0: Query"

[48]: https://docs.rs/bevy_ecs/0.20.0/bevy_ecs/query/struct.QueryState.html "bevy_ecs 0.20.0: QueryState"

[49]: https://docs.rs/bevy_tasks/0.20.0/bevy_tasks/ "bevy_tasks 0.20.0 crate documentation"

[50]: https://docs.rs/bevy_asset/0.20.0/bevy_asset/struct.AssetServer.html "bevy_asset 0.20.0: AssetServer"

[51]: https://bevyengine.org/news/bevy-0-16/ "Bevy 0.16 release notes"

[52]: https://bevyengine.org/news/bevy-0-17/ "Bevy 0.17 release notes"

[53]: https://bevyengine.org/news/bevy-0-19/ "Bevy 0.19 release notes"

[54]: https://bevyengine.org/examples/3d-rendering/visibility-range/ "Bevy: visibility range example"

[55]: https://bevyengine.org/examples/shaders/automatic-instancing/ "Bevy: automatic instancing example"

[56]: https://www.cryengine.com/docs/static/engines/cryengine-3/categories/1114113 "CRYENGINE 3 manual: AI"

[57]: https://www.cryengine.com/docs/static/engines/cryengine-3/categories/9895942/pages/9215946 "CRYENGINE 3 AI console reference"

[58]: https://www.youtube.com/watch?v=-OaMjK9nfzE "Star Citizen: Building NPC Behaviors (2021)"

[59]: https://www.ea.com/frostbite/engine "Frostbite engine"

[60]: https://www.havok.com/havok-navigation/ "Havok Navigation"

[61]: https://www.havok.com/blog/asynchronous-navigation-processing/ "Havok: Asynchronous navigation processing"

[62]: https://gamma.cs.unc.edu/research/crowds/ "UNC GAMMA: crowd and multi-agent simulation research"

[63]: http://gamma.cs.unc.edu/ORCA/ "ORCA: Reciprocal Collision Avoidance"

[64]: http://gamma.cs.unc.edu/CA/ "ClearPath: scalable collision avoidance"
