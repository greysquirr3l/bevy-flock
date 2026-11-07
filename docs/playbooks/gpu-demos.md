# Building a GPU demo and its benchmark

This page is written for the agent or developer adding a GPU front end to the playground, and for the
one who will later have to trust the numbers it prints. Follow it in order. Every rule here exists
because breaking it produced a wrong picture or a wrong measurement in this repository.

The reference implementation is `demo-gpu/`: a `wgpu` renderer for the same Ashfall scene the software
rasteriser draws, with `benches/frame.rs` costing a frame. Read it alongside this page.

## 1. Know which crate you are writing

| Crate | May depend on | Must not |
| --- | --- | --- |
| `bevy-flock` (library) | Bevy ECS + math only | any renderer, any window, any GPU API |
| `demo` | `bevy-flock`, Bevy ECS, standard library | `wgpu`, `bevy_render`, a window |
| `demo-gpu` | `demo`, `wgpu`, `bytemuck`, `futures-lite` | duplicating scene or asset logic that `demo` owns |

The library never learns that a GPU exists. A GPU demo is a *consumer* of the same scenario, scene and
shot table the CPU demo uses, so the two can be compared. If a GPU front end needs something the
scenario does not expose, add it to `demo` and use it from both, or the comparison stops being one.

## 2. Classify the machine before writing a line

A GPU demo is only as good as the adapter under it, and a machine can lie to you in three ways: no
display, a display but no driver, or a driver that is a CPU. Establish which one you have and write
the answer down. Every figure produced later has to carry it.

```bash
echo "DISPLAY=${DISPLAY:-unset} WAYLAND=${WAYLAND_DISPLAY:-unset}"
ldd --version | head -1                                  # glibc floor for the driver
for lib in libvulkan.so.1 libEGL.so.1 libGL.so.1 \
           libxkbcommon.so.0 libwayland-client.so.0 \
           libX11.so.6 libasound.so.2 libudev.so.1; do
  printf '%-22s %s\n' "$lib" "$(ldconfig -p | grep -c "$lib")"
done
ls /usr/share/vulkan/icd.d/ 2>/dev/null                   # which ICDs exist
ls /dev/dri 2>/dev/null || echo "no render node"
vulkaninfo --summary 2>/dev/null | sed -n '1,25p'
```

Interpret it the same way every time:

- **A hardware adapter.** `deviceType = PHYSICAL_DEVICE_TYPE_DISCRETE_GPU` (or `INTEGRATED_GPU`) and a
  `/dev/dri/renderD*` node. GPU timings mean what they say. Record the adapter string.
- **A software adapter.** `llvmpipe`, `swiftshader`, or `deviceType = CPU`. This is a *CPU
  rasteriser behind a GPU API*. Frames are correct, timings are real, and both describe that driver
  only. Say so in every artefact: a 900 ms frame on `llvmpipe` is not evidence about a discrete GPU.
- **No adapter at all.** Headless CI, usually. The demo must still build, still encode, and still be
  testable. That is what the noop backend is for (§4).

Never write "the GPU is faster" from a software adapter. Never report a frame time without the
adapter, the resolution and the triangle count next to it.

## 3. Reuse the scenario, not the pixels

The demo owns the deterministic part: population, city layout, crowd placement, the shot table
(`render3d::shots`), the mesh loader (`gltf::Mesh`, `Meshes`), the material table and the atmosphere.
A GPU front end consumes those and must not re-derive them, or the two images will disagree for
reasons no one can debug.

Keep the contract narrow and by-reference:

```rust
pub fn open(scene: &Scene, meshes: &Meshes, backend: Backend, w: u32, h: u32) -> Result<Self, Error>;
pub fn set_shot(&mut self, shot: &Shot);
pub fn draw(&mut self);
pub fn wait(&self);
pub fn stats(&self) -> Stats;                 // instances, draws, triangles
pub fn read_rgba(&mut self) -> Result<Vec<u8>, Error>;
```

`set_shot` is where a shot is interpreted — camera, light, fog, **and what it can see**. Mirror the
software renderer's rules exactly, or the comparison is between two different pictures:

| Rule | Software | GPU |
| --- | --- | --- |
| Bodies | within `draw_distance`, beyond `body_margin`, nearest `max_bodies` | identical |
| Ruins | within `block_distance` | identical, and never counted against `max_bodies` |
| Ground | a tiled plane out to `ground_reach` | one large quad, always drawn |

