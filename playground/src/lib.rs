//! A windowed 3D Ashfall playground: the crowd and the city, drawn by Bevy's PBR pipeline.
//!
//! `demo-gpu` renders the same scene offscreen with a hand-written wgpu pipeline, which is what
//! makes its numbers comparable across machines and its images reproducible. This crate answers a
//! different question: what does the crowd look like standing in it, with shadows, a real light and
//! a camera you can walk? It draws through Bevy, so the GPU path is whatever the platform has --
//! Metal on macOS, Vulkan elsewhere -- and the scene is not a second city but the one the demo
//! already builds.
//!
//! ```text
//! cargo run --release -p bevy-flock-playground -- --view district
//! ```
//!
//! Controls: `WASD` to move, `Q`/`E` down and up, shift to sprint, hold the right mouse button to
//! look, `Tab` for the next shipped view, `Escape` to quit.
//!
//! # Why subcrates and not `bevy`
//!
//! The umbrella crate re-exports every Bevy subsystem, and this binary draws a city: no audio, no
//! text, no UI, no scenes, no animation player. The subcrates make the dependency graph say so.

pub mod camera;
pub mod scene;

use bevy_a11y::AccessibilityPlugin;
use bevy_app::{App, AppExit, Startup, TaskPoolPlugin, Update};
use bevy_asset::AssetPlugin;
use bevy_camera::{Camera, Camera3d, PerspectiveProjection, Projection};
use bevy_color::Color;
use bevy_core_pipeline::CorePipelinePlugin;
use bevy_diagnostic::FrameCountPlugin;
use bevy_ecs::message::MessageWriter;
use bevy_ecs::prelude::*;
use bevy_flock_demo::app::Playground;
use bevy_flock_demo::render3d::{self, Meshes};
use bevy_flock_demo::scene::Scene;
use bevy_image::ImagePlugin;
use bevy_input::InputPlugin;
use bevy_light::{AmbientLight, DirectionalLight};
use bevy_math::Vec3;
use bevy_pbr::{DistanceFog, FogFalloff, PbrPlugin};
use bevy_render::RenderPlugin;
use bevy_render::settings::{RenderCreation, WgpuSettings};
use bevy_time::TimePlugin;
use bevy_transform::TransformPlugin;
use bevy_transform::components::Transform;
use bevy_window::{Window, WindowPlugin, WindowResolution};
use bevy_winit::WinitPlugin;
use camera::{BASE_SPEED, FlyCamera, Views};
use scene::AshfallScene;
use std::path::PathBuf;
use wgpu::Backends;

/// The window the playground opens at when nothing says otherwise.
pub const DEFAULT_WIDTH: u32 = 1280;

/// The window's default height, the demo's 16:10.
pub const DEFAULT_HEIGHT: u32 = 800;

/// The widest or tallest window this crate accepts, matching the offscreen crate's arithmetic
/// bound and every current backend's `max_texture_dimension_2d`.
pub const MAX_DIMENSION: u32 = 16_384;

/// The command line.
#[derive(Debug, Clone, PartialEq)]
pub struct Options {
    /// Population to simulate, before the camera culls anything.
    pub population: u32,
    /// Ticks to run before the first frame, so the crowd is where the shipped images put it.
    pub ticks: u64,
    /// Layout seed.
    pub seed: u64,
    /// Layout half-extent, metres.
    pub extent: f32,
    /// Window width, pixels.
    pub width: u32,
    /// Window height, pixels.
    pub height: u32,
    /// Frames to draw before closing, or `None` to keep the window open.
    pub frames: Option<u32>,
    /// Which graphics backend to open.
    pub backend: Backend,
    /// A single view to open at, or `None` for the first.
    pub view: Option<String>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            population: 2000,
            ticks: 600,
            seed: 7,
            extent: 900.0,
            width: DEFAULT_WIDTH,
            height: DEFAULT_HEIGHT,
            frames: None,
            backend: Backend::Auto,
            view: None,
        }
    }
}

/// What the command line asked for.
#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    /// Open the window, with these options.
    Run(Box<Options>),
    /// Print the usage and stop.
    Help,
}

