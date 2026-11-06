//! A GPU render of the Ashfall Verge playground, and the harness that costs it.
//!
//! The software rasteriser in `bevy-flock-demo` exists so the playground runs with no graphics stack
//! at all. This crate is the other half of the comparison: the same scene, the same camera, the same
//! instance list and the same [`Shot`] table, drawn by an instanced pipeline on whatever `wgpu`
//! backend the machine has.
//!
//! - **A demo that uses the GPU.** [`GpuRenderer::open`] takes the scene and the loaded meshes;
//!   [`GpuRenderer::set_shot`] selects the bodies, the ruins and the ground a shot can see, and
//!   [`GpuRenderer::read_rgba`] hands the frame back as pixels. `main.rs` writes them as a PNG.
//! - **A benchmark that separates the halves of a frame.** [`GpuRenderer::draw`] is the CPU half —
//!   encode and submit — and [`GpuRenderer::wait`] is the GPU half. One number that adds them
//!   measures neither, so `benches/frame.rs` reports them apart.
//! - **A backend that always works.** [`Backend::Noop`] validates and encodes exactly as the real
//!   backends do and throws the commands away, so the encode path is testable in CI on a machine with
//!   no driver.
//!
//! # What this crate does not claim
//!
//! On a machine with no GPU, the Vulkan driver is usually `lavapipe`, a CPU implementation. Frames it
//! renders are correct and its timings are real, but they measure *that* driver: they say nothing
//! about a discrete GPU beyond the shape of the cost. Every number this crate produces records the
//! adapter it came from, through [`Adapter::label`], because a figure without its adapter is a figure
//! without meaning.

use bevy_flock::domain::num;
use bevy_flock_demo::gltf::Mesh;
use bevy_flock_demo::render3d::{Meshes, Shot};
use bevy_flock_demo::scene::{Instance, Scene};
use bevy_math::Vec3;
use bytemuck::{Pod, Zeroable};
use std::collections::BTreeMap;

/// The radiance divisor, chosen so a sun-lit surface lands near the software renderer's exposure.
const EXPOSURE: f32 = 0.34;

/// The texture format the offscreen target uses: sRGB, so the shader works in linear light and the
/// hardware encodes, exactly as a window's surface would.
const TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// `wgpu`'s texture copy requires rows to be a multiple of the copy alignment.
const COPY_ALIGN: u32 = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;

/// The slug the generated cube carries, so a batch keyed by slug can never collide with a model.
const CUBE: &str = "\u{0}block";

/// The slug the ground plane carries.
const GROUND: &str = "\u{0}ground";

/// Which graphics backend to open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Backend {
    /// Vulkan, then GL, then the noop device: whatever this machine can actually do.
    #[default]
    Auto,
    /// Vulkan only. `lavapipe` counts as Vulkan, and is what a machine with no GPU will give you.
    Vulkan,
    /// OpenGL ES only.
    Gl,
    /// The noop device: encodes and validates, draws nothing, needs no driver.
    Noop,
}

impl Backend {
    /// Parse a command-line name.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "auto" => Some(Self::Auto),
            "vulkan" => Some(Self::Vulkan),
            "gl" | "gles" => Some(Self::Gl),
            "noop" => Some(Self::Noop),
            _ => None,
        }
    }

    /// The name to print and record.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Vulkan => "vulkan",
            Self::Gl => "gl",
            Self::Noop => "noop",
        }
    }

    /// The backends to ask `wgpu` for.
    const fn mask(self) -> wgpu::Backends {
        match self {
            Self::Auto => wgpu::Backends::VULKAN
                .union(wgpu::Backends::GL)
                .union(wgpu::Backends::NOOP),
            Self::Vulkan => wgpu::Backends::VULKAN,
            Self::Gl => wgpu::Backends::GL,
            Self::Noop => wgpu::Backends::NOOP,
        }
    }

    /// The backend options that make a noop device possible, and none otherwise.
    fn options(self) -> wgpu::BackendOptions {
        let mut options = wgpu::BackendOptions::default();
        if matches!(self, Self::Auto | Self::Noop) {
            options.noop = wgpu::NoopBackendOptions::enabled();
        }
        options
    }
}

/// What can go wrong opening a device or reading a frame back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GpuError {
    /// No adapter matched the requested backend.
    NoAdapter(&'static str),
    /// The adapter would not give a device.
    NoDevice(String),
    /// Mapping the readback buffer failed, which on a real driver means the device was lost.
    Readback(String),
    /// A model in the scene is not in the mesh table, so its batch cannot be built.
    MissingModel(&'static str),
    /// The shot selected nothing at all, so there is nothing to draw.
    EmptyView,
}

impl std::fmt::Display for GpuError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoAdapter(backend) => write!(formatter, "no {backend} adapter on this machine"),
            Self::NoDevice(why) => write!(formatter, "the adapter refused a device: {why}"),
            Self::Readback(why) => write!(formatter, "the readback failed: {why}"),
            Self::MissingModel(slug) => write!(formatter, "no loaded mesh for model `{slug}`"),
            Self::EmptyView => write!(formatter, "the shot selected no geometry"),
        }
    }
}

impl std::error::Error for GpuError {}

