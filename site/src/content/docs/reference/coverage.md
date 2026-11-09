---
title: Feature coverage
description: What the crate does, what it deliberately does not, and what "framework" would have meant.
---

`bevy-flock` is a simulation library for NPCs and crowds. It is not a full NPC game framework, and the difference matters
more before you adopt it than after.

## In the crate

| area | status |
| --- | --- |
| Spatial partition and neighbour queries | complete, parallel, byte-identical across worker counts |
| Steering: boids, avoidance, path following | complete, scalar and wide kernels |
| Behaviour trees and state machines | complete, with cost hints per leaf |
| Pluggable logic per NPC, per group, per population | complete via the registry |
| Tiered cognition with a virtual-cost budget | complete, calibrated |
| Ambient continuum above ~250k | complete, nine gates verified |
| Social memory (optional) | complete, bounded, feature-gated |
| Save/load with v1→v2 compatibility (optional) | complete, feature-gated |
| Navigation (optional) | polygon navmesh, adjacency, area costs, string-pulled routes |
| Determinism guarantees | tested: same seed + features + platform ⇒ same digest |

## Deliberately out

| area | why |
| --- | --- |
| Rendering | Bevy does it; a simulation crate that renders is two crates in a trench coat |
| Physics and collision | overlap between crowd avoidance and a real physics engine is a bug factory |
| Networking and replication | authoritative-server design is game-specific; the crate hands you deterministic state to send |
| Asset loading, audio, UI | not simulation |
| Dialogue *content* | the crate schedules and delivers barks; your writers supply words |

## What "framework" would have meant

A framework would own your `main`, your schedule and your asset pipeline. This crate owns a resource, a set of systems
and a data model, and lets you keep the rest. That is a deliberate trade: less magic, fewer surprises at upgrade time, and
a boundary you can see in `cargo tree`.

If you need the framework layer, the `playground` crate in the repository is a worked example of building one on top.
