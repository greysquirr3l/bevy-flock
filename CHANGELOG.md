# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## Unreleased
- **The Bevy adapter patches its spatial index instead of rebuilding it every tick.** The incremental path existed, was
  measured, and was used by the benchmarks only: the adapter rebuilt the whole index from scratch on every tick, so a
  crowd paid for a full recount of a table that was usually still right. Each NPC now owns a slot in the grid's entry
  array, which is what lets the patch compare a body's cell now with the cell it was in, and the adapter reports what it
  did. `FlockStats` counts patched ticks and rebuilt ticks, and the new `FlockGridReport` resource holds the last outcome,
  which names the cause when a rebuild was needed. A crowd that stands still rebuilds once, on the tick it is built, and
  patches nothing after that. A spawn, a despawn, or a tick where more bodies cross a cell boundary than the patch can
  absorb falls back to the rebuild, and the cause says which of those it was.
- **The plugin's spatial grid is the one its configuration asks for.** `BoidsConfig::grid` was documented as the grid
  the perception pass builds and was used to validate spawn positions, but the index itself was always built from the
  default configuration. A game that set a cell size or a cell budget got the default grid and a validation that
  disagreed with it. The grid is now built from the configuration, which is what the field has always said it was.
- **A cleared spatial grid no longer answers about the population it used to hold.** `SpatialGrid::clear` emptied the
  entries and kept the cell memory, and that memory is what the patch compares against, so a grid that had been cleared
  and refilled reported that nothing had moved and kept the old population's cells. A rebuild after a clear now recounts
  every body, which is what a grid that has forgotten everything should say.
- **The invariant audit no longer calls a grid unsorted for keeping its entries in slot order.** A cell's run is written
  in the order the bodies arrived, so a grid whose entries are not in identity order have runs that are not in identity
  order either. `check_invariants` applied the sort check to those grids anyway and named the grid's own contract a
  fault. It applies the check to a grid built from identity-ordered entries now, and skips it for one that was not.
- **The scaling gate stopped blinding itself on a noisy runner.** A shared runner hands a cell the occasional run twice
  as long as the others, and the spread the gate reads as noise was the difference between the best and the worst of
  every sample — a range, which the worst sample the machine produced sets. One stolen run in the sequential cell
  therefore set the noise floor for every row that divides by it, and the gate reported "cannot verify" on runs whose
  measured speedups were all within 4-9% of the baseline. The spread is now the uncertainty of the number the gate
  compares: a robust standard error on the median, which one stolen run cannot move. The raw range and the samples
  themselves are recorded beside it, and CI measures four repeats per cell and eight for the sequential one, which is
  what lets the median hold against that run. No threshold moved.
- **The scaling gate stopped comparing hardware threads with cores.** A "four-core" row was four logical CPUs, and on
  a machine with SMT that is two cores' worth of parallelism: the CI runner measured 2.29x where the six-core baseline
  measured 3.73x, and the gate read that 38% loss as a regression in the crawl. It was the runner's topology, not the
  code. The sweep now counts the machine's physical cores from its own topology (`scripts/cpu_topology.py`), pins one
  CPU per core, and records the cores, the threads and the pinned CPUs in every row; a width wider than the machine
  has cores is skipped with a note rather than measured. The gate compares a candidate's widths with the baseline's
  only where both count real cores, and names the widths it left out: the committed baseline predates topology
  recording, so its wider rows count threads and CI compares widths 1 and 2. A red build that was a hardware artefact,
  fixed without touching a threshold.
- **The band boundary gate now measures the band, not the test harness.** The allocation counter in
  `tests/band_continuity.rs` was process-wide, so it read whatever the other four gates happened to be allocating as
  if it belonged to the frame under test, and the gate then asserted that a steady-state frame allocated *at least
  once* — a claim about the platform, not about this crate. It passed on one platform and failed on another with a
  frame that genuinely allocated nothing. The counter counts the thread the frame runs on now, the harness records
  that thread so a schedule which moved the systems elsewhere fails the gate rather than passing it vacuously, a
  deliberate allocation proves the counter before it is read, and the gate asserts what ADR 0022 claims: 600 steady
  frames allocate nothing at all. Measured on Linux and macOS.
- **The incremental rebuild no longer gives up on a clustered arrival.** A cell whose run is full had forced a full
  rebuild, which made a crowd flowing into one district the worst case for the patch: at 512 crossers arriving inside a
  four-cell hotspot, 0% of ticks patched and the median tick was the rebuild (2.58 ms at 100,000 bodies, 13.98 ms at a
  million). The patch now borrows a slot from the first run after the full one that has a spare, moving the boundaries
  between them and copying each donor's run one slot up, bounded by `BORROW_MAX_ELEMENTS` (elements shifted) and
  `BORROW_MAX_RUNS` (consecutive full runs). The same ticks now patch every time: 0.347 ms and 1.305 ms, 7.4x and
  10.7x, with p99 falling from 3.42 ms and 16.68 ms to 0.62 ms and 1.68 ms. `RebuildOutcome::Rebuilt` also carries a
  `RebuildCause` now (`Population`, `TooManyMovers`, `StaleTable`, `NarrowRun`), so a game can tell which of the four
  reasons its tick rebuilt, and `examples/patch_tail` grew `--arrival cluster --cluster-radius` to reproduce the case.
- **The docs and this site were audited against the tree, and eleven links were broken.** Moving `docs/adr` to
  `docs/dev/adr` and the tuning material deeper left relative links pointing at directories that no longer exist:
  ADR images, demo captures, the playground README, three charts in the performance journal. Markdown does not
  complain, so `scripts/check_docs_links.py` now resolves every relative link in `docs/`, the site and the crate
  READMEs, and runs in CI with a self-test.
- **Four corrections on the site.** The performance page said "five passes" (there are four since the phase-5 fusion),
  and its rebuild table labelled the pre-fix six-worker total as a serial reference, turning a 27% wall-clock
  improvement into a 1.38x story about threads. Both are now stated with the same-binary denominator, and the page
  gained the same-machine Godot comparison. The install page's second table was missing `bevy-flock-ecs` and had no
  lead-in; the internals page still ended the ADR index at 0060.
- **The Unreal side of the cross-engine benchmark exists.** `benchmarks/unreal/` is a `UTickableWorldSubsystem`
  running the W1 fixture in Unreal's idiom: SoA arrays, a counting-sort grid rebuild each tick, bounded separation and
  a fixed 60 Hz step, printed as the same JSON line the other harnesses print. A subsystem means no map, no actor and
  no blueprint: adding the module is the install. `scripts/cross_bench.py --engines unreal --unreal-cmd ...` drives it
  through an `UnrealEditor-Cmd -game -nullrhi` run and folds the row into the table, and its `--self-test` proves that
  path against a fake command. The harness is written against the UE 5.x API but has not been compiled against an
  engine here.
- **The cross-engine benchmark actually ran, on one machine.** `benchmarks/godot/` is a committed project with
  command-line parameters and two modes: `array`, which is a lower bound for Godot because it has no spatial index, and
  `grid`, which rebuilds a uniform grid each tick and is the like-for-like row. `scripts/cross_bench.py` runs both
  engines with identical arguments and prints the table through `compare_benchmarks.py`. First result on a six-core
  aarch64 VM: 2.5x faster than the GDScript grid rebuild, 1.9x slower than a bare GDScript loop at 10k. The Unreal row
  remains open; UE5 does not fit in this sandbox.
- **The README's first block compiles in a crate with one dependency.** It used to open with `use bevy_app::App;`,
  which fails with `error[E0432]: unresolved import 'bevy_app'` after `cargo add bevy-flock`, because a transitive
  dependency is not importable. The block now takes `App` from the prelude, and `tests/facade_paths.rs` compiles it as
  a test that imports nothing but the crate itself. The site's first-crowd page had the same defect plus an unused
  `SimClock` import.
- `tests/facade_paths.rs` now pins the published paths: module-path resolution, prelude-to-module type identity, and
  facade-to-crate type identity, so a path rename breaks a test instead of a downstream build. It runs in the
  `--no-default-features` configuration too, where only the core paths are in play.
