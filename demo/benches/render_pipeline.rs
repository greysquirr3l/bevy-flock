//! What a frame costs on the CPU: the software rasteriser against a wgpu pipeline.
//!
//! The two are not the same job, and the numbers only mean something if that is said out loud:
//!
//! * **`software`** runs the whole renderer in `raster.rs` — transform, clip, raster, shade, write
//!   the pixels — and hands back an image. This is the demo's renderer, and the thing that writes
//!   every shipped PNG.
//! * **`wgpu`** builds the same draw list and encodes it with a real wgpu device on its `noop`
//!   backend: the same pipeline description, vertex and index buffers, instance upload, render pass
//!   and submission, with the commands discarded instead of executed. That is exactly the CPU work a
//!   wgpu game pays every frame — command encoding, validation and buffer traffic — and it is
//!   exactly not the GPU work, which cannot be measured without a GPU.
//!
//! So the comparison answers "which pipeline is cheaper for the CPU in this sandbox," not "which
//! renders faster end to end." A GPU would collapse the shading and rasterisation the software path
//! does here, and would then be limited by the encode cost on the left-hand side of the table plus
//! whatever the device does. The honest reading is the ratio: the software path spends its time on
//! pixels, and the wgpu path spends a small fixed amount on the API and hands the rest off.
//!
//! Every case draws the same scene: the shipped city at seed 7, the district view's cameras, and a
//! crowd of 2,000 bodies built from the same generator the demo places.
use std::hint::black_box;

use bevy_flock_demo::city::CityLayout;
use bevy_flock_demo::render3d::{self, Meshes, Shot};
use bevy_flock_demo::scene::{Body, Scene};
use bevy_flock_demo::session::Budget;
use bytemuck::{Pod, Zeroable};
use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use wgpu::util::DeviceExt;

/// The seed the demo ships with, so the benchmark measures the scene the artefacts show.
const SEED: u64 = 7;

/// The city's half-width, metres: the demo's default extent.
const EXTENT: f32 = 900.0;

/// How many bodies stand in the scene.
const BODIES: u64 = 2_000;

/// Frame size for the full-detail cases, the shipped PNG's aspect at half its width.
const FULL_WIDTH: u32 = 600;

/// Frame height for the full-detail cases.
const FULL_HEIGHT: u32 = 375;

/// Frame size for the interactive cases: a 112 by 72 terminal cell picture, two pixels a cell.
const TERMINAL_WIDTH: u32 = 112;

/// Frame height for the interactive cases.
const TERMINAL_HEIGHT: u32 = 72;

/// The scene the benchmarks draw.
struct Fixture {
    /// The instanced scene.
    scene: Scene,
    /// The meshes, loaded once.
    meshes: Meshes,
    /// The five views.
    shots: Vec<Shot>,
}

impl Fixture {
    /// Build the city, the crowd and the meshes once; [`None`] if the shipped GLBs are missing.
    fn new() -> Option<Self> {
        let city = CityLayout::generate(bevy_flock::domain::ids::SimSeed::new(SEED), EXTENT);
        let centre = render3d::settlement_centre(&city);
        let bodies: Vec<Body> = (0..BODIES)
            .map(|index| Body {
                archetype: bevy_flock::domain::temperament::Archetype::Urchin,
                // A lattice with a jitter, which is what the demo's crowd looks like from above:
                // dense enough that the culling and batching have something to do.
                position: centre
                    + bevy_math::Vec3::new(
                        (index % 50) as f32 * 3.0 - 75.0,
                        0.0,
                        (index / 50) as f32 * 3.0 - 60.0,
                    ),
                yaw: 0.0,
                appearance_seed: index,
            })
            .collect();
        let scene = Scene::build(&city, &bodies);
        let shots = render3d::shots(&city, &bodies);
        let meshes = Meshes::load().ok()?;
        // The layout has done its job once the shots are framed: the scene is what is drawn.
        let _ = city;
        Some(Self {
            scene,
            meshes,
            shots,
        })
    }