Two mistakes found the hard way, both worth a test: a cap applied to the whole selection drops every
ruin (420 bodies are always nearer than the first block), and a descending sort keeps the *farthest*
bodies under a nearest-first cap. Assert on selected counts, not just on "something was drawn".

## 4. Pick the backend policy once, in one place

```rust
pub enum Backend { Auto, Metal, Vulkan, Gl, Noop }
```

- `Auto` asks for `METAL | VULKAN | GL | NOOP` and takes what it gets. It is the default everywhere.
- `Metal`/`Vulkan`/`Gl` exist so a bug can be pinned to one driver without editing code.
- The order in `Auto` is a decision, not a set. Metal comes first because it is the only backend an
  Apple machine has, and because a Mac with `MoltenVK` installed would otherwise answer with a
  translation layer whose timings describe the layer rather than the hardware. The backends a
  platform does not have cost nothing: `wgpu` compiles the Metal backend away on anything that is
  not Apple, so one `Auto` serves every machine.
- `Noop` encodes and validates every command and executes none. It needs no driver, so CI can prove
  the encode path — instance upload, bind groups, pipeline state, render pass — on a machine with no
  GPU at all. `wgpu` requires the option to be switched on explicitly:

```rust
let mut options = wgpu::BackendOptions::default();
options.noop = wgpu::NoopBackendOptions::enabled();
```

Every `open` failure must be a typed error carrying the reason (`NoAdapter`, `NoDevice`,
`MissingModel`). A renderer that skips a model it cannot find draws a hole and calls it a picture.

## 5. Build the pipeline the boring way

One instanced pipeline, one uniform block, one storage buffer of instances, one off-screen target.

1. **Instance buffer.** `struct Instance { model: mat4x4<f32>, tint: vec4<f32> }` in a
   `BufferBindingType::Storage { read_only: true }` binding indexed by `@builtin(instance_index)`.
   Storage, not a vertex buffer: it keeps the vertex layout to position and normal, and it lets the
   whole crowd be one upload.
2. **Group by model, then by distance.** Sort the selection so each model owns a contiguous slice,
   then draw one `draw_indexed(0..count, 0, first..first+n)` per model. Six to eight draws for a city
   is right; two thousand is a bug in the sort.
3. **Generated meshes for procedural geometry.** Ruins and the ground go through the same pipeline as
   the characters, as a unit cube and a unit quad with their own instance transforms. A second pass
   with a second shader is more code for the same pixels.
4. **Place by half-extent.** `BlockInstance::centre` is *at ground level* and `size` is a
   *half*-extent, so the cube's centre is one half-extent up. Getting this wrong floats the city.
5. **Off-screen target, sRGB, depth32float.** `Rgba8UnormSrgb` so the shader works in linear light
   and the hardware encodes. Add `COPY_SRC` only when you intend to read the frame back.

The wgpu 30 shapes that cost the most time to discover, so check them first if the compiler complains:
`InstanceDescriptor` has no `Default` and is taken by value; `RenderPipelineDescriptor` carries
`multiview_mask`; `DepthStencilState` takes `Option<bool>` and `Option<CompareFunction>`;
`poll` takes `PollType::Wait { submission_index, timeout }`; `get_mapped_range` returns a `Result`;
texture copies need `COPY_BYTES_PER_ROW_ALIGNMENT` (256) row alignment.

## 6. Shader rules

Three lines prevent most "why does it look wrong" questions:

```wgsl
let lambert = max(dot(normalize(nrm), normalize(camera.sun.xyz)), 0.0);
let hemi    = mix(camera.bounce.rgb, camera.sky.rgb, 0.5 + 0.5 * normal.y);
let lit     = tint * (camera.sun_colour.rgb * lambert + hemi) * camera.exposure.x;
```

1. `sun` points **towards** the light, matching `Atmosphere`, not away from it.
2. The atmosphere's radiance is authored for the software renderer's exposure. Apply the same
   divisor or every surface clips to white — that is what a `0.34` in a uniform is doing.
3. Fog resolves to `atmosphere.fog` at `fog_far`, and the pass **clears to the same colour**, so the
   far plane and the sky meet instead of showing a seam.

Differences that are acceptable, and must be documented rather than hidden: a flat fog-coloured sky
instead of the software gradient, no procedural ground grid, no impostor LOD.

## 7. Costing a frame: the part everyone gets wrong

