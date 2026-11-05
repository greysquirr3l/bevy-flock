# Remaining work state (working notes)

Repo: /home/ubuntu/bevy-flock. Branch main. Author: Nick Campbell <s0ma@protonmail.com>.
Toolchain on PATH: export PATH="$HOME/.cargo/bin:$PATH". Rust 1.97.1 pinned.

## Done in this session (committed or uncommitted)
- Committed: domain generator, nav (NavSource + GridNavSource), flow fields (+cache,
  resumable solver), groups/factions/formations, behavior utility scoring, dialog graph
  engine, barks, dialog provider seam, application director (+AlarmField, DirectorBoard).
  All 336 lib tests pass.
- Uncommitted so far: adapters/primary/perception.rs, adapters/primary/grouping.rs
  (neither wired into mod.rs yet, not compiled).

## Remaining plan tasks
1. encounter-perception  -> perception.rs written; wire module + systems into FlockPlugin
   (FlockSet::Perception), add PerceptionConfig resource, tests pass.
2. group-faction-model -> grouping.rs written; wire Groups/NpcIndex resources + systems.
3. world-director -> adapters/primary/world_director.rs: CrowdDirector resource wrapping
   application::DirectorBoard, systems build DirectorInputs from CrowdPulse + FlockStats +
   SimClock, apply Mood into AmbientMood resource, publish alarm count into CrowdPulse.
4. observability-and-debug -> adapters/primary/observability.rs: CrowdPulse resource,
   Diagnostics resource, sample_crowd (interval 60 ticks, O(n) allowed), report() text,
   Violation enum + check_world(&mut World) for duplicate NpcIds / tier sum, optional
   `tracing` feature spans.
5. render-adapter -> adapters/primary/render.rs: RenderPlan resource (reserved capacity,
   no per-frame alloc), VisibleInstance {npc, position, yaw, tint, variant}, cull by tier +
   distance, sort by variant, draw_call count; tests for culling parity + capacity stability.
   Feature `render` optional Bevy sprite/mesh adapter if time permits (else document gap).
6. nav-adapter -> adapters/secondary/nav_adapter.rs: NavGrid resource (GridNavSource +
   FlowCache), version refresh system, NavVersion bump test.
7. physics-adapter -> adapters/secondary/physics.rs: PhysicsPort trait (object safe),
   KinematicBody marker, order test with a recording stub; document avian gap.
8. networking-adapter -> adapters/secondary/network.rs: NpcNetState quantised,
   InterestSet radius filter, encode/decode round trip, bounded replication test;
   document replicon gap.
9. memory-in-cognition -> perception writes KEY_PLAYER_SEEN/DISTANCE; add
   behavior/social_leaves.rs (GreetKnown, AvoidKnown leaves w/ cost hints) gated or not on
   memory feature; memory bridge system under `feature = "memory"` calling MemoryStore.
10. dialog-dag / bark / provider already at library level; add ECS system in
    adapters/primary/dialogue.rs: DialogueSessions component + talk system + bark emit
    system writing BarkQueue; tests.
11. examples-showcase -> examples/: horde_100k.rs (headless), tavern_dialog.rs,
    custom_behavior.rs (custom LeafBehavior + GroupDirector), ambient_metropolis.rs,
    plus README sections.
12. benchmark-suite -> benches: generator.rs (100k blueprints + alloc count),
    utility.rs (20 candidates x 10k NPCs), flow.rs (solve + cache), dialog.rs (graph walk),
    director.rs; extend regression gate script to read criterion JSON vs committed baseline.
13. property-and-fuzz-suite -> tests/properties.rs with proptest (dev-dep) for grid parity,
    steering stability, tree cursor, dialog validator; fuzz/ targets (cargo-fuzz, no_std-free)
    + mutation digest test.
14. docs-site-and-readme -> book/ (mdbook: SUMMARY.md + ~10 chapters with tested snippets),
    docs/comparison.md, README refresh, .github/workflows/pages.yml.
15. prelude-rustdoc-book -> prelude re-exports for all new modules, lib.rs crate docs update,
    docs/diagrams/*.d2 + rendered PNG in docs/images.
16. release-and-community -> ROADMAP.md, .github/ISSUE_TEMPLATE/*, release workflow,
    scripts/release_check.py, CHANGELOG entry.
17. cross-platform-audit -> CI feature matrix, wasm32 check, docs/audits/platform-2026-11.md.
18. ADRs 0035-0040: 0035 group relationship storage; 0036 dialog engine + provider seam;
    0037 barks; 0038 world director + alarm field; 0039 render plan; 0040 ports
    (nav/physics/network) + test strategy. Plus index update.
19. plan.toml: mark every delivered task with delivered_in = "<commit sha>" once committed.
20. Run full verification: cargo fmt, clippy -D warnings (all targets/features),
    cargo test --release --all-features, then commit + push.

## Conventions
- Lints: deny unsafe_code, missing_docs, clippy::unwrap_used/expect_used/panic/indexing_slicing
  in library code (tests exempt). Every public item needs docs.
- Tests live in `#[cfg(test)] mod tests` inside modules; integration tests in tests/.
- Commits: conventional messages, author Nick Campbell.
