# ashfall-playground

The Ashfall crowd and the city, drawn by Bevy's PBR pipeline in a window you can walk through.
On macOS that is Metal; elsewhere it is Vulkan or GL. Same scene as the software and offscreen
demos, one GPU pipeline that is not hand-written.

```sh
cargo run --release -p bevy-flock-playground -- --view district
```

| Control | |
|---|---|
| `W` `A` `S` `D` | move |
| `Q` / `E` | down / up |
| shift | sprint |
| right mouse (hold) | look |
| `Tab` | next shipped view |
| `Escape` | quit |

There is no screenshot of this crate's output in the repository yet. The images in `docs/demo`
are the software renderer's, and a Bevy frame at the same camera will not match one pixel for
pixel: different rasteriser, different shading model, different anti-aliasing. A capture from a
machine with a GPU is worth adding, and worth labelling as what it is.

## Flags

| Flag | |
|---|---|
| `--view NAME` | open at `district`, `street`, `hero`, `skyline` or `procession` |
| `--frames N` | draw N frames and exit, for a smoke test with nobody watching |
| `--width N` `--height N` | window size, at least 1 and at most 16384 a side (default 1280x800) |
| `--backend NAME` | `auto` (default), `metal`, `vulkan`, `gl` |
| `--population N` | NPCs to simulate, default 2000 |
| `--ticks N` | ticks before the first frame, default 600 |
| `--seed N` | layout seed, default 7 |
| `--extent M` | layout half-extent, metres, default 900 |
| `--help` | usage |

`--backend metal` on a machine without Metal is not a fallback: wgpu finds no adapter and Bevy
reports it. That is deliberate, because a demo that silently draws nothing looks like a scene bug.

## What is simulated, and where

The crowd is the library's: `Playground` builds a city and populates it, `place_crowd` and `tick`
run the simulation, and `bodies()` hands a renderer the people it should draw. Nothing in this
crate steps the crowd; it draws the state the demo produced, which is why the same seed and tick
count show the same city here, in `demo`'s software renderer, and in `demo-gpu`'s offscreen one.

Three things are this crate's own, and each is a pure function with tests in `src/scene.rs`:

- **Geometry.** `bevy_mesh` splits a loaded glTF mesh into Bevy's vertex attributes; `cube_mesh`
  builds the unit box the ruined blocks are drawn from.
- **Placement.** `transform_for` and `block_transform` map an instance's position, yaw and scale
  onto a `Transform`.
- **Materials.** Tints are quantised to four levels a channel before a material is created, so a
  crowd of two thousand shares at most 64 materials instead of one each. One material a body is one
  bind group a body, and the draw calls then outnumber the triangles.

## Verification

`cargo test` covers the parts that do not need a GPU: geometry conversion for every shipped model,
the cube's closed topology, transform mapping, tint quantisation and its bounds, the camera's
angle/forward round trip and orthonormal basis, the pitch clamp, view cycling, and every flag
including the window-size pair.

The window itself is not covered by a test and cannot be: it needs a display and a driver. A
machine with neither can still check that the scene and the app build, and `--frames` exists to
make a real GPU run a bounded, scriptable check:

```sh
cargo run --release -p bevy-flock-playground -- --backend metal --frames 120 --view district
```