/// One instance as the GPU sees it: a model matrix and a tint.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
struct InstanceRaw {
    /// Column-major model matrix, metres.
    model: [[f32; 4]; 4],
    /// The tint multiplied into the material.
    tint: [f32; 4],
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

/// The uniform block a frame reads: camera, light, fog.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
struct CameraUniform {
    /// World to clip, column-major.
    view_projection: [[f32; 4]; 4],
    /// Eye position, `w` unused.
    eye: [f32; 4],
    /// Direction **towards** the sun, `w` unused.
    sun: [f32; 4],
    /// Sun colour, `w` unused.
    sun_colour: [f32; 4],
    /// Sky ambient, `w` unused.
    sky: [f32; 4],
    /// Ground bounce ambient, `w` unused.
    bounce: [f32; 4],
    /// Fog start and end in `x` and `y`.
    fog: [f32; 4],
    /// Fog colour, `w` unused.
    fog_colour: [f32; 4],
    /// Exposure in `x`; the rest is padding.
    exposure: [f32; 4],
}

/// A candidate instance, kept so a shot can select from it without rebuilding the scene.
struct Row {
    /// Which batch it belongs to.
    slug: &'static str,
    /// Its transform and tint.
    raw: InstanceRaw,
    /// Where it stands, for the distance test.
    position: Vec3,
}

/// One model's uploaded geometry.
struct Geometry {
    /// Vertex buffer.
    vertices: wgpu::Buffer,
    /// Index buffer.
    indices: wgpu::Buffer,
    /// Indices per instance.
    count: u32,
}

/// One draw: which geometry, and which slice of the instance buffer it uses.
struct Batch {
    /// The model's uploaded geometry.
    index: usize,
    /// The first instance this batch owns.
    first: u32,
    /// How many instances it draws.
    instances: u32,
}

/// What a frame drew.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    /// Instances submitted.
    pub instances: u32,
    /// Draw calls.
    pub draws: u32,
    /// Triangles, counting instancing.
    pub triangles: u64,
}

/// The adapter this renderer opened, recorded so every timing can name it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Adapter {
    /// Backend name, as `wgpu` spells it: `Vulkan`, `Gl`, `Noop`.
    pub backend: String,
    /// Device name: `llvmpipe (LLVM 20.1.2, 128 bits)` on a machine with no GPU.
    pub name: String,
    /// Driver name.
    pub driver: String,
    /// Driver version or extra information.
    pub driver_info: String,
    /// Whether the device is a CPU implementation.
    pub cpu: bool,
}

impl Adapter {
    /// One line for a log or a benchmark label.
    #[must_use]
    pub fn label(&self) -> String {
        format!("{} | {} | {}", self.backend, self.name, self.driver)
    }

    /// Whether this adapter is a software implementation, which every number must record.
    #[must_use]
    pub const fn is_software(&self) -> bool {
        self.cpu
    }
}

/// An offscreen GPU renderer for one scene.
pub struct GpuRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    uniform: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    target: wgpu::Texture,
    view: wgpu::TextureView,
    depth: wgpu::TextureView,
    instances: wgpu::Buffer,
    instance_bytes: Vec<u8>,
    rows: Vec<Row>,
    geometry: Vec<Geometry>,
    slug_index: BTreeMap<&'static str, usize>,
    batches: Vec<Batch>,
    adapter: Adapter,
    clear: [f32; 3],
    width: u32,
    height: u32,
}

