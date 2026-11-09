# ADR 0007, No renderer, no physics, no networking in this crate

- Status: Accepted
- Date: 2026-11-04

## Context

The temptation is obvious: a crowd crate that also ships instanced rendering, a
physics bridge and replication is a one-stop shop. It is also how a library
becomes unusable, because it forces a renderer, a physics engine and a networking
stack on a dedicated server that wants none of them.

## Decision

The crate contains no `Sprite`, no `Mesh`, no camera, no collider, no network
protocol. Optional integrations (avian, instancing, replicon, snapshots) are
separate features and separate adapters, each of which can be disabled.

The simulation emits state; a renderer observes it. The dependency arrow points
one way and nothing in `domain/` knows a renderer exists.

## Consequences

- `default-features = false` gives a headless simulation with no rendering
  dependency tree, which is what a server actually wants.
- The benchmark suite runs with `App::new()` and no plugins beyond this crate's,
  so its numbers describe simulation and not the GPU.
- A game that wants a specific look writes its own render adapter against
  `Transform` and `NpcIdentity::appearance_seed`. That is a small amount of code,
  and it is code the game was going to write anyway.
- Nobody can file "bevy-flock's shader is slow" as an issue, because there is no
  shader.

## Alternatives rejected

- **Bundle a default renderer behind a feature**, becomes the only renderer
  anyone tests, then the default, then a liability.
- **Depend on a specific physics engine**, avian and rapier users would each
  need a fork. Steering produces a desired velocity; what consumes it is the
  game's choice.