/// Which graphics backend to open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Backend {
    /// Whatever the machine can do, best first.
    #[default]
    Auto,
    /// Metal: the native backend on macOS, and the only one an Apple GPU exposes.
    Metal,
    /// Vulkan, including `lavapipe`, which is what a machine with no GPU reports.
    Vulkan,
    /// OpenGL ES.
    Gl,
}

impl Backend {
    /// Parse a command-line name.
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "auto" => Some(Self::Auto),
            "metal" => Some(Self::Metal),
            "vulkan" => Some(Self::Vulkan),
            "gl" | "gles" => Some(Self::Gl),
            _ => None,
        }
    }

    /// The backend name as a flag would spell it.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Metal => "metal",
            Self::Vulkan => "vulkan",
            Self::Gl => "gl",
        }
    }

    /// The wgpu mask this selection means.
    pub const fn mask(self) -> Backends {
        match self {
            Self::Auto => Backends::PRIMARY.union(Backends::SECONDARY),
            Self::Metal => Backends::METAL,
            Self::Vulkan => Backends::VULKAN,
            Self::Gl => Backends::GL,
        }
    }
}

/// Everything the playground can fail with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The demo would not build: a bad seed, a missing asset.
    Demo(String),
    /// A flag was given a value that is not valid for it.
    Usage(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Demo(why) => write!(formatter, "demo: {why}"),
            Self::Usage(why) => write!(formatter, "usage: {why}"),
        }
    }
}

impl std::error::Error for Error {}

/// The usage text, which is also the crate's front door.
pub const USAGE: &str = "\
ashfall-playground — the Ashfall crowd, drawn by Bevy on a real GPU

    ashfall-playground [flags]

Flags:
    --view NAME       open at one of the shipped views (district, street, hero,
                      skyline, procession) instead of the first
    --frames N        draw N frames and close, for a smoke test on a machine
                      with no one watching
    --width N         window width in pixels (default 1280, at most 16384)
    --height N        window height in pixels (default 800, at most 16384)
    --backend NAME    auto, metal, vulkan or gl (default auto; metal on macOS)
    --population N    NPCs to simulate (default 2000)
    --ticks N         ticks before the first frame (default 600)
    --seed N          layout seed (default 7)
    --extent M        layout half-extent, metres (default 900)
    --help, -h        print this

Controls:
    WASD move · Q/E down and up · shift sprint · right mouse look
    Tab next view · Escape quit
";

impl Options {
    /// Read the flags.
    ///
    /// # Errors
    ///
    /// [`Error::Usage`] when a flag is unknown, is missing its value, or is given one that does not
    /// parse. The window size is checked as a pair, because either flag alone can make the pair
    /// invalid.
    pub fn parse(arguments: impl Iterator<Item = String>) -> Result<Request, Error> {
        let mut options = Self::default();
        let mut width = options.width;
        let mut height = options.height;
        let mut arguments = arguments;
        while let Some(argument) = arguments.next() {
            let mut value = || {
                arguments
                    .next()
                    .ok_or_else(|| Error::Usage(format!("{argument} needs a value")))
            };
            match argument.as_str() {
                "--population" => options.population = parse(&value()?)?,
                "--ticks" => options.ticks = parse(&value()?)?,
                "--seed" => options.seed = parse(&value()?)?,
                "--extent" => options.extent = parse(&value()?)?,
                "--width" => width = parse(&value()?)?,
                "--height" => height = parse(&value()?)?,
                "--frames" => options.frames = Some(parse(&value()?)?),
                "--view" => options.view = Some(value()?),
                "--backend" => {
                    let name = value()?;
                    options.backend = Backend::parse(&name)
                        .ok_or_else(|| Error::Usage(format!("unknown backend `{name}`")))?;
                }
                "--help" | "-h" => return Ok(Request::Help),
                other => return Err(Error::Usage(format!("unknown flag `{other}`"))),
            }
        }
        if width == 0 || height == 0 {
            return Err(Error::Usage(format!(
                "a window must be at least 1x1 pixel, not {width}x{height}"
            )));
        }
        if width > MAX_DIMENSION || height > MAX_DIMENSION {
            return Err(Error::Usage(format!(
                "a window may be at most {MAX_DIMENSION} pixels a side, not {width}x{height}"
            )));
        }
        options.width = width;
        options.height = height;
        Ok(Request::Run(Box::new(options)))
    }
}