impl GpuRenderer {
    /// Open a device, upload every model the scene can name, and build the pipeline.
    ///
    /// # Errors
    ///
    /// [`GpuError::NoAdapter`] when the machine has nothing matching `backend`,
    /// [`GpuError::NoDevice`] when the adapter refuses a device, and [`GpuError::MissingModel`] when
    /// the scene names a model the mesh table does not hold. A renderer that skipped a missing model
    /// would draw a hole and call it a picture.
    #[expect(
        clippy::too_many_lines,
        reason = "a wgpu pipeline is a linear description; splitting it hides the required order"
    )]
    pub fn open(
        scene: &Scene,
        meshes: &Meshes,
        backend: Backend,
        width: u32,
        height: u32,
    ) -> Result<Self, GpuError> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: backend.mask(),
            flags: wgpu::InstanceFlags::empty(),
            memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
            backend_options: backend.options(),
            display: None,
        });
        let wgpu_adapter = futures_lite::future::block_on(
            instance.request_adapter(&wgpu::RequestAdapterOptions::default()),
        )
        .map_err(|_| GpuError::NoAdapter(backend.name()))?;
        let info = wgpu_adapter.get_info();
        let (device, queue) = request_device(&wgpu_adapter)?;
        let adapter = Adapter {
            backend: format!("{:?}", info.backend),
            name: info.name,
            driver: info.driver,
            driver_info: info.driver_info,
            cpu: matches!(info.device_type, wgpu::DeviceType::Cpu),
        };

        let mut geometry = Vec::new();
        let mut by_slug: BTreeMap<&'static str, usize> = BTreeMap::new();
        for slug in scene_slugs(scene) {
            if by_slug.contains_key(slug) {
                continue;
            }
            let Some(mesh) = meshes.get(slug) else {
                return Err(GpuError::MissingModel(slug));
            };
            by_slug.insert(slug, geometry.len());
            geometry.push(upload_mesh(&device, &queue, mesh, slug));
        }
        // Two generated meshes, so the ruins and the ground are drawn by the same pipeline as the
        // bodies rather than by a second pass with its own shader.
        by_slug.insert(CUBE, geometry.len());
        geometry.push(upload_mesh(&device, &queue, &cube(), "block"));
        by_slug.insert(GROUND, geometry.len());
        geometry.push(upload_mesh(&device, &queue, &plane(), "ground"));

        let rows = scene_rows(scene, &by_slug)?;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("ashfall"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ashfall"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
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
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("camera"),
            size: std::mem::size_of::<CameraUniform>() as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        // Sized for every row once, so a shot never reallocates: the crowd changes which instances
        // it draws, never how many exist.
        let instances = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("instances"),
            size: (rows.len().max(1) * std::mem::size_of::<InstanceRaw>()) as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ashfall"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: instances.as_entire_binding(),
                },
            ],
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
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                // The models are closed and the ground is a single quad; culling would need the
                // winding of six Meshy exports and a generated plane to agree, which they do not.
                cull_mode: None,
                ..wgpu::PrimitiveState::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: TARGET_FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

        let extent = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("target"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: TARGET_FORMAT,
            // COPY_SRC is what makes the frame readable; a windowed front end would not need it.
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());
        let depth = device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("depth"),
                size: extent,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Depth32Float,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            })
            .create_view(&wgpu::TextureViewDescriptor::default());

        Ok(Self {
            device,
            queue,
            pipeline,
            uniform,
            bind_group,
            target,
            view,
            depth,
            instances,
            instance_bytes: Vec::new(),
            rows,
            geometry,
            slug_index: by_slug,
            batches: Vec::new(),
            adapter,
            clear: [0.0; 3],
            width,
            height,
        })
    }

    /// The adapter that was opened.
    #[must_use]
    pub const fn adapter(&self) -> &Adapter {
        &self.adapter
    }

    /// The texture's size in pixels.
    #[must_use]
    pub const fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Candidates the scene offered, before any shot selected from them.
    #[must_use]
    pub const fn candidates(&self) -> usize {
        self.rows.len()
    }

    /// Instances the last [`Self::set_shot`] selected.
    #[must_use]
    pub fn instance_count(&self) -> u32 {
        self.batches.iter().map(|batch| batch.instances).sum()
    }

    /// Write the camera and the light for `shot`, select what it can see, and upload the instances.
    ///
    /// Selection mirrors the software renderer's: bodies beyond the draw distance, or nearer than the
    /// body margin, are dropped, and the nearest `max_bodies` survive. Ruins are selected by the
    /// block distance and the ground is always drawn, because a horizon without a floor is a hole.
    ///
    /// Selection is a sort by model and then by distance, so the instance buffer stays contiguous per
    /// batch and the copy is one `write_buffer` per shot rather than one per body.
    pub fn set_shot(&mut self, shot: &Shot) {
        let aspect = pixels(self.width) / pixels(self.height);
        let uniform = CameraUniform {
            view_projection: shot.camera.view_projection(aspect).to_cols_array_2d(),
            eye: shot.camera.eye.extend(0.0).to_array(),
            sun: rgba_vec(shot.atmosphere.sun.normalize_or_zero()),
            sun_colour: rgba(&shot.atmosphere.sun_colour),
            sky: rgba(&shot.atmosphere.sky),
            bounce: rgba(&shot.atmosphere.bounce),
            fog: [shot.atmosphere.fog_near, shot.atmosphere.fog_far, 0.0, 0.0],
            fog_colour: rgba(&shot.atmosphere.fog),
            exposure: [EXPOSURE, 0.0, 0.0, 0.0],
        };
        self.queue
            .write_buffer(&self.uniform, 0, bytemuck::bytes_of(&uniform));
        // The horizon is what fog resolves to, so clearing to it makes the far plane disappear.
        self.clear = shot.atmosphere.fog;

        let eye = shot.camera.eye;
        // Three groups, three rules. `max_bodies` is a cap on the crowd; applying it to the whole
        // selection dropped every ruin, because 420 bodies are always nearer than the first block.
        let mut bodies: Vec<(usize, f32)> = Vec::new();
        let mut ruins: Vec<(usize, f32)> = Vec::new();
        let mut ground: Option<usize> = None;
        for (index, row) in self.rows.iter().enumerate() {
            let distance = row.position.distance(eye);
            match row.slug {
                GROUND => ground = Some(index),
                CUBE => {
                    if distance <= shot.block_distance {
                        ruins.push((index, distance));
                    }
                }
                _ => {
                    if distance <= shot.draw_distance && distance >= shot.body_margin {
                        bodies.push((index, distance));
                    }
                }
            }
        }
        // Nearest first, and the tie-break is the row index, so the same shot selects the same bodies
        // in the same order every time.
        bodies.sort_by(|left, right| {
            left.1
                .total_cmp(&right.1)
                .then_with(|| left.0.cmp(&right.0))
        });
        bodies.truncate(shot.max_bodies);
        let mut selected = bodies;
        selected.append(&mut ruins);
        if let Some(index) = ground {
            selected.push((index, 0.0));
        }
        // Group by model so each batch is one contiguous slice of the instance buffer.
        selected.sort_by(|left, right| {
            let left_slug = self.rows.get(left.0).map_or("", |row| row.slug);
            let right_slug = self.rows.get(right.0).map_or("", |row| row.slug);
            left_slug
                .cmp(right_slug)
                .then_with(|| left.1.total_cmp(&right.1))
                .then_with(|| left.0.cmp(&right.0))
        });

        self.instance_bytes.clear();
        self.batches.clear();
        let mut first = 0_u32;
        // One pass, flushing a batch each time the model changes. The pattern binds the *previous*
        // batch's geometry under its own name: binding it as `geometry` shadowed the row's own index
        // and every batch inherited the ground's buffer, which drew a city out of six planes.
        let mut current: Option<(&'static str, usize, u32)> = None;
        for (index, _) in &selected {
            let Some(row) = self.rows.get(*index) else {
                continue;
            };
            let Some(row_geometry) = self.geometry_index(row.slug) else {
                continue;
            };
            self.instance_bytes
                .extend_from_slice(bytemuck::bytes_of(&row.raw));
            match current {
                Some((slug, previous, instances)) if slug == row.slug => {
                    current = Some((slug, previous, instances + 1));
                }
                Some((_, previous, instances)) => {
                    self.batches.push(Batch {
                        index: previous,
                        first,
                        instances,
                    });
                    first += instances;
                    current = Some((row.slug, row_geometry, 1));
                }
                None => current = Some((row.slug, row_geometry, 1)),
            }
        }
        if let Some((_, previous, instances)) = current {
            self.batches.push(Batch {
                index: previous,
                first,
                instances,
            });
        }
        if !self.instance_bytes.is_empty() {
            self.queue
                .write_buffer(&self.instances, 0, &self.instance_bytes);
        }
    }

    /// The uploaded geometry index for a slug, if the scene built one.
    fn geometry_index(&self, slug: &'static str) -> Option<usize> {
        self.slug_index.get(slug).copied()
    }

    /// Encode one frame and submit it. This is the CPU half; it does not wait for the GPU.
    pub fn draw(&self) {
        if self.batches.is_empty() {
            return;
        }
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame"),
            });
        {
            // `wgpu`'s clear value is `f64`; widening from `f32` is exact and needs no cast.
            let [r, g, b] = self.clear.map(f64::from);
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("frame"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color { r, g, b, a: 1.0 }),
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
            for batch in &self.batches {
                let Some(geometry) = self.geometry.get(batch.index) else {
                    continue;
                };
                pass.set_vertex_buffer(0, geometry.vertices.slice(..));
                pass.set_index_buffer(geometry.indices.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(
                    0..geometry.count,
                    0,
                    batch.first..batch.first + batch.instances,
                );
            }
        }
        self.queue.submit([encoder.finish()]);
    }

    /// Wait for the GPU to finish everything submitted. This is the other half of a frame.
    pub fn wait(&self) {
        let _ = self.device.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        });
    }

    /// What [`Self::draw`] submits.
    #[must_use]
    pub fn stats(&self) -> Stats {
        Stats {
            instances: self.instance_count(),
            draws: u32::try_from(self.batches.len()).unwrap_or(u32::MAX),
            triangles: self
                .batches
                .iter()
                .filter_map(|batch| {
                    self.geometry
                        .get(batch.index)
                        .map(|geometry| (batch, geometry))
                })
                .map(|(batch, geometry)| u64::from(geometry.count / 3) * u64::from(batch.instances))
                .sum(),
        }
    }

    /// Read the frame back as tightly packed RGBA, top row first.
    ///
    /// # Errors
    ///
    /// [`GpuError::Readback`] when the map fails or a row falls outside the mapped range.
    pub fn read_rgba(&self) -> Result<Vec<u8>, GpuError> {
        let unpadded = self.width * 4;
        let row_bytes = unpadded.div_ceil(COPY_ALIGN) * COPY_ALIGN;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: u64::from(row_bytes) * u64::from(self.height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("readback"),
            });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row_bytes),
                    rows_per_image: Some(self.height),
                },
            },
            wgpu::Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit([encoder.finish()]);

        let (sender, receiver) = std::sync::mpsc::channel();
        buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = sender.send(result);
            });
        self.wait();
        match receiver.recv() {
            Ok(Ok(())) => {}
            Ok(Err(error)) => return Err(GpuError::Readback(format!("{error}"))),
            Err(error) => return Err(GpuError::Readback(format!("{error}"))),
        }
        let mapped = buffer
            .slice(..)
            .get_mapped_range()
            .map_err(|error| GpuError::Readback(format!("{error}")))?;
        let out = pack_rows(&mapped, self.width, self.height)?;
        drop(mapped);
        buffer.unmap();
        Ok(out)
    }

    /// The colour the frame clears to, which is also what "a pixel that was drawn" is measured
    /// against by a caller checking the frame is not empty.
    #[must_use]
    pub fn clear_colour(&self) -> [u8; 4] {
        [
            linear_to_srgb(self.clear[0]),
            linear_to_srgb(self.clear[1]),
            linear_to_srgb(self.clear[2]),
            255,
        ]
    }
}

