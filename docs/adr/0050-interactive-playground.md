# ADR 0050 — Exploring the playground without a window

- **Status:** Accepted
- **Date:** 2026-11-05
- **Task:** `ashfall-demo-interactive`
- **Supersedes:** nothing
- **Related:** [ADR 0049](0049-row-major-neighbour-broadcast.md) (the kernels the demo draws),
  `demo/README.md`, `docs/performance.md`

## Context

The playground shipped five PNGs and a report: the city, the crowd, the assets and the timings could
be inspected, but not *explored*. A person looking at `district.png` cannot walk down the street it
shows, and a person evaluating the crowd cannot see it move.

Two front ends were asked for: interactive camera controls, or a command-line playback mode. There is
also a candidate library for the first — `bevy-archie`, another crate by the same author — and a
second request that came with it: measure the CPU cost of the software rasteriser against a wgpu
pipeline.

The sandbox this work runs in has no window system, no GPU and no `libvulkan`, `libxkbcommon` or
`libwayland-client`, which rules out everything a winit window needs before the first frame. The
constraint is not a sandbox quirk, either: the same is true of CI, of a remote shell, and of the
`demo` job that already has to run headless.

## Decision

**Explore in the terminal, and keep the crude parts crude on purpose.**

1. **A pure session core, no I/O.** `demo/src/session.rs` owns the camera, the current view, the lens
   and the draw budget; `step`/`advance` take an input snapshot and return; nothing in it touches a
   terminal, a clock or a file. Eleven unit tests drive it with synthesised input, which is what makes
   the interactive path testable in CI at all.
2. **Input is decoded, not read.** `demo/src/input.rs` turns bytes into `Command`s, with a 140 ms
   escape timeout so a lone `Esc` is not mistaken for the head of a sequence. A `Decoder` is fed bytes
   in tests and keys in the binary.
3. **The picture is the same rasteriser.** A cell is split by `▀` into two vertical pixels, so a
   `cols × rows` terminal shows `cols × rows·2` square pixels at roughly 20 kB a frame. No image
   protocol is assumed, because half of the terminals people actually use do not have one.
4. **A draw budget, not a smaller scene.** The interactive path changes the *shots*, not the world:
   420 bodies inside 240 m, ruins inside 300 m, triangles under 0.75 px dropped, and — the change that
   mattered — anything past 24 m drawn as a two-triangle camera-facing billboard. The same scene at
   full detail costs 850 ms a frame at 600×375 and about 1,100 ms at the district view's own
   resolution; the budget costs 19.0 ms at 112×72, which is 53 fps, and 38 ms in a real 112×88-pixel
   terminal, which is 26 fps measured.
5. **Playback is the same loop with a camera on a rail.** `--play N` steps the scripted flight, writes
   a PNG per frame plus `playback.json`, and needs no tty — so the feature can be exercised in CI, and
   the images can be assembled into a video without a screen recorder.
6. **`bevy-archie` is not wired in, and the seam is named.** Archie is a windowed, gamepad-and-keyboard
   front end: it needs a window, and the crate in this environment cannot open one. What it needs from
   this side is a `CameraInput` per frame and a `Session`, and both are public and tested. The ADR
   records the seam rather than pretending to a binding that could not be compiled here:
   `Session::camera()`, `Session::advance`, `Command`, and the `Budget` are the four things a windowed
   front end would consume.

The result, captured from a pty at 120 × 88 pixels and enlarged without smoothing:

![The street view in a terminal](../demo/ashfall-play.png)

## The measurement

`demo/benches/render_pipeline.rs` runs the same scene through both pipelines and records what the CPU
does:

| Case | Median |
| --- | --- |
| `software/full/600x375` | 850 ms |
| `software/interactive/112x72` | 19.0 ms |
| `wgpu/full/600x375` | 33.6 µs |
| `wgpu/interactive/112x72` | 34.7 µs |
| `wgpu_upload/2064_instances` | 106 µs |

The wgpu side runs on its `noop` backend: a real `wgpu::Device`, a real pipeline description, real
vertex and index buffers, a real render pass and a real submission, with the commands discarded
instead of executed. That is precisely the CPU work a wgpu game pays every frame and precisely *not*
the GPU work, which cannot be measured without a GPU. Read the table as two facts and no more:

- The CPU side of a wgpu frame is **34 µs** for 2,064 instances in six batches, and does not care
  whether the target is 600×375 or 112×72: the size of the framebuffer is the GPU's problem.
- The software path spends 850 ms on the full-detail frame and 19.0 ms on the terminal frame, because
  everything a GPU would parallelise it does on one core, one triangle at a time.

The ratio — 550× at terminal size, 25,000× at full detail — is not "wgpu is faster." It is the shape
of the cost: per-pixel and per-triangle work moves to the device, and the CPU keeps a fixed,
population-proportional cost that the impostor budget also attacks (a body past 24 m is two triangles
instead of 11,000).

One number in the table looks wrong and is not: an isolated `write_buffer` of the crowd costs 106 µs,
three times what the whole frame costs. Inside `frame()` the write is queued to the staging belt and
rides the submit; measured alone it flushes. A renderer that uploads outside a submit pays the flush,
which is why the benchmark keeps both cases.

## Consequences

- **The terminal is a first-class front end.** It runs over SSH, in a container, in CI, on a machine
  with no GPU, and on the sandbox that produced every artefact in `docs/demo/`.
- **The budget is data.** `session::Budget` is a plain `Copy` struct, so a caller that wants a richer
  picture (a bigger terminal, a faster machine) changes four numbers rather than the renderer.
- **The switch is hard, and the distance is eye to feet.** A body crossing the threshold in motion
  pops between an 11,000-triangle mesh and two triangles: there is no cross-fade band. At the sizes
  this front end targets a body is three or four pixels tall and both are a smudge, so a fade would
  buy nothing; on a 300-cell terminal it would be visible, and the answer there is a fade band rather
  than a lower threshold. The distance is measured from the camera to the body's feet, not to its
  silhouette, so a tall body crosses a fraction late — under a pixel at 24 m. Three tests pin the
  behaviour instead of leaving it to inspection: the switch is monotone in the threshold and exact at
  2 triangles per billboard, the quad's projection matches the camera's own matrix to within 2.5 px
  with a 0.26 width-to-height ratio, and every shipped view still carries an infinite threshold so the
  committed PNGs stay full detail.
- **`impostor_distance` defaults to infinity.** The shipped PNGs are made at full detail and stay
  byte-identical; the LOD exists for the steered view. A renderer that turned it on by default would
  have silently changed five committed images.
- **A windowed front end remains unwritten.** Archie would give game controller support, a resizable
  window and a real `wgpu` surface; the work it needs from this side is done and tested, and the
  binding is one file when a machine with a display is available.
- **The comparison is CPU-only by construction.** Any claim about end-to-end frame times on a GPU
  needs a GPU, and this ADR claims none.