- **The four crates are publishable, and something checks it.** `bevy-flock-core` still carried `publish = false`
  from the extraction, had no `readme`, `keywords`, `categories` or docs.rs table, and `bevy-flock-ecs` and
  `bevy-flock-anim` declared a `readme` that did not exist. `scripts/publish_check.py` now verifies the metadata
  crates.io requires, that every `path` dependency also carries a satisfiable `version`, that no publishable crate
  depends on a `publish = false` one, and prints the publish order; CI runs it and its self-test on every push.
- **The animation layer is a `bevy-flock` feature now.** `features = ["anim"]` (and therefore
  `features = ["full"]`) turns on blended clip weights, gaze and gesture playback. Before this the layer lived in
  `bevy-flock-anim`, which depended on `bevy-flock`, so a feature could never include it: Cargo rejects the cycle. The
  layer is a real crate again, this time depending on `bevy-flock-ecs` rather than on the facade, so there is no
  compatibility shim to carry (ADR 0062).
  See ADR 0061.
- **The animation bench, baseline and no-allocation test moved with the code**, to `benches/anim_binding.rs`,
  `benches/anim-baseline.json` and `tests/anim_no_alloc.rs`. The CI gate reads the new baseline path.
- **Two configuration warnings are gone.** `block_bases`, `prefix_bases` and an orphaned `build_parallel` stub were
  dead on the no-threads path; they are now gated or deleted, so `--no-default-features` builds clean.
- **A `full` feature, so "everything" is one line.** `features = ["full"]` turns on `ecs`, `simd`, `threads`, `memory`,
  `serde` and `navmesh`. Before this, a game that wanted memory and saves had to read the feature table and guess the
  combination. The animation layer is still a separate crate and cannot be folded in: `bevy-flock-anim` depends on
  `bevy-flock`, so a re-export would be a cycle.
- **`examples/everything.rs` is the adoption path in one file**: add the plugin, ask for a crowd, run frames, read the
  counts, save a memory snapshot and load it back. Ninety lines, no renderer, and the first example a newcomer should
  read. Run it with `cargo run --release --example everything --features memory,serde`.
- **The crate docs stop understating the crate.** The status section still described the scheduler, continuum, dialogue,
  snapshots and render adapters as future work when all five ship, and the feature table listed one of seven. Both now
  match the code, and the docs name `bevy-flock-anim` and `bevy-flock-core` where a reader decides what to depend on.
- **The doc gate is green again.** Nine broken or private intra-doc links had accumulated, two of them pointing at items
  that no longer exist (`NavError::BadBitmap`, `NavGrid::bump`), so `RUSTDOCFLAGS="-D warnings" cargo doc` failed on
  `main`. Fixed at the source: the links name real items, and the two stale claims now describe the API that is there.
- **Documentation is now three things instead of one pile.** An Astro + Starlight site in `site/` covers install, a first
  crowd, concepts, guides and reference, published at <https://greysquirr3l.github.io/bevy-flock/>. Everything written for
  maintainers moved under `docs/dev/` (ADRs, tuning journals, gates, benchmarks, lint policy, release checklists) and is
  linked from the site rather than duplicated. `README.md` was cut to what a newcomer needs: the logo, the five-line
  example, what the crate does, how to verify it in ten minutes, and where to read next.
- **Neither ships.** Both crates exclude `docs/`, `site/`, `benchmarks/`, `crates/`, `scripts/` and the demo crates from the
  package tarball. `cargo package --list` for `bevy-flock` contains no documentation directory.
- **The simulation is its own crate.** `bevy-flock-core` now holds `domain`, `behavior`, `application`, `error` and
  `testkit` with no Bevy app in its dependency graph, so a headless server, a replay tool or a test harness can run a crowd
  without an engine. `bevy-flock` re-exports every path that moved, so existing code compiles unchanged; the only edit
  needed is if you depended on the crate with `default-features = false`, where the `ecs` feature now decides whether the
  three `Resource` derives exist. Feature names forward unchanged: `simd`, `threads`, `memory`, `serde`, `navmesh`.

- **A fourth naming culture, and the names stop shouting.** `NameStyle` gains `Highland` — broad
  vowels, gutturals, and surnames taken from places rather than trades — so a world seed can land on
  one of four cultures instead of three. A blueprint now carries its own style, and a collision is
  broken with an epithet from that culture: a Highland called "of the Glen" rather than "of the
  Fjord". The second syllable's onset is also lowered now, which fixes a bug that predates this
  change and affected every style: the onset pools are capitalised because the first one opens the
  name, so a mid-word onset kept its capital too, and names came out as "SkaNaum" and "TaQuaeus".
  They are now "Skanaur" and "Taquaeus".
- **The GPU demo draws on an Apple GPU.** `demo-gpu` asked `wgpu` for Vulkan, GL and the noop
  device, and a Mac has neither of the first two, so every run on Apple silicon fell through to
  the device that encodes commands and draws none: a blank image while the hardware sat idle. The
  Metal backend is now enabled and `Backend::Metal` can be asked for by name, and `auto` asks for
  Metal first — a Mac with `MoltenVK` installed would otherwise answer with a translation layer,
  and a frame time measured through one describes the layer rather than the hardware. The Apple
  dependencies are `cfg`-gated inside `wgpu`, so a Linux runner compiles the Metal backend away and
  the noop CI job is unaffected.
- **The asset pipeline skills move to `.github/skills/`, and CI stops checking them.** The four
  playbooks are agent-facing tooling rather than crate source, so they live under `.github/` where
  the tooling reads them and are ignored by Git. Three `scripts` job steps went with them — the
  `py_compile` of the Blender scripts and the two runs of the animation contract checker — because
  CI checks out the repository and those files are no longer in it. A workflow that names a path
  the runner does not have fails on every push, which is worse than the check it was carrying.
- **The lint spine now actually holds.** Eight lints that had been allowed with a paragraph each —
  `needless_pass_by_value`, `cast_precision_loss`, `cast_possible_truncation`, `cast_sign_loss`,
  `cast_possible_wrap`, `float_cmp`, `missing_const_for_fn`, `significant_drop_tightening` — are
  denied across the library, tests, benches and examples, which is what `Cargo.toml` claimed while
  VS Code reported the opposite. Every cast goes through `src/domain/num.rs`, which says what each
  conversion does with the bits; float equality is compared as `to_bits()`; Bevy systems carry the
  only `#[expect(needless_pass_by_value)]`s, because `&Res<T>` is not a `SystemParam`.
- **`suboptimal_flops` is allowed, and that is a numerical decision.** Fusing the kernels by hand
  made `application::packed::tests::the_vectorised_kernel_agrees_with_the_scalar_one` fail on a
  one-ulp difference: a fused multiply-add rounds once where the scalar reference rounds twice.
  `scripts/lint_audit.py` now enforces three documented `#[expect]` rules instead of two, and the
  Criterion guard in `benches/` is the third.
- **CI is reproducible without Actions.** `scripts/ci_local.py` reads `.github/workflows/ci.yml` and
  runs the `run:` steps itself, expanding each matrix and substituting it into the commands, so the
  local gate cannot drift from the real one. It skips the steps that provision a runner, refuses any
  job it cannot translate faithfully rather than running part of it, skips jobs pinned to another OS,
  and names the two jobs that are not shell (`release` is tag-gated, `deny` is an action).
  `--check` and `--self-test` run inside CI, so a workflow shape the translator cannot follow fails
  the build instead of quietly thinning the local gate (21 jobs, 35 local runs).
