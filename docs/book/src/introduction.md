# bevy-flock

A crowd of NPCs that behaves like people, costs like arithmetic, and replays exactly.

`bevy-flock` is a Bevy library for spawning anywhere from one NPC to millions, giving each
of them a mind, and keeping the whole thing inside a frame budget the game chooses. It is
not a renderer, not a physics engine and not a networking layer; it is the part between
those that decides where everybody walks, what they are doing, and why.

## The promise

Three claims, each measured and each defended by a gate in CI:

1. **A crowd you can afford.** Ten thousand agents steer in 9.18 ms of a 60 Hz frame on the
   reference host, 1.8× of margin, and the cost per agent *falls* as the crowd grows,
   because each agent's work is local. Past a hundred thousand, population becomes density
   and the cost stops growing at all.
2. **Determinism you can ship.** Same seed, same world, tick for tick, across runs and
   across machines with the same binary. The random streams, the spatial index, the spawn
   order and the think scheduler are all ordered by construction, and a digest test fails
   if any of that drifts.
3. **Behaviour that is yours.** Trees, utility scoring, state machines, a director,
   dialogue, barks, groups and memory are all extension points. The crate supplies the
   machinery and a catalogue of leaves; the game supplies the judgement.

## Who this is for

- **Gameplay programmers** who want NPCs with a mind, not a `Vec<Transform>` with noise
  added to it.
- **Technical directors** who need to know what the crowd costs before they promise it in a
  design doc, and need the answer to survive a year of feature work.
- **Engine tinkerers** who want to read a crate where the hot path is small enough to hold
  in your head and the decisions are written down.

## What it is not

- **Not a renderer.** `adapters::primary::render` produces a sorted list of instances with a
  variant and a tint; drawing it is your job, and there is no gizmo, sprite or mesh
  dependency in the tree.
- **Not an ECS tutorial.** It assumes you know what a `Query` is. It does explain the parts
  that are unusual, the scheduler, the tiers, the bands, because those are the parts that
  make the numbers work.
- **Not finished.** `0.1.0` is the foundation plus the engine: crowds, borders, behaviour,
  memory, saving, diagnosis. The status table in the README is the honest list, and the
  plan in `plan.toml` is what remains.

## How to read this

If you want a crowd on screen, read [Getting started](getting-started.md) and stop. If you
want to understand why it is fast, read [The model](the-model.md) and
[Crowds at scale](crowds-at-scale.md). If you are writing gameplay, read
[Behaviour](behaviour.md), [Memory](memory.md) and [Extending it](extending.md). If
something is wrong at 2 a.m., read [When it goes wrong](debugging.md).

Every number in this book comes from `docs/dev/tuning/performance.md`, which records the host, the
toolchain and the lockfile the measurement used. Nothing here is a projection.