    /// The district view at full detail: what writes the shipped PNGs.
    fn full(&self) -> Option<&Shot> {
        self.shots.first()
    }
}

/// One instance as the GPU sees it: a model matrix and a colour.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
struct InstanceRaw {
    /// Column-major model matrix.
    model: [[f32; 4]; 4],
    /// RGBA tint.
    colour: [f32; 4],
}

/// One vertex as the GPU sees it.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
struct VertexRaw {
    /// Position, metres.
    position: [f32; 3],
    /// Normal, unit length.
    normal: [f32; 3],
}

/// A loaded model's buffers and how many indices it has.
struct Batch {
    /// Vertex buffer.
    vertices: wgpu::Buffer,
    /// Index buffer.
    indices: wgpu::Buffer,
    /// Index count to draw.
    count: u32,
    /// How many instances this batch draws.
    instances: u32,
}

/// A wgpu device, a pipeline, and the buffers one frame needs — everything except the frame itself.
struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    camera: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    target: wgpu::TextureView,
    depth: wgpu::TextureView,
    instances: wgpu::Buffer,
    instance_bytes: Vec<u8>,
    batches: Vec<Batch>,
    width: u32,
    height: u32,
}

impl Gpu {
    /// A noop device, a pipeline with one instanced draw per model, and the scene uploaded; [`None`]
    /// when the build has no `wgpu` backend that can open without a display.
    ///
    /// Long because a pipeline description is a list: adapter, device, shader, layout, pipeline,
    /// attachments, batches. Splitting it would hide the order the API requires.
    #[expect(
        clippy::too_many_lines,
        reason = "a wgpu pipeline is a linear description; splitting it hides the required order"
    )]
    fn new(fixture: &Fixture, width: u32, height: u32) -> Option<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::NOOP,
            flags: wgpu::InstanceFlags::empty(),
            memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
            backend_options: wgpu::BackendOptions {
                noop: wgpu::NoopBackendOptions::enabled(),
                ..Default::default()
            },
            display: None,
        });
        let adapter = futures_lite::future::block_on(
            instance.request_adapter(&wgpu::RequestAdapterOptions::default()),
        )
        .ok()?;
        let (device, queue) = futures_lite::future::block_on(
            adapter.request_device(&wgpu::DeviceDescriptor::default()),
        )
        .ok()?;

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("ashfall"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ashfall"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let camera = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("camera"),
            size: 64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("ashfall"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("ashfall"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<VertexRaw>() as wgpu::BufferAddress,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x3,
                            offset: 0,
                            shader_location: 0,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x3,
                            offset: 12,
                            shader_location: 1,
                        },
                    ],
                })],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba8UnormSrgb,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            cache: None,
            multiview_mask: None,
        });

        let target = device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("colour"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            })
            .create_view(&wgpu::TextureViewDescriptor::default());
        let depth = device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("depth"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Depth32Float,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            })
            .create_view(&wgpu::TextureViewDescriptor::default());

        // The draw list: one batch per model, instances grouped by model, exactly as the render
        // plan's batching contract asks a renderer to group them.
        let mut by_model: std::collections::BTreeMap<&'static str, Vec<InstanceRaw>> =
            std::collections::BTreeMap::new();
        for instance in &fixture.scene.bodies {
            by_model
                .entry(instance.model)
                .or_default()
                .push(instance_matrix(instance));
        }
        for group in [
            &fixture.scene.pylons,
            &fixture.scene.shelters,
            &fixture.scene.stalls,
        ] {
            for instance in group {
                by_model
                    .entry(instance.model)
                    .or_default()
                    .push(instance_matrix(instance));
            }
        }
        let mut batches = Vec::new();
        let mut instance_bytes = Vec::new();
        for (slug, instances) in by_model {
            let Some(mesh) = fixture.meshes.get(slug) else {
                continue;
            };
            let vertices: Vec<VertexRaw> = mesh
                .positions
                .iter()
                .zip(mesh.normals.iter())
                .map(|(position, normal)| VertexRaw {
                    position: *position,
                    normal: *normal,
                })
                .collect();
            let vertices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(slug),
                contents: bytemuck::cast_slice(&vertices),
                usage: wgpu::BufferUsages::VERTEX,
            });
            let indices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(slug),
                contents: bytemuck::cast_slice(&mesh.indices),
                usage: wgpu::BufferUsages::INDEX,
            });
            instance_bytes.extend_from_slice(bytemuck::cast_slice(&instances));
            batches.push(Batch {
                vertices,
                indices,
                count: mesh.indices.len() as u32,
                instances: instances.len() as u32,
            });
        }
        let instances = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("instances"),
            size: instance_bytes.len().max(16) as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        // The instance data does not change between frames in this benchmark; it is written once
        // here so the loop below measures the per-frame upload of the same bytes, which is what a
        // renderer does when a crowd moves.
        queue.write_buffer(&instances, 0, &instance_bytes);

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ashfall"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: camera.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: instances.as_entire_binding(),
                },
            ],
        });
        let gpu = Self {
            device,
            queue,
            pipeline,
            camera,
            bind_group,
            target,
            depth,
            instances,
            instance_bytes,
            batches,
            width,
            height,
        };
        gpu.upload_camera(fixture.full()?);
        Some(gpu)
    }

    /// Write the camera's view-projection for `shot`.
    fn upload_camera(&self, shot: &Shot) {
        let aspect = self.width as f32 / self.height as f32;
        let matrix = shot.camera.view_projection(aspect);
        self.queue.write_buffer(
            &self.camera,
            0,
            bytemuck::cast_slice(&[matrix.to_cols_array()]),
        );
    }

    /// One frame of CPU work: upload the crowd, encode the pass, submit.
    fn frame(&self) {
        self.queue
            .write_buffer(&self.instances, 0, &self.instance_bytes);
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("frame"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.target,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.7,
                            g: 0.66,
                            b: 0.62,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            let mut first = 0;
            for batch in &self.batches {
                pass.set_vertex_buffer(0, batch.vertices.slice(..));
                pass.set_index_buffer(batch.indices.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..batch.count, 0, first..first + batch.instances);
                first += batch.instances;
            }
        }
        self.queue.submit([encoder.finish()]);
    }
}