- **Temperament reaches the simulation, and this is a behaviour change.** A personality now scales
  that NPC's own steering weights — five multipliers on `[separation, alignment, cohesion, seek,
  arrive]`, centred on `1.0` and clamped to `0.25 ..= 4.0` so a game's tuning stays the baseline —
  and so do unmet needs, which decay on their own every eight ticks by default
  (`BoidsConfig::needs`, `NeedsDecay`). The measurable effect: guards hold rank with the highest
  alignment (1.164) and lowest separation (0.918), urchins the reverse, merchants cluster hardest
  (cohesion 1.237). Both knobs are `Option`-shaped and turn the feature off cleanly
  (`with_temperament(None)`, `with_needs_decay(None)`); neither adds a Cargo feature, a dependency
  or an allocation, and the biased frame costs nothing measurable (1.8772 ms against 1.8766 ms for
  10,000 agents). Spawned personalities changed for a given seed — the archetype is now a preset an
  individual deviates from by a third — and `NpcBundle` gained a `needs` field, so a save made
  after this change carries different temperaments than one made before it (ADR 0051).
- **The terminal capture is regenerated, not screenshotted.** `docs/demo/ashfall-play.png` is now
  produced by `demo/examples/frame_capture.rs`, which renders through the client's own `Session` and
  `Budget` at 120 x 44 cells and blows each framebuffer pixel up four times with the new
  `Image::scaled` — nearest-neighbour, so a block is the pixel it came from. The capture is the
  procession view with the impostor cross-fade active (`--band 8`, against `--band 0` for the hard
  switch it replaced; the two differ by 1,152 pixels, which is how the band was checked), and both
  captions now name the command that makes it, so the image cannot drift away from the code.

- **A GPU front end, and a playbook for building the next one.** `demo-gpu/` renders the Ashfall
  district through an instanced `wgpu` pipeline over the same scene, meshes and `Shot` table the
  software rasteriser uses, with `Backend::{Auto, Vulkan, Gl, Noop}` so it validates and encodes on a
  machine with no driver. `benches/frame.rs` separates a frame's latency (`frame`), its throughput
  (`eight_frames_per_drain`) and the cost of shipping pixels back (`frame_with_readback`), and reports
  the adapter it measured: on this sandbox that is `llvmpipe`, a CPU Vulkan driver, at 974.56 ms for
  600x375 with 5,401,392 triangles. `docs/dev/tuning/playbooks/gpu-demos.md` is the agent-facing instruction set
  — environment classification, adapter policy, pipeline and shader rules, correctness gates, the
  benchmarking trap that made a drain-free loop look like encode time, and the documentation duties.
- **Documentation audit.** ADR 0022 had been on disk and missing from the index since it landed; the
  index has it now. ADR 0050 gains an amendment correcting two premises that have since changed (this
  environment has an X server and a *software* Vulkan driver, not the absence of `libvulkan`) and
  recording the GPU measurements. The release checklist's "23 ADRs" count, which had drifted by
  twenty-eight, is now a rule instead of a number.

## [Unreleased]
### Added
- `SpatialGrid::with_slack_floor` (default `DEFAULT_SLACK_FLOOR`, 2): every cell now keeps spare slots regardless of
  occupancy, so a pair arriving in an empty cell is patched instead of forcing a rebuild. At 512 crossers a tick the
  patched share rises from 61% to 99.5% and the p99 falls from 2.83 ms to 0.58 ms at 100,000 bodies (16.08 ms to
  2.49 ms at a million) for 2.7x and 1.7x the table. `with_slack_floor(0)` restores the percentage-only rule.


- `SpatialGrid::with_slack_percent` (default `DEFAULT_SLACK_PERCENT`, 25) and `SpatialGrid::capacity`: the slack
  each cell's run is laid out with is now a knob rather than a constant. Measured with `examples/patch_tail.rs`:
  at a million bodies the patch's p95 falls from 13.9 ms to 2.4 ms for 17.8% more table, while at 100,000 it
  changes nothing and costs 1%, because the cells that refuse an arrival there are empty ones. See ADR 0064.

### Added

- Incremental rebuild step 2: the spatial grid now keeps a slotted table (capacity prefix, occupancy and
  cursor) and patches the bodies that changed cell in place instead of rebuilding. `RebuildOutcome::Patched`
  reports it. 11.3x faster than a rebuild at 1M bodies with 64 crossers per tick (1.190 ms against 13.427 ms).
  See ADR 0064.


### Added

- `SpatialGrid::build_incremental`: recomputes cell numbers against the table the last rebuild left, and ends the tick
  there when no body changed cell. 0.197 ms against 1.790 ms at 100,000 bodies, and 1.104 ms against 13.532 ms at a
  million, on the six-core sandbox. A tick with real movement rebuilds as before, at about four percent more than a plain
  rebuild, until the patch path lands.
- `SpatialGrid::check_invariants`, `RebuildOutcome` and `GridInvariant`: the four promises a completed rebuild makes, named
  so a failure says which one broke, plus the outcome of an incremental tick.
- `SpatialGrid::entries_mut`, so the code that moves bodies between rebuilds has a supported way to do it.
- `examples/incremental_build`: the harness for the still, crosser and full shapes, in the same JSONL schema as the other
  benchmarks.

- **The level-of-detail switch cross-fades instead of popping.** `Shot::impostor_band` widens the
  switch into a window: a body between `impostor_distance - impostor_band` and `impostor_distance` is
  drawn as both, the billboard claiming an increasing share of the pixels while the mesh gives its own
  up. The fade is a per-pixel ordered dither (`Renderer::triangle_coverage`), not a blend, because a
  blended fragment cannot write depth and a body that cannot write depth stops occluding itself — the
  blending implementation drew blotches where a hidden facet overpainted a visible one. The shipped
  views keep the band at zero, so the committed PNGs are unchanged. The interactive frame measures
  19.39 ms with the band against 19.63 ms without, which is this machine's noise floor. Where the
  billboard pass can still go faster — flat-shaded cards, fused card passes, cards aligned to the
  mesh's own bounds — is written up in `docs/demo/billboard-optimisations.md`.
- **The playground is explorable, in a terminal.** `demo/src/bin/ashfall-play.rs` flies the same
  scene the PNGs show — `wasd`, arrows, `1`-`5` to jump views, `p` for the scripted flight — with no
  window, GPU or widget toolkit: the software rasteriser draws into a fixed-size image and the
  terminal paints it two pixels to a cell with `▀`. `session.rs` is a pure, tested core (camera, view,
  lens, draw budget); `input.rs` decodes keys with an escape timeout. To hold 28 fps the interactive
  path draws under a `Budget` (420 bodies, 240 m, sub-pixel triangle cull) and past 24 m a body is a
  two-triangle billboard instead of an 11,000-triangle mesh — 850 ms a frame becomes 19.0 ms at
  112 by 72 pixels, and 38 ms in a real terminal, which is 26 fps. `--play N` runs the same flight headlessly and writes a PNG per frame plus
  `playback.json`, so CI can exercise it and `ffmpeg` can turn it into a video.
- **The rasteriser's CPU cost is measured against a wgpu pipeline.** `demo/benches/render_pipeline.rs`
  draws the same 2,000-body district scene through `raster.rs` and through a real `wgpu` device on its
  `noop` backend — pipeline, buffers, instance upload, render pass, submission, commands discarded.
  The CPU half of a wgpu frame is 33.6 µs for 2,064 instances and does not care how big the target is;
  the software path is 850 ms at full detail and 19.0 ms under the interactive budget. Figures,
  caveats and what they do *not* say are in `docs/dev/tuning/performance.md` and ADR 0050.
- **A 3D playground with an animated crowd.** `demo/src/bin/ashfall-3d.rs` renders five views of the
  same city with no GPU, window or renderer dependency: a software rasteriser (`raster.rs`, with
  near-plane clipping, a depth buffer, footprint-aware procedural shading, supersampling and a
  deflate/PNG writer) over glTF geometry (`gltf.rs`). Skeletons are handled too — `skinned.rs` reads
  a rig, samples a clip, composes joint matrices and skins the mesh per instance — so `procession`
  shows eighteen walkers on one CC0 KayKit mannequin at staggered phases rather than a row of
  T-posed statues. The Meshy bodies stay static on purpose: generated meshes carry no skeleton, and
  the demo is honest about which asset carries what.
- **A playground that ships assets and evidence.** `demo/` is a separate package: a six-district
  city of 2096 ruined blocks, a crowd placed by district keyed on `NpcId`, and four artefacts per
  run (SVG plan, engine-agnostic scene, run report, asset manifest). Six models were generated with
  Meshy — three bodies, three props — and `demo/tools/shrink_glb.py` re-encoded their embedded 2K
  textures and re-laid the binary chunk, taking 22.9 MB to 2.4 MB with geometry byte-identical.
  `demo/tests/assets.rs` holds every shipped file to a size budget and its container to glTF 2;
  `demo/tests/playground.rs` proves the crowd repeats, differs by seed, lands in its settlements and
  actually walks. Three findings are recorded in `demo/README.md`, including that `App::new()` has no
  `TimePlugin` and therefore a headless crowd integrates at zero delta, and the band plugin is
  tracked with a `band-focus-plane` task because it measures the player's distance on `x`/`y` while
  the 3D crowd uses `x`/`z`.
- **One lint spine, and no exceptions.** `[lints]` in `Cargo.toml` carries the whole policy:
  `pedantic`, `nursery`, `cargo`, `perf` and `all` as warnings that CI promotes with
  `-D warnings`, and `unwrap_used`, `expect_used`, `panic`, `indexing_slicing`,
  `cast_ptr_alignment` and `suspicious` denied outright — in `tests`, `benches` and `examples` as
  well as `src`. 474 `.expect(…)` and 34 `.unwrap_err()` in the test tree moved to
  `bevy_flock::testkit` (`OrDie`, `OrDieErr`, `GetOrDie`, `GetOrDieMut`), which reports a broken
  fixture as `unreachable!` with the caller's message and location. No `#[allow]` or `#[expect]`
  remains in tree except four `unsafe_code` expectations on counting allocators, the steering
  kernels included. `application::packed` reads through
  `get` and unpacks its lanes with a literal-index macro; the wide kernel of `application::crowd`
  moved to one 28-byte row per agent and slices each bucket's range once, so the neighbour loop has
  no bounds check in it at all. That last part was not cosmetic: a per-neighbour `get` measured 19%
  slower on a dense crowd, and the row-plus-slice form is 2% faster than the array layout it
  replaced (`docs/dev/tuning/performance.md`, ADR 0049). `scripts/lint_audit.py` keeps it that way: it fails
  on any override outside its (empty) sanctioned table, on a sanctioned entry that has disappeared,
  and on a `Cargo.toml` whose `[lints]` no longer denies what the policy says it denies. Both
  `docs/dev/lint/lint-policy.md` and the script's `--self-test` state and check what is enforced, and why. `raw_borrows_via_references`
  and the two clippy 1.99 lints are enabled by a probe-gated CI job rather than the manifest, because
  the pinned 1.97 toolchain cannot name them and an unknown lint name warns on every downstream build.