/// Copy a mapped texture readback into tightly packed rows.
///
/// `wgpu` pads every copied row to [`COPY_ALIGN`], so the mapped buffer is wider than the frame: a
/// 3-pixel-wide frame occupies 12 bytes of a 256-byte row. Dropping the padding is arithmetic that
/// is easy to get subtly wrong and impossible to see in a screenshot, so it is a function of its own
/// with tests rather than four lines inside a method that needs a device to run.
///
/// # Errors
///
/// [`GpuError::Readback`] when a row falls outside the mapped buffer, which means the copy and the
/// request disagreed about the size.
fn pack_rows(mapped: &[u8], width: u32, height: u32) -> Result<Vec<u8>, GpuError> {
    let unpadded = width * 4;
    let row_bytes = unpadded.div_ceil(COPY_ALIGN) * COPY_ALIGN;
    let capacity = usize::try_from(unpadded)
        .ok()
        .and_then(|row| usize::try_from(height).ok().map(|rows| row * rows))
        .unwrap_or(0);
    let mut out = Vec::with_capacity(capacity);
    for row in 0..height {
        let start = usize::try_from(row * row_bytes).unwrap_or(usize::MAX);
        let end = start.saturating_add(usize::try_from(unpadded).unwrap_or(usize::MAX));
        match mapped.get(start..end) {
            Some(slice) => out.extend_from_slice(slice),
            None => return Err(GpuError::Readback(format!("row {row} is out of range"))),
        }
    }
    Ok(out)
}

/// Every model slug a scene can name.
fn scene_slugs(scene: &Scene) -> Vec<&'static str> {
    let mut slugs: Vec<&'static str> = Vec::new();
    for instance in scene
        .bodies
        .iter()
        .chain(scene.pylons.iter())
        .chain(scene.shelters.iter())
        .chain(scene.stalls.iter())
    {
        if !slugs.contains(&instance.model) {
            slugs.push(instance.model);
        }
    }
    slugs
}