/// The instance transform for one scene instance: scale, yaw, translate — and its colour.
fn instance_matrix(instance: &bevy_flock_demo::scene::Instance) -> InstanceRaw {
    let (sin, cos) = instance.yaw.sin_cos();
    let s = instance.scale;
    let position = instance.position;
    InstanceRaw {
        model: [
            [cos * s, 0.0, -sin * s, 0.0],
            [0.0, s, 0.0, 0.0],
            [sin * s, 0.0, cos * s, 0.0],
            [position.x, position.y, position.z, 1.0],
        ],
        colour: [instance.tint[0], instance.tint[1], instance.tint[2], 1.0],
    }
}

/// A vertex shader that projects instanced geometry with one Lambert term, which is all the software
/// path does that a GPU would need to be told.
const SHADER: &str = r"
struct Camera { view_projection: mat4x4<f32> };
struct Instance { model: mat4x4<f32>, colour: vec4<f32> };

@group(0) @binding(0) var<uniform> camera: Camera;
@group(0) @binding(1) var<storage, read> instances: array<Instance>;

struct VsIn {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) colour: vec4<f32>,
};

@vertex
fn vs(input: VsIn, @builtin(instance_index) index: u32) -> VsOut {
    let instance = instances[index];
    let world = instance.model * vec4<f32>(input.position, 1.0);
    let normal = normalize((instance.model * vec4<f32>(input.normal, 0.0)).xyz);
    let sun = normalize(vec3<f32>(-0.42, 0.72, 0.55));
    let lambert = max(dot(normal, sun), 0.0) * 0.7 + 0.3;
    var out: VsOut;
    out.clip = camera.view_projection * world;
    out.colour = vec4<f32>(instance.colour.rgb * lambert, 1.0);
    return out;
}

@fragment
fn fs(input: VsOut) -> @location(0) vec4<f32> {
    return input.colour;
}
";