- **Reciprocal local avoidance (ADR 0045).** A second pass over the neighbour set the kernel
  already gathers removes the excess closing speed between each pair, split by priority, so two
  agents walking at each other stop at the separation floor instead of sharing space.
  `NpcAvoidancePriority` (default 1.0) is how a game says who yields; `BoidsConfig::with_avoidance`
  turns it off. On by default. The pass runs on the same `SteeringRoad` as steering, is held to bit
  equality between the scalar and threaded roads, and allocates nothing in a steady frame. Measured
  at 10,000 agents: 2.25 ms steering, 3.27 ms with avoidance; at 100,000: 25.74 ms and 38.08 ms.
- **Obstacle-driven navigation (ADR 0046).** `ObstacleNavPlugin` plus a `NavObstacle` component
  derive a `GridNavSource` from the world's blockers, rebuilding at most `cells_per_tick` cells a
  frame, a whole row at a time, and publishing atomically: the version increments once, on the step
  that finishes the rebuild, so no NPC ever paths against a half-built wall.
- **Stimulus perception (ADR 0047).** `StimulusBuffer` is a bounded, decaying ring of sights,
  sounds and smells; `perceive` applies range, cone, age and an optional `Occluder`. `StimulusPlugin`
  publishes from `StimulusSource` entities and writes `PerceivedStimuli` per observer on a tick
  interval, so a guard can lose sight of a target and search without a special case in its tree.
- **Navigation links and an optional navmesh (ADR 0048).** `NavLink` with a cost and a `LinkTag`
  participates in flow-field relaxation rather than being post-processed, so a ladder can be the
  only route to a floor, and `FlowField::link_tag_at` tells a leaf what it is about to walk onto.
  The `navmesh` feature adds a polygon source with area costs and a funnel, with the funnel's
  pinch-restart branches fixed to advance the apex to the opposite corner.
- **Perception keeps six slots, not thousands.** `perceive` collected every hit and sorted them
  before truncating to `max_results`; it now inserts in order into a bounded result set, which
  costs one comparison per slot and no allocation. 75.7 ms → 35.8 ms on the dense benchmark, and
  `the_bounded_insert_keeps_exactly_the_best_slots` is the parity test against the old behaviour.
- **The feature extremes build again.** `cargo check --no-default-features --all-targets` failed on
  main before this: `tests/memory_bridge.rs` imports `domain::memory`, and `Cargo.toml` never
  declared `required-features = ["memory"]` for it, so the test compiled only in configurations that
  happened to have the feature on. `fuzz_decoders` now declares it too rather than relying on
  `#![cfg(feature = "memory")]` to reduce it to an empty binary that reports success.
  `src/application/crowd.rs` had the matching lint bug: its `#[expect(clippy::indexing_slicing)]`
  was unfulfilled without `simd` (the kernel that indexes), which `-D warnings` turns into an error,
  and CI only linted `--all-features`. CI now lints and compiles both extremes with `--all-targets`.
- **Perception stops scanning the whole world (ADR 0047).** `StimulusBuffer` keeps a conservative
  bounding box, so an observer with nothing nearby returns in O(1); `StimulusIndex` buckets the live
  records into a uniform grid rebuilt once per pass (69 µs for 4,096 records) and `perceive_indexed`
  reads only the observer's neighbourhood. Identical output to the linear scan — candidate superset,
  one cell per record, total-order selection — held by two randomised parity tests, and the dense
  case falls back to the linear scan so it is never slower. 4,096 records over 400 m: 9.6 ms → 4.0 ms.
  The ECS pass uses the index by default.
- **Three fixes found by the above.** `steer_flock` wrote the kernel's desired velocity as
  `(x, 0, y)`, dropping vertical and lateral motion; it now writes all three axes. Sorting a
  `LinkTable` placed same-key edges after every equal key, leaving insertion order to decide ties.
  `Crowd::begin` and `Crowd::order_by_cell` now share a count reset, so the order is idempotent and
  the avoidance pass can reuse it.
- **Steering uses every core (ADR 0044).** `Crowd::steer` cuts a frame into contiguous slices of
  agents and hands them to the `ComputeTaskPool`, with `SteeringRoad { Auto, Wide, Threads, Scalar }`
  to pin the choice and `BoidsConfig::with_road` to set it. All four roads assert bit equality with
  the scalar reference, so determinism and the whole-world digest are untouched. Measured on six
  vCPUs: 8.48 ms → 4.23 ms at 10,000 agents and 84.0 ms → 29.6 ms at 100,000, against the wide road
  this supersedes. New `threads` feature (on by default) enables `bevy_tasks/multi_threaded`; without
  it the pool has one thread and `Auto` stays on the wide road. `flock_frame_road` joins the
  benchmark suite so the four roads are always measured on the same host in one run.
- **The book, and a README you can check.** `docs/book/src` is an eight-chapter guide
  (getting started, the model, crowds at scale, behaviour, memory, extending, debugging,
  costs) that reads as Markdown on a forge and builds with `mdbook`. The README gains a
  ten-minute verification section: every claim it makes is mapped to the test, benchmark or
  script that pins it.
- **Platform and feature audit (`docs/dev/tuning/platforms.md`).** Nine host configurations and four
  `wasm32-unknown-unknown` configurations are now compiled and recorded, and the audit found
  a real bug: with `--no-default-features`, the memory leaves and the diagnostics reached
  into `memory`-gated modules, so the server build did not compile. The leaves now restate
  the encounter flag values with a test that pins them to `EncounterKind::flag`. New CI jobs:
  `wasm` (four configurations) and `portability` (Windows and macOS `--all-features`).
- **A release process (`RELEASING.md`)** with a tag-triggered `release` CI job that packages
  the crate, checks the API against the last published release and re-runs the decision
  scripts. `cargo package --locked` is warning-free; benches and examples now ship with the
  package so every `[[bench]]` target stops warning on publish.