/// Flatten the scene into rows: bodies and props from their instances, ruins as scaled cubes, and one
/// ground plane.
fn scene_rows(
    scene: &Scene,
    by_slug: &BTreeMap<&'static str, usize>,
) -> Result<Vec<Row>, GpuError> {
    let mut rows = Vec::with_capacity(
        scene.bodies.len() + scene.pylons.len() + scene.shelters.len() + scene.stalls.len() + 1,
    );
    for instance in scene
        .bodies
        .iter()
        .chain(scene.pylons.iter())
        .chain(scene.shelters.iter())
        .chain(scene.stalls.iter())
    {
        if !by_slug.contains_key(instance.model) {
            return Err(GpuError::MissingModel(instance.model));
        }
        rows.push(Row {
            slug: instance.model,
            raw: instance_raw(instance),
            position: instance.position,
        });
    }
    for block in &scene.blocks {
        let scale = Vec3::new(block.size.x, block.size.y, block.size.z);
        // Ruined blocks are drawn lower and darker: the same cube, sunk and tinted, which is what the
        // software renderer does with a vertical jitter.
        // `centre` is at ground level and `size.y` is a half-extent, so the cube's middle is one
        // half-extent up. Ruined blocks sink a little, which is how the layout expresses rubble.
        let lift = block.size.y * 0.3_f32.mul_add(-block.ruin, 0.86);
        let shade = 0.2_f32.mul_add(1.0 - block.ruin, 0.42);
        rows.push(Row {
            slug: CUBE,
            raw: scaled_cube(
                block.centre + Vec3::Y * lift,
                scale,
                [shade, shade * 0.96, shade * 0.9, 1.0],
            ),
            position: block.centre,
        });
    }
    // A 4 km plane under the eye: past the fog, so its edge is never visible.
    rows.push(Row {
        slug: GROUND,
        raw: scaled_cube(
            Vec3::new(0.0, -0.05, 0.0),
            Vec3::new(2000.0, 0.05, 2000.0),
            [0.3, 0.3, 0.33, 1.0],
        ),
        position: Vec3::ZERO,
    });
    Ok(rows)
}

/// The model matrix for one instance: scale, yaw, translate.
fn instance_raw(instance: &Instance) -> InstanceRaw {
    let (sin, cos) = instance.yaw.sin_cos();
    let scale = instance.scale;
    let position = instance.position;
    InstanceRaw {
        model: [
            [cos * scale, 0.0, -sin * scale, 0.0],
            [0.0, scale, 0.0, 0.0],
            [sin * scale, 0.0, cos * scale, 0.0],
            [position.x, position.y, position.z, 1.0],
        ],
        tint: instance.tint,
    }
}

/// A translated, scaled, axis-aligned cube.
const fn scaled_cube(centre: Vec3, half: Vec3, tint: [f32; 4]) -> InstanceRaw {
    InstanceRaw {
        model: [
            [half.x, 0.0, 0.0, 0.0],
            [0.0, half.y, 0.0, 0.0],
            [0.0, 0.0, half.z, 0.0],
            [centre.x, centre.y, centre.z, 1.0],
        ],
        tint,
    }
}

/// A three-channel colour as a padded `vec4`, which is how a uniform block carries it.
fn rgba(colour: &[f32; 3]) -> [f32; 4] {
    [
        colour.first().copied().unwrap_or(0.0),
        colour.get(1).copied().unwrap_or(0.0),
        colour.get(2).copied().unwrap_or(0.0),
        0.0,
    ]
}

/// A direction as a padded `vec4`.
const fn rgba_vec(direction: Vec3) -> [f32; 4] {
    [direction.x, direction.y, direction.z, 0.0]
}

/// A pixel dimension as a float.
///
/// `u16` first, because a texture wider than 65,535 pixels cannot be created anyway and the
/// conversion is then exact: `f32::from` has no lossy cast to argue with.
fn pixels(value: u32) -> f32 {
    f32::from(u16::try_from(value).unwrap_or(u16::MAX))
}

/// A linear channel as sRGB, which is what the target texture holds after the hardware encodes it.
fn linear_to_srgb(value: f32) -> u8 {
    let clamped = value.clamp(0.0, 1.0);
    let encoded = if clamped <= 0.003_130_8 {
        clamped * 12.92
    } else {
        1.055_f32.mul_add(clamped.powf(1.0 / 2.4), -0.055)
    };
    to_byte(encoded * 255.0)
}

/// A rounded, clamped 0-255 value as a byte.
///
/// The rounding happens here; the conversion belongs to `bevy_flock::domain::num`, which is where
/// every cast in this repository lives. It truncates toward zero and saturates, so a negative or
/// enormous input lands on `0` or `255` instead of wrapping, and `NaN` becomes `0`. Rounding first is
/// what makes this a rounding rather than a truncation.
const fn to_byte(value: f32) -> u8 {
    num::f32_to_u8(value.round())
}

/// Ask the adapter for a device with default limits, which is all this pipeline needs.
fn request_device(adapter: &wgpu::Adapter) -> Result<(wgpu::Device, wgpu::Queue), GpuError> {
    futures_lite::future::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
        .map_err(|error| GpuError::NoDevice(format!("{error}")))
}

