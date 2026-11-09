# ADR 0040, Optional integrations are ports, not dependencies

**Status:** Accepted
**Date:** 2026-11-05

## Context

Three integrations are obvious candidates for a crowd crate: physics (Avian, Rapier),
networking (replicon), and a renderer (Bevy's own). All three are version-coupled to
Bevy, all three release on their own schedule, and all three would double the compile
time of a library that mostly does arithmetic.

A hard dependency would also decide for the user. A game that already runs Rapier does
not want Avian in its tree because its crowd library chose one.

## Decision

Ship ports, not adapters:

* **Physics.** `PhysicsPort` receives one velocity per NPC and a `flush()` per frame.
  `PhysicsBridge` holds it as an `Option`; with no port attached the export pass returns
  immediately, so the default build pays nothing. `PhysicsBacked` marks entities whose
  transform belongs to an external engine. Four lines implement the port for any
  engine; the crate ships none, and ADR 0021's integration notes say why.
* **Networking.** `NpcNetState` is a fixed-width wire format (`NpcNetState::BYTES`,
  pinned by a test) with `encode`/`decode`; `InterestSet` + `collect_interest` produce a
  bounded, deterministically ordered set for one viewer. Both are replicon-shaped and
  replicon-free, so a game's replicon version is its own business.
* **Rendering.** `RenderPlan` is a sorted, culled instance list with a per-instance tint
  and variant. It is a `Vec` a renderer can read; whether that renderer is a Bevy
  material, an instanced mesh or a custom pass is not this crate's concern.

Feature flags follow the same principle. `serde` (authoring and saves), `memory`
(social memory) and `simd` are opt-in or opt-out with measured, documented reasons;
none of them pulls an engine dependency in.

## Consequences

* The crate's dependency tree stays small enough to compile quickly, and a game's
  choice of physics or netcode does not constrain its crowd library.
* The user writes a dozen lines per integration. That is the trade: a dozen lines they
  control, against a version conflict they do not.
* Because the ports are plain traits, they are testable without the engine: the tests in
  `adapters::secondary` use recording stubs, which is also how a game can test its own
  adapter.

## Alternatives considered

* **Optional feature-gated adapters.** Tempting, and it turns every Avian release into a
  patch release of this crate. Rejected on maintenance grounds, not on principle, a
  maintained integration is welcome as a separate crate that implements the port.
* **A callback per integration.** Loses the typed contract and the object safety that
  make the bridge cheap to store.