- **The engine, not just the crowd.** The remaining open plan tasks land together
  (ADRs 0035-0040). Behaviour trees gain the full node set — six composites, four
  decorators, flat compiled nodes, a fixed-size cursor stack, stateless shared leaves —
  and `tests/tree_no_alloc.rs` keeps a tick allocation-free. Beside them,
  `behavior::utility` picks goals by weighted considerations with inhibition, count
  compensation and an `explain` that prints the arithmetic. `domain::generator`
  produces reproducible NPCs (names, temperament, needs, background) from a seed and an
  index. `domain::groups` adds formations with stable slots and faction relations;
  `application::director` decides by returning commands and keeps a bounded trace, which
  is what makes a crowd's mood replayable. `behavior::dialog` validates graphs at load
  (dangling choice, unreachable node, loop without exit), `behavior::bark` funnels every
  line through one bounded, cooldown-gated channel with a per-frame cap, and
  `behavior::provider` keeps a slow line source off the tick. `domain::nav`/`domain::flow`
  give destination-driven movement a port, a grid and a hand-authored source, with a
  version-keyed field cache.
- **Integrations are ports (ADR 0040).** `PhysicsPort`, the `NpcNetState` wire format with
  a bounded `InterestSet`, and a sorted `RenderPlan` are engine-free: a game writes a
  dozen lines for its own Avian, replicon or renderer instead of inheriting a version
  conflict. `NpcNetState::BYTES` is pinned by a test, and `benches/replication.rs`
  measures the filter at 1k/10k/100k plus encode and decode.
- **Proof for the new surface.** Six property tests over arbitrary inputs (blackboard
  capacity and write survival, RNG determinism, grid/brute-force parity, wire round trip,
  weight clamping, tick monotonicity), four runnable examples (`hello_flock`,
  `custom_behavior`, `tavern_dialog`, `ambient_metropolis`) and benches for the scheduler
  (`cognition`), groups, flow fields and replication. CI compiles every bench and runs the
  examples. 443 library tests, the whole integration suite and clippy at `-D warnings`
  across all targets and features.
- **Git hooks are tracked, and the commit-msg hook refuses attribution trailers.** `scripts/pre-push`
  runs the two CI checks that are cheap enough to run on every push — `cargo fmt --all -- --check` and
  `cargo audit` over every committed lockfile, both the workspace's and the compat crate's, with the
  waivers read out of `deny.toml` so there is one list rather than two. The scan denies `unsound`,
  `unmaintained` and `yanked` and not just plain vulnerabilities: cargo audit files those three under
  `warnings` and exits zero for them by default, so a reader of `vulnerabilities` alone calls a
  lockfile carrying a use-after-free advisory clean. A scan that could not run is a block, not a pass.
  `scripts/commit-msg` refuses a message carrying a `Co-authored-by:` trailer — the history belongs to
  the person who made the changes, and the body is where the explanation belongs.
  `scripts/install-hooks.sh` installs both and leaves the hooks it does not own alone.

- **Scaling regression suite and a generated report (ADR 0034).** `scripts/scaling_regression.py` gates
  CI on speedup ratios rather than wall clock, so a four-core runner is not a regression: thresholds
  widen with the measured spread (`fail = max(25%, 2.0 x widest)`, `warn = max(12%, 1.5 x widest)`), a
  different fixture is refused instead of compared, and no shared core count is a failure rather than a
  pass. The tolerances are exported as constants, printed by the report and stated in ADR 0034 with what
  they mean in seconds; the job carries a 15-minute timeout so the CI budget is enforced too.
  `scripts/scaling_report.py --check` fails when the committed report or ADR 0034 stops matching the
  code and the baseline, which is how the "differences below 18%" contradiction got caught.