/// Upload one mesh's vertices and indices.
fn upload_mesh(device: &wgpu::Device, queue: &wgpu::Queue, mesh: &Mesh, label: &str) -> Geometry {
    let vertices: Vec<VertexRaw> = mesh
        .positions
        .iter()
        .zip(mesh.normals.iter())
        .map(|(position, normal)| VertexRaw {
            position: *position,
            normal: *normal,
        })
        .collect();
    let vertex_bytes = bytemuck::cast_slice(&vertices);
    let vertices_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: vertex_bytes.len().max(4) as wgpu::BufferAddress,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    queue.write_buffer(&vertices_buffer, 0, vertex_bytes);
    let index_bytes = bytemuck::cast_slice(&mesh.indices);
    let index_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: index_bytes.len().max(4) as wgpu::BufferAddress,
        usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    queue.write_buffer(&index_buffer, 0, index_bytes);
    Geometry {
        vertices: vertices_buffer,
        indices: index_buffer,
        count: u32::try_from(mesh.indices.len()).unwrap_or(u32::MAX),
    }
}

/// A unit cube, centred on its origin, with flat normals and counter-clockwise winding from outside.
fn cube() -> Mesh {
    let mut positions = Vec::with_capacity(24);
    let mut normals = Vec::with_capacity(24);
    let mut indices = Vec::with_capacity(36);
    let faces = [
        (
            [0.0, 0.0, 1.0],
            [
                [-1.0, -1.0, 1.0],
                [1.0, -1.0, 1.0],
                [1.0, 1.0, 1.0],
                [-1.0, 1.0, 1.0],
            ],
        ),
        (
            [0.0, 0.0, -1.0],
            [
                [1.0, -1.0, -1.0],
                [-1.0, -1.0, -1.0],
                [-1.0, 1.0, -1.0],
                [1.0, 1.0, -1.0],
            ],
        ),
        (
            [1.0, 0.0, 0.0],
            [
                [1.0, -1.0, 1.0],
                [1.0, -1.0, -1.0],
                [1.0, 1.0, -1.0],
                [1.0, 1.0, 1.0],
            ],
        ),
        (
            [-1.0, 0.0, 0.0],
            [
                [-1.0, -1.0, -1.0],
                [-1.0, -1.0, 1.0],
                [-1.0, 1.0, 1.0],
                [-1.0, 1.0, -1.0],
            ],
        ),
        (
            [0.0, 1.0, 0.0],
            [
                [-1.0, 1.0, 1.0],
                [1.0, 1.0, 1.0],
                [1.0, 1.0, -1.0],
                [-1.0, 1.0, -1.0],
            ],
        ),
        (
            [0.0, -1.0, 0.0],
            [
                [-1.0, -1.0, -1.0],
                [1.0, -1.0, -1.0],
                [1.0, -1.0, 1.0],
                [-1.0, -1.0, 1.0],
            ],
        ),
    ];
    for (normal, corners) in faces {
        let base = u32::try_from(positions.len()).unwrap_or(0);
        for corner in corners {
            positions.push(corner);
            normals.push(normal);
        }
        indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    Mesh {
        positions,
        normals,
        indices,
        base_colour: [0.5, 0.5, 0.5],
        textured: false,
        name: "block".to_string(),
    }
}

/// A unit square on the XZ plane, two triangles, facing up.
fn plane() -> Mesh {
    Mesh {
        positions: vec![
            [-1.0, 0.0, -1.0],
            [1.0, 0.0, -1.0],
            [1.0, 0.0, 1.0],
            [-1.0, 0.0, 1.0],
        ],
        normals: vec![[0.0, 1.0, 0.0]; 4],
        indices: vec![0, 2, 1, 0, 3, 2],
        base_colour: [0.4, 0.38, 0.34],
        textured: false,
        name: "ground".to_string(),
    }
}

/// The WGSL the pipeline runs: an instanced draw with one light, hemisphere ambient and distance fog.
const SHADER: &str = r"
struct Camera {
    view_projection: mat4x4<f32>,
    eye: vec4<f32>,
    sun: vec4<f32>,
    sun_colour: vec4<f32>,
    sky: vec4<f32>,
    bounce: vec4<f32>,
    fog: vec4<f32>,
    fog_colour: vec4<f32>,
    exposure: vec4<f32>,
};

struct Instance {
    model: mat4x4<f32>,
    tint: vec4<f32>,
};

@group(0) @binding(0) var<uniform> camera: Camera;
@group(0) @binding(1) var<storage, read> instances: array<Instance>;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) tint: vec3<f32>,
};

@vertex
fn vs(
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @builtin(instance_index) index: u32,
) -> VsOut {
    let instance = instances[index];
    let world = instance.model * vec4<f32>(position, 1.0);
    var out: VsOut;
    out.clip = camera.view_projection * world;
    out.world = world.xyz;
    out.normal = (instance.model * vec4<f32>(normal, 0.0)).xyz;
    out.tint = instance.tint.rgb;
    return out;
}