A frame has a latency and a throughput, and on a CPU-backed adapter they are the same number because
the CPU *is* the GPU. Name each case after what the loop does:

| Case | Loop | Answers |
| --- | --- | --- |
| `frame` | `draw(); wait();` | latency of one frame |
| `eight_frames_per_drain` | 8 × `draw()`, then `wait()` | throughput |
| `frame_with_readback` | `draw(); wait(); read_rgba();` | cost of shipping the frame back |

Rules that keep the numbers honest:

1. **Setup outside the timing loop.** Open the device, upload every mesh, warm up one frame, and only
   then start collecting. `criterion` measures what is inside `iter`.
2. **A loop with no drain measures saturation.** A case that submitted without ever waiting reported
   992 ms here while the same submit measured 0.1 ms inside the binary, because the queue had nowhere
   to go. If a "CPU half" number looks like a frame time, you are measuring the queue.
3. **Never compare a GPU case with a CPU case without saying which half.** The software rasteriser's
   whole frame *is* CPU work; a GPU frame is mostly not. Put both in the same table and label the
   column, or the comparison is a lie with a decimal point.
4. **Record the adapter.** Print it in the benchmark output and put it in the label:
   `gpu/{backend}/{w}x{h}` is not enough without `llvmpipe` versus `RTX 4070` in the record.
5. **Skip loudly, never silently.** With no adapter, print why and return; a benchmark that reports
   zero reads as "free".
6. **Sample size.** Twenty samples is enough for a 1 s case; for a sub-millisecond case use the
   default. Do not lower `warm_up_time` below 3 s.

A recorded run looks like this (software Vulkan, so it is a statement about `llvmpipe`):

| Case | Median |
| --- | --- |
| `frame` | 974.56 ms |
| `eight_frames_per_drain` | 7.893 s (≈986.6 ms/frame) |
| `frame_with_readback` | 997.35 ms |

## 8. Correctness gates

Four tests, all cheap, all in `tests/`:

1. **Encode without a driver.** Noop backend, real scene: `stats().instances > 0`,
   `triangles > 0`, two `draw()`/`wait()` pairs with no validation error.
2. **Determinism on a real adapter.** Draw, read back, draw again, read back: the two buffers must be
   byte-identical. Anything else is a race or an uninitialised buffer.
3. **Not a blank frame.** Count pixels that differ from `clear_colour()`, and require a sensible
   fraction. Counting non-zero bytes counts the fog and passes on an empty frame.
4. **A typed failure for a missing model.** Hand the renderer a scene naming a model the table does
   not hold and assert `MissingModel`.

## 9. Artefacts and documentation duties

A GPU demo is not finished until these exist:

1. Images written to `docs/demo/`, with a caption that names the adapter and says whether it is a
   CPU rasteriser. Regenerate rather than reusing an older frame.
2. A row in `docs/performance.md` in the same shape as the others: adapter, resolution, scene,
   triangles, median and interval, and which case it is.
3. Any change to a shipped front end goes into its ADR as an amendment, not a rewrite: the original
   decision and the reason it changed both need to survive.
4. `CHANGELOG.md` entry, and a `plan.toml` record with `delivered_in` set to the commit.
5. CI: build the crate, run its no-driver test, and compile its benchmark. Do not run the benchmark
   in CI — a shared runner has no adapter and the numbers would be noise.

## 10. When to use Bevy's renderer instead

Reach for `bevy_render`, `bevy_core_pipeline` and `bevy_pbr` when the demo needs to be a *window*:
interactive camera input, PBR materials, shadows, post-processing, glTF animation via
`bevy_animation`. The raw `wgpu` path above exists because it is measurable, dependency-light,
headless-friendly and shares the instance table with the software renderer. Those are the properties
the comparison needs, not a preference.

If you take the Bevy path, keep the same split: a scene/instance layer that both renderers read, a
camera derived from `Shot`, and a benchmark that names the adapter. Do not let the ECS become the only
place the crowd exists, or the two front ends will drift and the software path will silently become
the reference for a different scene.

## 11. Definition of done

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --release                      # includes the no-driver and determinism gates
cargo bench --bench frame --no-run         # the benchmark compiles
python3 ../scripts/lint_audit.py           # the repository's lint spine still holds
```

Then check, by hand: one frame rendered at a size you can actually look at, in the same composition
as the software render, with the adapter written down next to it.