/// Parse one flag's value, naming the flag in the error rather than the type.
fn parse<T: std::str::FromStr>(value: &str) -> Result<T, Error> {
    value
        .parse()
        .map_err(|_| Error::Usage(format!("`{value}` is not a number")))
}

/// How many frames are left before the window closes itself.
#[derive(Resource, Debug)]
struct ExitAfter {
    /// Frames still to draw.
    remaining: u32,
}

/// Count down and ask the app to exit, so `--frames` is a smoke test rather than a hang.
fn exit_after_frames(mut remaining: ResMut<ExitAfter>, mut exit: MessageWriter<AppExit>) {
    if remaining.remaining == 0 {
        exit.write(AppExit::Success);
        return;
    }
    remaining.remaining -= 1;
}

/// The fog: a linear falloff from the eye to the draw distance, near the demo's sky.
const fn fog() -> DistanceFog {
    DistanceFog {
        color: Color::srgb(0.36, 0.40, 0.47),
        directional_light_color: Color::srgba(1.0, 0.85, 0.65, 0.35),
        directional_light_exponent: 24.0,
        falloff: FogFalloff::Linear {
            start: 120.0,
            end: 1_400.0,
        },
    }
}

/// A camera at the first view, looking where the demo would have pointed it.
fn spawn_camera(mut commands: Commands, views: Res<Views>) {
    let (eye, target, fov, near, far) = views.get(views.current).map_or(
        (
            Vec3::new(0.0, 40.0, 130.0),
            Vec3::new(0.0, 4.0, 0.0),
            core::f32::consts::FRAC_PI_3,
            0.1,
            3_000.0,
        ),
        |shot| {
            (
                shot.camera.eye,
                shot.camera.target,
                shot.camera.fov_y,
                shot.camera.near,
                shot.camera.far,
            )
        },
    );
    let fly = FlyCamera::looking_from(eye, target, BASE_SPEED);
    commands.spawn((
        Camera3d::default(),
        Camera::default(),
        Projection::Perspective(PerspectiveProjection {
            fov,
            near,
            far,
            ..PerspectiveProjection::default()
        }),
        Transform::from_translation(eye).with_rotation(fly.rotation()),
        fly,
        // Per-camera, so a second camera would not inherit the dusk: Bevy's global ambient is the
        // fallback, and this is the view's own light.
        AmbientLight {
            color: Color::srgb(0.55, 0.62, 0.75),
            brightness: 220.0,
            ..AmbientLight::default()
        },
        fog(),
    ));
}

/// The sun, aimed along the demo's light direction.
fn spawn_sun(mut commands: Commands, views: Res<Views>) {
    let (sun, colour) = views.get(views.current).map_or_else(
        || (Vec3::new(0.3, 1.0, 0.45).normalize(), [1.0, 0.92, 0.8]),
        |shot| {
            (
                shot.atmosphere.sun.normalize_or_zero(),
                shot.atmosphere.sun_colour,
            )
        },
    );
    // A directional light shines along its own `-Z`, so it is turned to look along the sun.
    let rotation = Transform::default().looking_to(-sun, Vec3::Y).rotation;
    commands.spawn((
        DirectionalLight {
            color: Color::srgb(
                colour.first().copied().unwrap_or(1.0),
                colour.get(1).copied().unwrap_or(1.0),
                colour.get(2).copied().unwrap_or(1.0),
            ),
            illuminance: 11_000.0,
            shadow_maps_enabled: true,
            ..DirectionalLight::default()
        },
        Transform::from_rotation(rotation),
    ));
}