@fragment
fn fs(input: VsOut) -> @location(0) vec4<f32> {
    let normal = normalize(input.normal);
    let sun = normalize(camera.sun.xyz);
    let lambert = max(dot(normal, sun), 0.0);
    let hemisphere = mix(camera.bounce.rgb, camera.sky.rgb, 0.5 + 0.5 * normal.y);
    let radiance = input.tint * (camera.sun_colour.rgb * lambert + hemisphere);
    let distance = length(input.world - camera.eye.xyz);
    let span = max(camera.fog.y - camera.fog.x, 1.0);
    let fog = clamp((distance - camera.fog.x) / span, 0.0, 1.0);
    // The atmosphere's radiance is authored for the software renderer's exposure; matching it keeps
    // the two images in the same family instead of clipping one of them to white.
    let lit = radiance * camera.exposure.x;
    return vec4<f32>(mix(lit, camera.fog_colour.rgb, fog), 1.0);
}
";

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_flock::testkit::{OrDie, OrDieErr};

    /// An instance at the origin facing north, for tests that only vary one field.
    const fn instance(yaw: f32, scale: f32, position: Vec3) -> Instance {
        Instance {
            model: "block",
            position,
            yaw,
            scale,
            tint: [1.0, 1.0, 1.0, 1.0],
        }
    }

    /// Every backend's printed name parses back to itself, because the name is also the flag value.
    #[test]
    fn backend_names_round_trip_through_parse() {
        for backend in [Backend::Auto, Backend::Vulkan, Backend::Gl, Backend::Noop] {
            assert_eq!(Backend::parse(backend.name()), Some(backend));
        }
    }

    /// `gles` is what `wgpu` calls the backend and `gl` is what a person types; both mean OpenGL.
    #[test]
    fn backend_parse_accepts_both_spellings_of_opengl_and_rejects_nonsense() {
        assert_eq!(Backend::parse("gl"), Some(Backend::Gl));
        assert_eq!(Backend::parse("gles"), Some(Backend::Gl));
        assert_eq!(Backend::parse("metal"), None);
        assert_eq!(Backend::parse(""), None);
    }

    /// The default is the one that opens whatever the machine has, not a backend it may not.
    #[test]
    fn the_default_backend_is_auto() {
        assert_eq!(Backend::default(), Backend::Auto);
    }

    /// Rounding is half away from zero and the conversion saturates, so nothing wraps.
    #[test]
    fn to_byte_rounds_and_saturates() {
        assert_eq!(to_byte(0.0), 0);
        assert_eq!(to_byte(0.4), 0);
        assert_eq!(to_byte(0.5), 1);
        assert_eq!(to_byte(1.5), 2);
        assert_eq!(to_byte(254.4), 254);
        assert_eq!(to_byte(254.6), 255);
        assert_eq!(to_byte(-3.0), 0);
        assert_eq!(to_byte(1.0e9), 255);
        assert_eq!(to_byte(f32::NAN), 0);
    }

    /// The transfer function hits both ends exactly and matches the published value at mid grey.
    ///
    /// `linear_to_srgb` is what `clear_colour` compares a frame against, so an error here makes every
    /// "was anything drawn" check wrong in the same direction.
    #[test]
    fn linear_to_srgb_hits_the_endpoints_and_the_midpoint() {
        assert_eq!(linear_to_srgb(0.0), 0);
        assert_eq!(linear_to_srgb(1.0), 255);
        // 0.5^(1/2.4) = 0.7492, and 1.055 * 0.7492 - 0.055 = 0.7354, which is 188/255.
        assert_eq!(linear_to_srgb(0.5), 188);
        // Out of range clamps rather than wrapping.
        assert_eq!(linear_to_srgb(-1.0), 0);
        assert_eq!(linear_to_srgb(4.0), 255);
    }

    /// The transfer function is monotone, which is what makes a dithered frame readable at all.
    #[test]
    fn linear_to_srgb_is_monotone() {
        let mut previous = 0;
        for step in 0..=64 {
            let value = f32::from(u8::try_from(step).or_die()) / 64.0;
            let encoded = linear_to_srgb(value);
            assert!(
                encoded >= previous,
                "encoding fell from {previous} to {encoded} at {value}"
            );
            previous = encoded;
        }
    }

    /// A yaw of a quarter turn swaps the two horizontal axes and the scale reaches both.
    #[test]
    fn instance_raw_rotates_scales_and_translates() {
        let raw = instance_raw(&instance(
            std::f32::consts::FRAC_PI_2,
            2.0,
            Vec3::new(1.0, 2.0, 3.0),
        ));
        // Column 0 is the rotated x axis: (cos, 0, -sin) * scale = (0, 0, -2) at a quarter turn.
        assert!((raw.model[0][0]).abs() < 1.0e-6);
        assert!((raw.model[0][2] + 2.0).abs() < 1.0e-6);
        // Column 2 is the rotated z axis: (sin, 0, cos) * scale = (2, 0, 0).
        assert!((raw.model[2][0] - 2.0).abs() < 1.0e-6);
        // Column 3 is the translation, with the homogeneous 1 in `w`.
        assert!((raw.model[3][0] - 1.0).abs() < 1.0e-6);
        assert!((raw.model[3][1] - 2.0).abs() < 1.0e-6);
        assert!((raw.model[3][2] - 3.0).abs() < 1.0e-6);
        assert!((raw.model[3][3] - 1.0).abs() < 1.0e-6);
    }

    /// A zero yaw and unit scale is the identity, so an instance with no rotation draws where it says.
    #[test]
    fn instance_raw_at_rest_is_a_translation() {
        let raw = instance_raw(&instance(0.0, 1.0, Vec3::new(-4.0, 0.5, 7.0)));
        let identity = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [-4.0, 0.5, 7.0, 1.0],
        ];
        for (column, expected) in raw.model.iter().zip(identity) {
            for (value, want) in column.iter().zip(expected) {
                assert!((value - want).abs() < 1.0e-6, "expected {expected:?}");
            }
        }
    }

    /// A scaled cube carries its half extent in the diagonal, which is what `scaled_cube` is for.
    #[test]
    fn scaled_cube_is_half_extent_and_centre() {
        let raw = scaled_cube(
            Vec3::new(3.0, 4.0, 5.0),
            Vec3::new(2.0, 6.0, 8.0),
            [0.1, 0.2, 0.3, 1.0],
        );
        assert!((raw.model[0][0] - 2.0).abs() < 1.0e-6);
        assert!((raw.model[1][1] - 6.0).abs() < 1.0e-6);
        assert!((raw.model[2][2] - 8.0).abs() < 1.0e-6);
        assert!((raw.model[3][0] - 3.0).abs() < 1.0e-6);
        assert!((raw.model[3][1] - 4.0).abs() < 1.0e-6);
        assert!((raw.model[3][2] - 5.0).abs() < 1.0e-6);
    }

    /// The vertex layout in the pipeline is written as a stride and two offsets, so the struct the
    /// bytes come from has to keep the size the layout promises. A field added here without a change
    /// there silently reads the next vertex's position as this one's normal.
    #[test]
    fn the_packed_structs_keep_the_strides_the_pipeline_declares() {
        assert_eq!(size_of::<VertexRaw>(), 24);
        assert_eq!(size_of::<InstanceRaw>(), 80);
        assert_eq!(size_of::<CameraUniform>(), 192);
    }

    /// A row of 3 pixels is 12 bytes in a 256-byte stride, and the copy has to drop the other 244.
    #[test]
    fn pack_rows_strips_the_row_padding() {
        let (width, height) = (3_u32, 2_u32);
        let row_bytes = usize::try_from(COPY_ALIGN).or_die();
        let mut mapped = vec![0xEE_u8; row_bytes * 2];
        // First row: 12 bytes of 0x01, second: 12 bytes of 0x02.
        for (index, byte) in mapped.iter_mut().take(12).enumerate() {
            *byte = u8::try_from(index).or_die();
        }
        for (index, byte) in mapped.iter_mut().skip(row_bytes).take(12).enumerate() {
            *byte = u8::try_from(index).or_die() + 100;
        }
        let packed = pack_rows(&mapped, width, height).or_die();
        assert_eq!(packed.len(), 24);
        let expected: Vec<u8> = (0..12_u8).chain(100..112_u8).collect();
        assert_eq!(packed, expected);
    }

    /// A buffer that is short by a row is an error rather than a frame with a hole in it.
    #[test]
    fn pack_rows_reports_a_row_outside_the_buffer() {
        let row_bytes = usize::try_from(COPY_ALIGN).or_die();
        let short = vec![0_u8; row_bytes];
        let error = pack_rows(&short, 1, 2).or_die_err();
        assert!(matches!(error, GpuError::Readback(_)), "got {error:?}");
    }

    /// A frame narrower than the copy alignment still packs one row, which is the off-by-one that
    /// `div_ceil` exists to avoid.
    #[test]
    fn pack_rows_handles_a_frame_narrower_than_the_alignment() {
        let row_bytes = usize::try_from(COPY_ALIGN).or_die();
        let mut mapped = vec![0_u8; row_bytes];
        for (index, byte) in mapped.iter_mut().take(4).enumerate() {
            *byte = u8::try_from(index).or_die();
        }
        let packed = pack_rows(&mapped, 1, 1).or_die();
        assert_eq!(packed, vec![0, 1, 2, 3]);
    }

    /// The cube is twelve triangles, six faces, and each face's four corners share its normal.
    #[test]
    fn the_cube_is_six_flat_faces() {
        let mesh = cube();
        assert_eq!(mesh.positions.len(), 24);
        assert_eq!(mesh.normals.len(), 24);
        assert_eq!(mesh.indices.len(), 36);
        for face in 0..6 {
            let base = face * 4;
            let normal = mesh.normals.get(base).copied().or_die();
            for corner in 0..4 {
                let other = mesh.normals.get(base + corner).copied().or_die();
                assert_eq!(
                    normal.map(f32::to_bits),
                    other.map(f32::to_bits),
                    "face {face} corner {corner} is not flat"
                );
            }
            // A unit cube's corners are all at distance sqrt(3) from its centre.
            for corner in 0..4 {
                let position = mesh.positions.get(base + corner).copied().or_die();
                let length = Vec3::from_array(position).length();
                assert!((length - 3.0_f32.sqrt()).abs() < 1.0e-6);
            }
        }
    }

    /// Every triangle's winding agrees with the normal it carries, which is what back-face culling
    /// depends on: a flipped pair of indices draws nothing and looks like a modelling bug.
    #[test]
    fn every_triangle_winds_with_its_normal() {
        for mesh in [cube(), plane()] {
            for triangle in mesh.indices.chunks_exact(3) {
                let [a, b, c] = <[u32; 3]>::try_from(triangle).or_die();
                let position = |index: u32| {
                    Vec3::from_array(
                        mesh.positions
                            .get(usize::try_from(index).or_die())
                            .copied()
                            .or_die(),
                    )
                };
                let normal = Vec3::from_array(
                    mesh.normals
                        .get(usize::try_from(a).or_die())
                        .copied()
                        .or_die(),
                );
                let geometric = (position(b) - position(a)).cross(position(c) - position(a));
                let agreement = geometric.normalize().dot(normal);
                assert!(
                    agreement > 0.9,
                    "{}: a triangle's winding disagrees with its normal ({agreement})",
                    mesh.name
                );
            }
        }
    }

    /// The ground is four corners and two triangles, all facing up.
    #[test]
    fn the_plane_is_two_triangles_facing_up() {
        let mesh = plane();
        assert_eq!(mesh.positions.len(), 4);
        assert_eq!(mesh.indices.len(), 6);
        for normal in &mesh.normals {
            assert_eq!(
                normal.map(f32::to_bits),
                [0.0_f32.to_bits(), 1.0_f32.to_bits(), 0.0_f32.to_bits()],
                "the ground is not facing up"
            );
        }
    }

    /// A view that selects nothing is an error, not a black frame that looks like a driver problem.
    #[test]
    fn the_empty_view_error_says_what_happened() {
        let message = GpuError::EmptyView.to_string();
        assert!(message.contains("no geometry"), "got {message}");
    }
}