/// The district view as the terminal front end draws it: the interactive budget, same camera.
fn budgeted_shot(fixture: &Fixture) -> Option<Shot> {
    let full = fixture.full()?;
    let mut shot = Budget::default().apply(full);
    shot.camera = full.camera;
    Some(shot)
}

/// The software rasteriser, at full detail and at the interactive budget.
fn software(criterion: &mut Criterion) {
    let Some(fixture) = Fixture::new() else {
        eprintln!("render_pipeline: skipping the software cases, the shipped meshes did not load");
        return;
    };
    let Some(terminal) = budgeted_shot(&fixture) else {
        eprintln!("render_pipeline: skipping, the shot table is empty");
        return;
    };

    let mut group = criterion.benchmark_group("software");
    group.throughput(Throughput::Elements(BODIES));
    group.bench_function("full/600x375", |bencher| {
        bencher.iter(|| {
            let Some(full) = fixture.full() else {
                return;
            };
            let (image, stats) = render3d::render(
                black_box(&fixture.scene),
                black_box(&fixture.meshes),
                black_box(full),
                FULL_WIDTH,
                FULL_HEIGHT,
            );
            black_box((image.fingerprint(), stats.triangles));
        });
    });
    group.bench_function("interactive/112x72", |bencher| {
        bencher.iter(|| {
            let (image, stats) = render3d::render(
                black_box(&fixture.scene),
                black_box(&fixture.meshes),
                black_box(&terminal),
                TERMINAL_WIDTH,
                TERMINAL_HEIGHT,
            );
            black_box((image.fingerprint(), stats.triangles));
        });
    });
    // What the cross-fade costs: the same interactive frame with the band switched off, so the pair
    // differ only in the bodies that are inside it.
    let mut hard = terminal.clone();
    hard.impostor_band = 0.0;
    group.bench_function("interactive-band-off/112x72", |bencher| {
        bencher.iter(|| {
            let (image, stats) = render3d::render(
                black_box(&fixture.scene),
                black_box(&fixture.meshes),
                black_box(&hard),
                TERMINAL_WIDTH,
                TERMINAL_HEIGHT,
            );
            black_box((image.fingerprint(), stats.triangles));
        });
    });
    group.finish();
}

/// The same scene through a wgpu device, measuring only what the CPU does.
fn gpu(criterion: &mut Criterion) {
    let Some(fixture) = Fixture::new() else {
        eprintln!("render_pipeline: skipping the wgpu cases, the shipped meshes did not load");
        return;
    };
    let (Some(full), Some(terminal)) = (
        Gpu::new(&fixture, FULL_WIDTH, FULL_HEIGHT),
        Gpu::new(&fixture, TERMINAL_WIDTH, TERMINAL_HEIGHT),
    ) else {
        eprintln!("render_pipeline: skipping the wgpu cases, no adapter could be opened");
        return;
    };
    let Some(interactive) = budgeted_shot(&fixture) else {
        return;
    };
    terminal.upload_camera(&interactive);

    let instances: usize = full
        .batches
        .iter()
        .map(|batch| batch.instances as usize)
        .sum();
    let mut group = criterion.benchmark_group("wgpu");
    group.throughput(Throughput::Elements(instances as u64));
    group.bench_function("full/600x375", |bencher| {
        bencher.iter(|| {
            full.frame();
            black_box(());
        });
    });
    group.bench_function("interactive/112x72", |bencher| {
        bencher.iter(|| {
            terminal.frame();
            black_box(());
        });
    });
    group.finish();

    // The crowd upload on its own, which is the part of a wgpu frame that scales with population
    // rather than with draw calls: one `write_buffer` of every instance.
    let mut upload = criterion.benchmark_group("wgpu_upload");
    upload.throughput(Throughput::Bytes(full.instance_bytes.len() as u64));
    upload.bench_function(format!("{instances}_instances"), |bencher| {
        bencher.iter(|| {
            full.queue
                .write_buffer(&full.instances, 0, &full.instance_bytes);
            black_box(());
        });
    });
    upload.finish();
}

criterion_group!(benches, software, gpu);
criterion_main!(benches);
