# `ashfall-gpu`

The Ashfall scene, its shipped meshes and its shot table drawn through an instanced `wgpu`
pipeline instead of the software rasteriser in `demo/`. It exists to answer one question the
software renderer cannot: what does a frame cost when the drawing is not the bottleneck.

Nothing here is required to use the library. It is a front end that observes the simulation the
same way a game would.

## Running it

```text
cargo run --release --manifest-path demo-gpu/Cargo.toml -- --backend auto --view all
```

`--backend auto` opens the first adapter that answers, in this order: Metal, then Vulkan, then GL,
then `noop`. Pass `metal`, `vulkan` or `gl` to insist, and `noop` to encode every command and draw
none — that one runs anywhere, including a runner with no driver, and writes a blank image.

Metal is first because it is the only backend an Apple machine has: a Mac has neither Vulkan nor
GL, so without it the demo falls through to `noop` and writes a blank image while the hardware sits
idle. Metal also comes before Vulkan on purpose — a Mac with `MoltenVK` installed would answer with
it, and a frame time measured through a translation layer describes the layer, not the GPU.

| flag | meaning |
| --- | --- |
| `--backend auto\|metal\|vulkan\|gl\|noop` | which wgpu backend to open |
| `--view district\|street\|hero\|skyline\|procession\|all` | which shot to draw (default `all`) |
| `--width N --height N` | frame size in pixels (default 600x375, at least 1 and at most 16384 a side, and no more than the device allows — 8192 with the default wgpu limits) |
| `--frames N` | frames to time after two warm-up frames (default 20) |
| `--population N --ticks N --seed N --extent M` | the simulation the frame is drawn from |
| `--out DIR` | where the PNGs go (default `target/gpu`) |

Each view prints one JSON line with three numbers: the median CPU time to encode and submit the
frame, the median time to submit *and* wait for the device, and the count of lit pixels. The first
is what the game thread pays; the second is what a frame takes end to end on this device.

The adapter label and whether it reports itself as software are printed first. **Read them.** A
`llvmpipe` or `lavapipe` adapter is a CPU pretending to be a GPU: it is a correctness target, not a
performance one, and a frame time measured on it says nothing about a discrete card.

## Tests

```text
cargo test --offline --manifest-path demo-gpu/Cargo.toml
```

17 library tests and 10 binary tests, none of which need a device: the readback row packing, the
CLI parser, the instance layout `bytemuck` hands to wgpu, the generated cube and plane geometry
and their winding, the colour and byte transforms, and the sizes of the packed structs. The two
integration tests drive the real pipeline and skip when no adapter answers.

The images are *not* compared against the software renderer's. Two rasterisers disagree on
coverage rules, sampling and shading at the last bit by construction, so the equivalence that
matters is the scene, the shot table and the instance stream — which both front ends build from
the same library calls — and the PNGs are for a human to look at.

## Benchmark

```text
cargo bench --offline --manifest-path demo-gpu/Cargo.toml
```

Two groups: `wgpu` times a full frame with the submit and the wait separated, and `wgpu_upload`
times the per-frame instance upload alone, which is the part that scales with population rather
than with draw calls. On a software adapter, expect the upload to dominate and the draw to look
almost free — that is the adapter, not the pipeline.

`docs/dev/tuning/playbooks/gpu-demos.md` is the instruction set this crate was built against: environment
classification, adapter policy, the pipeline recipe, the shader rules, the correctness gates and
the benchmarking traps.