- The gate refuses to be green over an empty test: a cell whose spread pushes the failure line past 25%
  is `unverified`, the sequential cell's spread propagates to every row that divides by it, and a run
  where fewer than half the cells could be verified exits 2. Found by the first full verification, where
  one 17.7 s outlier on the one-core denominator turned five rows into fake 9-12% gains.
  `scripts/scaling_report.py` writes `docs/dev/tuning/reports/scaling-report.md` — wall clock, speedup and
  efficiency matrices, the measured optimum against the cap formula, an ASCII chart and the verdict —
  from the same baseline the gate uses. The sweep moves to millisecond timing, two repeats per cell and
  a `meta` record, and corrects an earlier defect: shape B built six-crate bundles from an eight-crate
  fixture, so four of its eighteen builds never happened. The CI job runs a reduced matrix (shape A,
  one core and the runner's own, concurrency 1/2/4) and uploads the raw sweep and verdict as artifacts.

- **Group scaling measured at 1, 2, 3 and 6 cores, both task shapes.** `scripts/scaling_sweep.sh`
  pins every run with `taskset` and gives each task a cold target directory; `scripts/scaling_curves.py`
  plots wall clock and speedup with the raw rows checked in at `docs/dev/tuning/data/scaling-sweep.json`. Findings:
  the cap formula is a floor with headroom (six one-thread tasks take 8 s, all eight take 5 s, because
  cargo's I/O start overlaps), threads per task dominates the curve, and a six-thread task is already
  queue-limited on three cores. ADR 0033 records all three, including the case where the formula
  under-admits.

- **ADR 0033 states the group capping policy once.** `cap = nproc / threads_per_task`, measured per
  task rather than assumed, with group sizes taken from `wiggum validate` because wiggum injects audit
  tasks the phases do not declare (23 authored, 24 scheduled, measured). `scripts/group_cap.py` gains
  `--groups-from` for the exact answer, `--check-plan` for the authored one, and a self-test that CI
  runs on every push: five recommendation cases, eleven evidence rows, wiggum's wave rule, cycle
  detection, and a plan that still parses. Capacity stays out of CI on purpose — runners have two to
  four cores, so a strict gate would be switched off within a week.

- **ADR 0032, amendment 2: the group cap is a thread budget.** Eight one-core codegen tasks on six
  cores: sequential 24 s, capped at half the cores 15 s, capped at six 10 s, uncapped 8 s. The
  earlier "cap at half the cores" advice was measured on workspace builds that each spawn `cargo -j6`
  and is wrong for any other shape. `cap = nproc / threads_per_task`, measured with
  `scripts/group_cap.py --measure "<command>"`; the old rule is withdrawn rather than quietly dropped.

- **ADR 0032 amended: the build lock is nearly free and `CARGO_TARGET_DIR` isolation does not pay.**
  Three cold compile tasks on six cores: one target sequential 45 s, one target concurrent 43 s
  (+4%), three targets concurrent 46 s, three targets sequential 82 s. The original advice to isolate
  a cold-building group was wrong — three cold dependency trees oversubscribe six cores and give back
  what the overlap won. The shared target directory stays the default; isolation is now documented as
  a debugging tool, and compile-heavy groups are capped at about half the cores.

- **ADR 0031's re-open trigger is now a CI gate.** `scripts/cell_size_gate.py` reads the
  `flock_frame_cell` estimates and compares 9 m against 13.5 m: below a 3% saving the decision
  stands, 3-10% annotates the run, above 10% the build fails and the ADR is wrong. The thresholds
  are asymmetric because a runner cannot resolve 3% and a flapping gate gets disabled. The
  `cell-size-gate` CI job runs the benchmark and the script on every push, and the plan carries the
  dependency as `cell-size-reopen-gate`, which watches the `benchmark-suite` regression-gate work.

- **The cell budget is configurable, and the ECS sweep says leave the default alone (ADR 0031).**
  `SpatialGridConfig::max_cells` and `with_cell_budget` let a game raise the grid past 2^20 cells;
  `BoidsConfig::with_grid` exposes it through the plugin. Measured whole-frame time at 10,000 and
  100,000 agents: 1.4M cells (9 m) is free, 2.9M (7 m) costs ~1% at 100,000, and neither moves the
  frame the way it moves the kernel — tiering caps the steered population at a few thousand, so the
  8-20% kernel preference for 9 m never reaches the frame. `MAX_CELLS` stays at 2^20; the knob is
  there for games that steer more of their crowd.

- **The transposed crowd kernel is on by default, and cell sizing is documented (ADR 0031).** A
  sweep of five cell sizes at 10,000 and 100,000 agents found the road winning at every density —
  including the 3-agents-per-bucket case ADR 0030 was cautious about — by 27-37% at 10k and 19-31%
  at 100k, so `simd` joined the default features (`default-features = false` restores a
  dependency-free build). The kernel's preferred density is 6-10 agents per occupied cell: four per
  bucket is the minimum for a block, and 48 per bucket is the slowest configuration on both roads.
  The default grid lands on 13.5 m, which is 19% better than scalar and 8-20% short of the 9 m
  optimum; the recommendation is documented rather than enforced, because raising the cell budget
  would also raise the cost of the ECS query path that ADR 0003 sized it for. `examples/grid_sweep.rs`
  re-runs the measurement; end to end at 100,000 agents the frame is 79.07 ms, down from 90.0 ms.

- **The crowd kernel can now use SIMD — the transposed shape (ADR 0030).** ADR 0029 rejected
  lane-per-neighbour vectorisation for good reasons: the lanes shared nothing and the masked
  arithmetic threw away every rejected neighbour. The shape that works is the band kernel's
  transpose — block four agents that share a cell and broadcast each neighbour into all four lanes.
  Behind `--features simd`, with a scalar road for buckets that cannot fill a block and for
  ambient-tier agents, it is 22% faster planar, 21% volumetric and 29% faster on a dense crowd
  (10,000 agents, kernel only), and `the_transposed_road_agrees_with_the_scalar_road` holds it to
  identical counters and desired velocities against `Crowd::steer_scalar`. Off by default: it costs
  a per-frame structure-of-arrays copy that a sparse crowd does not earn back.

- **SIMD was measured against the 3D crowd kernel and rejected (ADR 0029).** A structure-of-arrays,
  four-neighbours-at-a-time scan with a scalar tail was built behind the `simd` feature, held to
  parity with the scalar road in the same process, and benchmarked on planar, volumetric and dense
  crowds: it lost by 16%, 26% and 8%, and AVX2 did not change the verdict. The scalar scan rejects
  out-of-range neighbours before doing any work, and that is precisely what masked SIMD throws away.
  The code is removed rather than flagged off; the `crowd_kernel` benchmark group and ADR 0029 keep
  the measurement reproducible. The `simd` feature still vectorises the band kernel (ADR 0023),
  where it wins.

- **Crowds are three-dimensional (ADR 0028).** The steering kernel and the spawn placement no
  longer assume the plane `y = 0`: positions, velocities and desired velocities are `[f32; 3]`,
  cells come from all three axes with the grid's own arithmetic, and the scan walks a box of cells
  and tests true 3D distance. New placements `uniform_volume` (a box) and `uniform_sphere`
  (volume-uniform, `r = R·∛u`) join `fixed`, which now honours the height it was given. Nothing else
  had to change — the grid and the integration system were already volumetric. Cost: 19% on a
  planar crowd (`flock_frame/10000` 7.71 ms → 9.18 ms, still 1.8× inside the frame budget), because
  the price is per-neighbour arithmetic rather than empty-cell bookkeeping. Breaking for direct
  `Crowd` users only: `push` and `desired` use `[f32; 3]`, and `configure` lost its `plane`
  argument.

### Asset pipeline skills, ported and made checkable

`.github/skills/` gains four playbooks carried over from the TrapGrid Arsenal project and rewritten for a
crowd library: Meshy NPC assets, the complete archetype package, Git-safe external placeholders, and
the shared humanoid action library. The vocabulary moved (operator -> NPC archetype, deck -> roster
card, selection line -> bark) and so did the boundary: this crate loads no media, so every skill
produces files for the game that consumes it and names `build_render_plan` as the seam.

Two things were added rather than translated. The Meshy skill opens with a variant budget, because a
crowd needs four bodies and sixteen tints rather than ten thousand meshes: `VisibleInstance` carries
`variant` and `tint`, and `LodTier::Ambient` is culled before it ever reaches the plan. And the
animation pipeline's clip list existed in three files with nothing comparing them, so
`check_action_contract.py` parses both scripts and the skill's own table with `ast` and fails on any
disagreement, including an index order that is not alphabetical — that order is what
`GltfAssetLabel::Animation(n)` means. The Blender scripts themselves cannot run in CI, which is
exactly why the thing that can run checks their contract instead.

### Lint drift is a catalogue, not a sentence

The eleven warnings a newer stable reports — six `assert_is_empty`, two `tuple_array_conversions` in
the packed kernel, `manual_slice_fill`, `needless_borrows_for_generic_args` and
`chunks_exact_to_as_chunks` — used to be one sentence in `plan.toml` with counts. A count is not an
inventory: it does not say which file, it cannot be checked, and it goes stale silently.
`docs/dev/lint/lint-drift.md` now lists every site with the rewrite stable wants, and
`scripts/lint_drift.py` derives that table from a live `cargo +stable clippy` run and fails CI when
the two disagree in either direction: an unlisted warning, a row that is gone, or a table derived
from a different clippy than the one running. A clippy that does not compile exits 2 rather than
reporting "nothing to report", which is the failure mode the old probe had.

### Lint policy is now machine-checked in both directions

`scripts/lint_audit.py` grew the two checks it was missing. It now sees `#[allow]`/`#[expect]` of
**any** lint, not only `clippy::` ones — which immediately turned up four `unsafe_code` attributes
and two `dead_code` ones the policy page had been describing as "none". The four are counting
allocators in tests and an example (`GlobalAlloc` is unsafe by definition, and nothing else can
observe an allocation from outside the crate); they are now `#![expect(…, reason = "…")]` so they
fail when the allocator goes away, and they are named in the script. The two `dead_code` ones guarded
fixture constructors nothing called, so the fixtures were deleted.

It also reads `Cargo.toml` and fails unless every `deny` is still denied *and* the set of `allow`
entries equals its `TOLERATED` table, so a lint cannot be quietly downgraded and a new allow cannot
arrive undocumented.

The `lint-future` job was checking nothing: `rust-toolchain.toml` makes rustup run 1.97.1 even in a
job that installs stable, so every probe said "unknown" and the job exited green. It now probes and
builds with `+stable` on a throwaway file, fails if the probe itself errors, and denies the three
post-1.97 lints rather than every warning a newer stable has. Running it found 51 real
`rest_pattern_accessible_field` sites (`Err(Variant { .. })` → `Err(Variant { field: _, … })`), all
fixed; the remaining 11 newer-lint warnings are `stable-lint-drift` in `plan.toml`.

### Added

- **Spawn placement is a validated type (ADR 0027).** `Placement` offers four answers —
  `spiral` (the default, 1.6 m, draw-free and compact at any population), `uniform_disc`,
  `uniform_rect` and `fixed` (authored positions, cycled when the population is larger than the
  list) — and `FlockConfig::with_placement` validates the choice against the simulated volume
  before it reaches the spawner. That check is the point: a crowd placed outside the grid does not
  fail loudly, it piles into the border cells and the frame gets slower. Random placements draw
  from each NPC's own stream, after temperament and archetype, so a replay scatters the same people
  into the same spots and switching to an authored list changes nobody's identity.
- `FlockConfig` is no longer `Copy` (a placement may own a list of points) and `FlockPlugin` is
  `Clone` instead; `spawn_position(index)` remains as the default placement's answer.

### Changed

- **Crowd steering is cell-major (ADR 0026).** `steer_flock` now gathers the crowd into arrays in
  spatial-grid order, steers it with one pass per agent over the cells in range, and scatters the
  results. The model is unchanged — same weights, same priority truncation, same arrival curve —
  and a parity test holds it within 1e-3 of `desired_velocity`. What went away is the per-agent
  `Vec<Neighbour>`, the distance sort inside `SpatialGrid::radius`, and the second and third
  passes over the neighbour buffer: `flock_frame/10000` fell from **18.2 ms to 7.71 ms** (130 fps,
  2.2× inside the 16.6 ms budget), 100,000 agents from 192 ms to 73.5 ms, and a steady frame
  allocates nothing (`tests/crowd_no_alloc.rs`).
- **The crowd no longer averages an agent into its own cohesion.** The grid returns an agent as
  its own nearest neighbour, and the old loop divided the alignment and cohesion sums by a count
  that included it, diluting both by `1/n`. The kernel excludes the agent, which is what the
  equations meant; `the_kernel_does_not_average_an_agent_into_its_own_cohesion` pins the old bias
  so the difference is documented rather than discovered in a changelog.
- New: `FlockSteerReport` publishes what the last steering pass cost (`steered`, `isolated`,
  `pairs`, `cells`), and `steer_flock_reference` keeps the old per-agent path available for games
  that want to inspect neighbour lists.

### Reproducibility

- `Cargo.lock` and `compat/Cargo.lock` are committed, and every CI job runs `--locked`, with a
  `lockfile` job that fails if either is stale. A dependency bump is now a commit rather than a
  silent resolution, which matters because `docs/dev/tuning/performance.md` publishes timings that only
  mean something against an exact dependency graph.
- `docs/dev/tuning/performance.md` grew a reproduction section: the toolchain and profile that are pinned,
  the lockfiles, the exact `cargo bench` command for each row, the host the numbers were taken
  on (6 vCPU Neoverse-N1, single-threaded paths), and what still varies between runs (1-4% run
  to run; a different machine is a different measurement, which is why the document argues in
  ratios). It also records a cross-check: `flock_frame/10000` re-measured at 18.75 ms against
  the committed 18.2 ms.
- Stated plainly rather than left to be inferred: the 10,000-agent crowd frame is 18.2 ms, i.e.
  55 fps against a 16.6 ms budget. The 60 Hz route at that crowd size is the band path.

### Fixed

- **The band no longer manifolds what it folds.** The manifest window was square, so its
  corners reached 1.41 radius while folding began at 1.15 radius: every agent manifested from
  there was born outside the band and folded on the next tick, and the band held one ration of
  agents (256) forever instead of its 5,000 target. The window is now a disc bounded by the
  manifest *and* fold radii, and the spawn point is the one that was vetted. ADR 0025.
- `seed_field` now distributes the population uniformly over the field instead of filling it
  from the origin and stopping when the count ran out — which had left the map's centre empty
  and made the manifest behave like a vacuum cleaner.

### Documentation

- `docs/dev/tuning/memory.md`: heap and cache profile of the packed kernel, scalar and wide.
- `docs/dev/adr/0024`: the layout as a cost contract, the ordering constraints, and the parity rule.
- `docs/dev/meta/release-checklist.md`: gate ledger, performance and memory sign-off for the
  1,000,000-inhabitant milestone, with the open manifest/fold churn listed as a 1.0 blocker.
- `examples/memory_probe.rs`: allocation and footprint probe, cachegrind-friendly.

### Added

- `simd` feature: the steering kernel processes four agents at a time with `wide::f32x4`,
  behind a cell-major block order. 1,000,000-inhabitant frame 4.75 -> 3.78 ms; packed steer
  at 50,000 agents 138 -> 56.5 ms. Bit-identical to the scalar path, which is kept as the
  reference and tested against it. See ADR 0023.

### Added

- A complete behaviour-tree node set: `Sequence`, `Selector`, `Parallel` with
  `RequireAll`/`RequireAny`, `Inverter`, `RepeatUntilFail`, `Cooldown`, `Timeout`,
  `Condition` and `Action`. Compiled to a flat node array; a tick allocates
  nothing, which `tests/tree_no_alloc.rs` measures rather than assumes.
- Abort semantics that reach running leaves deepest-first, with subtree ranges in
  the compiled layout so a timeout can cancel the work underneath it.
- `NodeSpec::from_ron` behind the `serde` feature, so a designer can author a tree
  without recompiling; a golden test asserts it evaluates identically to the
  builder equivalent.
- `VirtualCost`, the declared-cost newtype the think scheduler will spend
  (ADR 0009), plus `LeafBehavior::cost_hint` and `FlockRegistry::tree_cost`.
- `StandardLeaves`: sixteen built-in leaves (actions and predicates) that are
  stateless by construction, so one registered instance serves the whole crowd,
  each with a cost calibrated against `benches/brain.rs`.
- `Archetype::logic`: a behaviour tree per archetype with a pinned declared cost —
  guard 27 vns, merchant 46, urchin 36, noble 35, wanderer 35.
- `NpcBrain`: a component holding a shared tree, a per-NPC cursor and the cached
  cost, ready for the scheduler task to drive.

- Social memory behind the `memory` feature (ADR 0010): `MemoryStore`, a flat
  array of 16-byte slots indexed by eligibility and subject, with lazy integer
  decay, a LIFO free list, and exact `implant`/`release` round-tripping.
- `MemoryArchive`, a bounded cold tier that evicts the faintest record by lowest
  total familiarity then lowest `NpcId`, so eviction is a pure function of what is
  stored.
- `DecayPolicy` with half-life in ticks and a `NONE` variant for grudges that never
  fade; memory decays to exactly zero rather than to a permanent sliver.
- `benches/memory.rs`: remember 7.7 ns, recall 5.5 ns, decayed recall 8.5 ns, sweep
  6.9 ns per NPC over 16,384 eligible NPCs.

### Changed

- `LeafBehavior::tick` and `abort` take `&self`: a leaf is a shared function over
  the blackboard, not per-NPC state. This makes the "one instance, whole crowd"
  model explicit instead of a footgun.
- `VirtualCost::DEFAULT` is 40 vns — several times the priciest built-in leaf —
  rather than an arbitrary 1,000.

- `MemorySnapshot` (ADR 0012): the save format. Little-endian, magic `BFLK`, version
  1, records sorted by identity, times stored as ages so a save/load round trip
  does not age a grudge, CRC32 on header and body, and typed errors naming the
  reason — bad magic, unknown version, truncation, checksum, duplicate identity,
  implausible count, capacity, subject range.
- `SavedSlot`, `SaveRecord`, `Origin` and `LoadReport`; `rebase_offline` for a
  game that wants the world to have moved on while it was closed.
- A format test pins the size claim: 10,000 NPCs with two relationships each is
  430,032 bytes, not "roughly 430 KB".

- Format version 2 (ADR 0013): a sectioned container with a subject registry, so a
  content patch that adds, removes or reorders subjects translates memories instead
  of silently re-pointing them at the wrong person.
- `SubjectRegistry`, `SubjectEntry`, `SubjectKey`, `SubjectKind`: dense identifiers
  at runtime, stable keys in the file, labels for humans only.
- `LoadReport` replaces `RestoreReport`, and reports `dropped_slots`, `translated`
  and `unverified_mapping` so a lossy load says what it lost.
- `LegacySubjectPolicy::{Refuse, AssumeDense}`: a version 1 file has no keys, so it
  is refused by default and loaded only when the game says so out loud.
- Unknown sections are skipped, so additive format changes no longer need a version.

- `benches/saves.rs`: version 1 against version 2 at 50,000 NPCs. The container costs
  0.4 ms of encode and 0.4 ms of decode over version 1 for 140 extra bytes; the
  original whole-file trailer checksum cost 6.7 ms of a 14.9 ms encode and now covers
  the header and table only.
- `registry_telemetry` example and `domain::telemetry`: ghost rows, orphan subjects
  and orphan slots as permille integers, plus a dependency-free SVG of accumulation
  over twelve sessions and references per subject.
- `tests/save_compat.rs` with a committed fixture corpus and `BLESS_SAVES=1` blessing
  (ADR 0014): the frozen writer, a version 1 file that still loads, a future version
  refused by number, an unknown section skipped, every bit flipped and every prefix
  tried without a panic.
- ADR 0014 (compatibility testing) and ADR 0015 (memory diagnostics).

- `MAX_SUBJECTS` raised from 16 to 64 (ADR 0016): a player, a party, a faction and its
  officers fit in one store. Sizing stays per request, so a game asking for four pays
  for four.
- `MemoryStore::footprint()`: `rows × (subjects × 16 + 8)` bytes, reported rather than
  estimated, with tests pinning the model and the 4× cost of sixty-four subjects.
- `benches/memory.rs` `memory_stride`: the sweep costs the same on ghost rows at 16 and
  64 subjects, and 3.3× on populated rows (71 µs → 232 µs at 4,096 rows).
- The telemetry example's layout is computed from the subject count rather than
  hard-coded: sixty-four columns used to put the panel 600 pixels off the canvas.

- `compat/`: the forward direction of save compatibility (ADR 0014, layer 4). Its own
  workspace, pinned to the `v0.1.0-rc.1` tag rather than a path, run in CI over the
  committed corpus: loads what it understands, refuses what it does not by name, never
  panics. First release candidate tagged.
- The telemetry example's chart is derived from the subject count, and its render is
  committed at `docs/images/registry-telemetry.png` so a layout regression is visible.

- `application::schedule::ThinkScheduler`: the virtual-cost fair-queue think scheduler
  (ADR 0009). A min-heap of `(virtual finish time, id)`, one think per participant per
  tick, weights that buy proportional share, and a starvation bound measured rather
  than claimed. 11 tests; `benches/think.rs` covers 1,000 to 100,000 participants.
- Measured: a 16.6 ms virtual budget serves 88.7% of 10,000 NPCs per tick (worst wait
  3 ticks) and 12.1% of 100,000 (worst wait 45 ticks), with the planner itself costing
  1.34 ms and 1.83 ms respectively — the number that makes this a budget rather than a
  trick.

- `domain::weight`: the proximity weight policy the scheduler's numbers were always
  missing. Distance bands (close / nearby / distant / background) with hysteresis, so an
  NPC on a boundary keeps the weight it earned instead of flickering between 8 and 1
  forty times a second. Six tests.
- ADR 0017 proposes tiered cognition — reflex, reactive, deliberative, social — where the
  tier selects the *logic* and the weight selects the *cadence*, with a reserved budget
  share per tier. Measured: ~7,400 full deliberation thinks fit in a 16.6 ms frame;
  a 95/4/1 tier mix fits ~110,000 because the average think drops from 2.25 µs to 150 ns.

- The ECS half of the scheduler: `NpcBlackboard`, `NpcThinkWeight`, `PlayerFocus`,
  `ThinkSchedule` and three ordered systems. Weights are measured from distance to the
  nearest player with hysteresis; costs come from each brain; only the chosen trees tick.
  A game opts in by adding a brain, which keeps ten thousand brainless crowd agents the
  cheap default.
- Measured end to end in a real `App`: 10,000 brains add 0.73 ms to a 4.28 ms frame.
  Comfortable maximum is **7,000 deliberative thinkers per tick** at a 14 ms budget and a
  2 µs deliberation; 6,000 with headroom.

- Tiered cognition (ADR 0017): `CognitionTier`, `TierPolicy` with hysteresis and
  engagement, `BudgetShares` with reserved per-tier budgets, `TieredSchedule` with one
  queue per tier, and the ECS systems that assign tiers under a promotion ration.
- Measured: at 10,000 NPCs a 14 ms budget spends 2.17 ms of frame on reflex (every NPC
  thinks every tick), 0.62 ms on reactive, 0.54 ms on deliberative and 0.45 ms on social.
  The reflex ceiling is ~64,000 per tick, not the ~93,000 arithmetic suggested — the
  difference is the cost of reaching the components.

- ADR 0018 proposes the ambient continuum: population outside a manifest band becomes
  per-cell density advected by the flow field, manifested and folded deterministically,
  with a packed reflex batch for the band itself.
- Full-suite measurement at 64,000 and 100,000 (see `docs/dev/tuning/performance.md`): 64,000 reflex
  agents thinking every tick cost 11.44 ms of frame; 100,000 cost 14.01 ms. The crowd path
  with no brains at all costs **198 ms** at 100,000 — the real ceiling, and the one the
  continuum is aimed at.

- `domain::continuum`: ambient population as twelve-byte cells with exact integer
  conservation, deterministic manifestation and O(1) folding. Advection of a million
  inhabitants over 40,000 cells measures 0.315 ms per stride; manifesting 2,000 identities
  is 10.5 µs.
- `MemoryStore::sweep_keeping` / `age` (ADR 0019): reclamation is a store-level cadence
  with an anchor exemption, so NPCs nobody simulates cannot leak rows, and anchored
  memories fade in warmth without losing their record.
- Gate 8 of ADR 0018 **fails**: a 5,000-entity manifest band costs 9.47 ms because the
  crowd path costs ~1.9 ms per 1,000 steered NPCs. See `docs/dev/tuning/gates/continuum.md`.

- `application::packed`: steering over contiguous arrays with a bucket grid. 5,000 agents
  steer, integrate and advance in **1.50 ms**, against 9.47 ms through the ECS crowd path.
  Density matters more than count: the same 5,000 in a 128 m square costs 10.92 ms.
- ADR 0020 specifies the manifest band plugin: radius derived from a target density,
  ownership table, system ordering, seven requirements, and how each open gate closes.
- `tests/memory_aging.rs`: four integration tests over long-duration simulation, covering
  the absent-NPC leak, the anchored grudge that outlives an hour of silence, per-owner
  exemption, and bounded reclamation work.

- `BandPlugin` (ADR 0020) wires the packed kernel into the ECS: fold, manifest and steer in
  one ordered set. Gate 8 **passes**: 100,000 inhabitants with 5,000 manifested measure
  **3.37 ms**, and 250,000 measure the same — flat in total population.
- ADR 0021 proposes concentric bands (near 60 Hz, mid 20 Hz, far as density) to take the
  same flatness past 250,000.

- ADR 0021 implemented and measured: near band (5,000 @ 60 Hz) plus mid band (15,000 @
  20 Hz) on the packed kernel. **1,000,000 total inhabitants at 5.02 ms**, under the 6 ms
  gate, with the near band's count unchanged. `BandConfig::mid_extent` exists because the
  mid pack inherited the near band's 512 m grid and clamped every agent into its edges.

### Documentation

- ADR 0011: how social memory integrates with the virtual-cost stride scheduler —
  cost as declared leaves, weight as a clamped boost, eligibility following the
  scheduler, and the feedback loop those rules close.

### Planned

The budgeted think scheduler, the utility scorer, the continuum ambient tier,
dialogue graphs and barks, snapshots and replay, rollback, and the render, `avian`
and `replicon` adapters are specified in `plan.toml` and not implemented yet.

## [0.1.0] — 2026-11-04

The simulation foundation.

### Added

- `FlockPlugin` with `FlockConfig`, four `CrowdPreset`s, and budgeted spawning
  driven by `SpawnQueue` (unique and recycled identity modes, 1 to 10,000+ NPCs,
  spawn budget per frame).
- `FlockSet` scheduling stages (`Spawn`, `Perception`, `Cognition`, `Steering`,
  `Integration`, `AdvanceTick`), chained so a game can join any of them.
- Boids steering with separation, alignment, cohesion, seek and arrive combined
  by priority-weighted truncation rather than a weighted sum.
- `SpatialGrid`: a uniform grid over a fixed volume with radius and k-nearest
  queries, deterministic result ordering, and no steady-state allocation.
- `RngStream`: PCG32 with per-NPC stream derivation from `(seed, NpcId)`,
  verified against the reference implementation and pinned by a golden vector.
- `Temperament` (Big Five plus bravery, sociability, industriousness),
  `Archetype` presets, `Needs` with decay and satisfaction, and a two-axis
  `Mood`.
- `BehaviorTree` with resume semantics, abort, a depth cap, and a flat compiled
  layout; `StateMachine` with dwell times and ordered guards; `Blackboard` with
  fixed capacity and compile-checked keys.
- `FlockRegistry` with the `LeafBehavior`, `GroupDirector`, `WorldDirector` and
  `NpcLogic` extension traits, and duplicate-identifier rejection at
  registration.
- Criterion benchmarks for whole frames at 1, 1,000, 10,000 and 100,000 NPCs,
  plus the spatial query and steering inner loops.
- Determinism tests that hash full NPC state in `NpcId` order and assert that
  identical seeds produce identical worlds, and that a larger population keeps
  the shared prefix of individuals.

### Notes

- MSRV is 1.97.1, matching Bevy 0.20.
- `glam` is pinned to the 0.33 line; `cargo tree -d -i glam` should never show
  two versions.