/// Build the app: the plugins a 3D scene needs, the scene itself, and the systems that steer it.
pub fn build_app(scene: AshfallScene, views: Views, options: &Options) -> App {
    let mut app = App::new();
    app.add_plugins((
        TaskPoolPlugin::default(),
        TimePlugin,
        TransformPlugin,
        InputPlugin,
        WindowPlugin {
            primary_window: Some(Window {
                title: format!(
                    "Ashfall playground — {} — {}x{}",
                    options.backend.name(),
                    options.width,
                    options.height
                ),
                resolution: WindowResolution::new(options.width, options.height),
                ..Window::default()
            }),
            ..WindowPlugin::default()
        },
        AccessibilityPlugin,
        AssetPlugin::default(),
        ImagePlugin::default(),
        RenderPlugin {
            render_creation: RenderCreation::Automatic(Box::new(WgpuSettings {
                backends: Some(options.backend.mask()),
                ..WgpuSettings::default()
            })),
            ..RenderPlugin::default()
        },
        CorePipelinePlugin,
        PbrPlugin::default(),
        FrameCountPlugin,
        // Last: it installs the event loop, and a plugin added after it would never see a frame.
        WinitPlugin::default(),
    ));

    app.insert_resource(scene);
    app.insert_resource(views);
    app.add_systems(
        Startup,
        (spawn_camera, spawn_sun, scene::spawn_scene).chain(),
    );
    app.add_systems(Update, (camera::fly, camera::cycle));
    app
}

/// Build the scene the command line asked for, and open the window on it.
///
/// # Errors
///
/// [`Error::Demo`] when the crowd or the assets cannot be built, [`Error::Usage`] when `--view`
/// names a view the demo does not ship.
pub fn run(options: &Options) -> Result<(), Error> {
    let mut playground = Playground::new(options.seed, options.population, options.extent)
        .map_err(|error| Error::Demo(format!("{error}")))?;
    playground.drain_spawn();
    playground.place_crowd();
    playground.tick(options.ticks);
    let bodies = playground.bodies();
    let scene = Scene::build(&playground.city, &bodies);
    let meshes = Meshes::load().map_err(|error| Error::Demo(format!("{error}")))?;
    let mut shots = render3d::shots(&playground.city, &bodies);

    if let Some(only) = options.view.as_deref() {
        let shipped: Vec<&str> = shots.iter().map(|shot| shot.name).collect();
        shots.retain(|shot| shot.name == only);
        if shots.is_empty() {
            return Err(Error::Usage(format!(
                "no view `{only}`; the demo ships {}",
                shipped.join(", ")
            )));
        }
    }

    let names = shots.iter().map(|shot| shot.name).collect();
    let views = Views { shots, current: 0 };
    let ashfall = AshfallScene::new(scene, meshes, names);
    let mut app = build_app(ashfall, views, options);
    if let Some(frames) = options.frames {
        app.insert_resource(ExitAfter { remaining: frames });
        app.add_systems(Update, exit_after_frames);
    }
    app.run();
    Ok(())
}

/// The asset directory the demo reads, for a caller that wants to check it exists first.
pub fn asset_root() -> PathBuf {
    bevy_flock_demo::assets::root()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse a command line written as a shell would split it.
    fn parse_line(line: &str) -> Result<Request, Error> {
        Options::parse(line.split_whitespace().map(str::to_string))
    }

    /// The usage error a line has to produce, so a test that expects a refusal cannot pass silently.
    fn usage_error(line: &str) -> Error {
        match parse_line(line) {
            Ok(_) => unreachable!("`{line}` was accepted"),
            Err(error) => error,
        }
    }

    /// The defaults are the shipped view of the district, at the demo's size and seed.
    #[test]
    fn the_defaults_are_the_district() {
        let default = Options::default();
        assert_eq!(default.width, DEFAULT_WIDTH);
        assert_eq!(default.height, DEFAULT_HEIGHT);
        assert_eq!(default.backend, Backend::Auto);
        assert_eq!(default.view, None);
        assert_eq!(default.frames, None);
        assert_eq!(default.population, 2000);
        assert_eq!(default.seed, 7);
    }

    /// Every flag lands on its field.
    #[test]
    fn every_flag_is_read() {
        let Ok(Request::Run(options)) = parse_line(
            "--population 500 --ticks 30 --seed 11 --extent 250.5 \
             --width 800 --height 600 --frames 4 --view hero --backend metal",
        ) else {
            unreachable!("the line was refused")
        };
        assert_eq!(options.population, 500);
        assert_eq!(options.ticks, 30);
        assert_eq!(options.seed, 11);
        assert!((options.extent - 250.5).abs() < 1e-6);
        assert_eq!((options.width, options.height), (800, 600));
        assert_eq!(options.frames, Some(4));
        assert_eq!(options.view.as_deref(), Some("hero"));
        assert_eq!(options.backend, Backend::Metal);
    }

    /// A backend name is case-sensitive and refuses nonsense rather than falling back to auto.
    #[test]
    fn backend_names_parse_and_nonsense_is_refused() {
        for (name, backend) in [
            ("auto", Backend::Auto),
            ("metal", Backend::Metal),
            ("vulkan", Backend::Vulkan),
            ("gl", Backend::Gl),
            ("gles", Backend::Gl),
        ] {
            assert_eq!(Backend::parse(name), Some(backend));
            assert_eq!(backend.name(), if name == "gles" { "gl" } else { name });
        }
        assert_eq!(Backend::parse("Metal"), None);
        assert!(matches!(usage_error("--backend direct3d"), Error::Usage(_)));
    }

    /// Metal and Vulkan select exactly one backend; auto selects the two that matter.
    #[test]
    fn the_backend_mask_is_the_backend() {
        assert!(Backend::Metal.mask().contains(Backends::METAL));
        assert!(!Backend::Metal.mask().contains(Backends::VULKAN));
        assert!(Backend::Vulkan.mask().contains(Backends::VULKAN));
        assert!(Backend::Gl.mask().contains(Backends::GL));
        assert!(Backend::Auto.mask().contains(Backends::METAL));
        assert!(Backend::Auto.mask().contains(Backends::VULKAN));
    }

    /// `--help` is a request, not an error, and it does not need a valid line around it.
    #[test]
    fn help_is_answered() {
        assert_eq!(parse_line("--help"), Ok(Request::Help));
        assert_eq!(parse_line("-h"), Ok(Request::Help));
        assert_eq!(parse_line("--width 10 --help"), Ok(Request::Help));
    }

    /// A missing value names the flag that is missing it.
    #[test]
    fn a_missing_value_names_its_flag() {
        let error = usage_error("--width");
        assert!(error.to_string().contains("--width"), "{error}");
        let error = usage_error("--view");
        assert!(error.to_string().contains("--view"), "{error}");
    }

    /// An unknown flag is refused by name rather than ignored.
    #[test]
    fn an_unknown_flag_is_refused() {
        let error = usage_error("--quality ultra");
        assert!(error.to_string().contains("--quality"), "{error}");
    }

    /// A window with no pixels in it, or more than a texture can hold, is refused at the flag.
    #[test]
    fn the_window_size_is_checked() {
        for line in ["--width 0", "--height 0", "--width 0 --height 0"] {
            let error = usage_error(line);
            assert!(error.to_string().contains("at least 1x1"), "{error}");
        }
        for line in ["--width 20000", "--height 16385"] {
            let error = usage_error(line);
            assert!(error.to_string().contains("at most"), "{error}");
        }
        // The bounds themselves are legal, in either order.
        for line in ["--width 16384 --height 1", "--height 1 --width 16384"] {
            assert!(
                matches!(parse_line(line), Ok(Request::Run(_))),
                "{line} was refused"
            );
        }
    }

    /// A value that is not a number is a usage error, not a panic and not a silent default.
    #[test]
    fn a_non_numeric_value_is_refused() {
        assert!(matches!(usage_error("--frames many"), Error::Usage(_)));
        assert!(matches!(usage_error("--seed -1"), Error::Usage(_)));
    }

    /// The usage text names every flag the parser accepts, so the two cannot drift.
    #[test]
    fn the_usage_lists_every_flag() {
        for flag in [
            "--view",
            "--frames",
            "--width",
            "--height",
            "--backend",
            "--population",
            "--ticks",
            "--seed",
            "--extent",
            "--help",
        ] {
            assert!(USAGE.contains(flag), "the usage does not mention {flag}");
        }
    }

    /// The demo's own fog and camera math is what the scene is lit with, so the light direction is
    /// the sun the demo would have used rather than an arbitrary one.
    #[test]
    fn the_fog_runs_from_the_eye_to_the_horizon() {
        let fog = fog();
        match fog.falloff {
            FogFalloff::Linear { start, end } => {
                assert!(start > 0.0);
                assert!(end > start);
            }
            other => unreachable!("the playground draws linear fog, not {other:?}"),
        }
    }
}
