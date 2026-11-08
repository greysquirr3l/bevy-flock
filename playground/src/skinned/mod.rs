//! Two thousand skinned NPCs on the district, from a clean checkout.
//!
//! This is the phase-2 artifact: the crowd is simulated by `bevy-flock`, animated by
//! `bevy-flock-anim`, and drawn by Bevy's PBR pipeline. Nothing is downloaded, nothing is baked and
//! there is no asset directory to point at — the body is built from boxes, the clips are keyframes
//! written in code, and the only shared resources are one mesh, one graph and one profile.
//!
//! ```text
//! cargo run --release --manifest-path playground/Cargo.toml --bin skinned -- --frames 600
//! ```
//!
//! # What a viewer should see
//!
//! Bodies walk the plaza at seven different paces, so the blend is visibly running: the slow ones
//! lean into a walk cycle, the fast ones into a run. Every third body looks at the monolith in the
//! middle, which is the gaze solve turning the pivot its chest is skinned to. Every ninety-seventh
//! body waves when its turn comes round.
//!
//! # Headless mode is the gate
//!
//! `--frames N --assert-motion` runs the example as a test: it exits non-zero unless the crowd
//! actually moved, the walk or run weight was non-zero on some body, a gesture reached a clip, and
//! some body's gaze was non-idle. That is the difference between "the example ran" and "the example
//! animated", and it is what CI runs under `xvfb-run`.

pub mod clips;
pub mod crowd;
pub mod rig;

use crate::skinned::rig::{ground, static_box};
use bevy_a11y::AccessibilityPlugin;
use bevy_animation::AnimationPlayer;
use bevy_animation::AnimationPlugin;
use bevy_app::{App, AppExit, Last, Startup, TaskPoolPlugin, Update};
use bevy_asset::{AssetPlugin, Assets};
use bevy_camera::{Camera, Camera3d, CameraPlugin, ClearColor, PerspectiveProjection, Projection};
use bevy_color::Color;
use bevy_core_pipeline::CorePipelinePlugin;
use bevy_core_pipeline::tonemapping::Tonemapping;
use bevy_diagnostic::FrameCountPlugin;
use bevy_ecs::message::MessageWriter;
use bevy_ecs::prelude::*;
use bevy_flock::adapters::primary::cognition::NpcBlackboard;
use bevy_flock::prelude::{
    FlockPlugin, GazeAim, GazeTarget, GestureChannel, GestureConfig, GestureKind, Npc, NpcGestures,
    SimClock, request_gesture,
};
use bevy_flock_anim::{AnimPlugin, ClipBindings, ClipSlot, GazePlugin, GesturePlugin};
use bevy_image::ImagePlugin;
use bevy_input::InputPlugin;
use bevy_light::{DirectionalLight, GlobalAmbientLight, LightPlugin};
use bevy_log::LogPlugin;
use bevy_math::Vec3;
use bevy_mesh::{Mesh3d, MeshPlugin};
use bevy_pbr::{DistanceFog, FogFalloff, MeshMaterial3d, PbrPlugin, StandardMaterial};
use bevy_render::RenderPlugin;
use bevy_render::settings::{RenderCreation, WgpuSettings};
use bevy_render::view::window::screenshot::{Screenshot, save_to_disk};
use bevy_time::TimePlugin;
use bevy_transform::TransformPlugin;
use bevy_transform::components::Transform;
use bevy_window::{Window, WindowPlugin, WindowResolution};
use bevy_winit::WinitPlugin;
use std::path::PathBuf;

use crate::Backend;
use clips::ClipError;
use crowd::{
    CrowdReport, RigError, RigHandles, build_handles, face_travel, mill_around, spawn_crowd,
};

/// Bodies the example spawns when no flag says otherwise.
pub const DEFAULT_POPULATION: usize = 2_000;
/// How wide the plaza is, in metres, for the default population.
pub const DEFAULT_RADIUS: f32 = 22.0;
/// Window width, pixels.
pub const DEFAULT_WIDTH: u32 = 1280;
/// Window height, pixels.
pub const DEFAULT_HEIGHT: u32 = 800;
/// Frames to draw after asking for a capture, so the readback lands before the window closes.
const CAPTURE_GRACE_FRAMES: u32 = 12;

/// What the command line asked for.
#[derive(Debug, Clone, PartialEq)]
pub struct Options {
    /// How many bodies to spawn.
    pub population: usize,
    /// How wide the plaza is, in metres.
    pub radius: f32,
    /// Frames to draw, or `None` for a window that stays open.
    pub frames: Option<u32>,
    /// Where to write a BMP of the last frame.
    pub capture: Option<PathBuf>,
    /// Window width, pixels.
    pub width: u32,
    /// Window height, pixels.
    pub height: u32,
    /// Which wgpu backend to open.
    pub backend: Backend,
    /// Whether to exit non-zero if nothing moved.
    pub assert_motion: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            population: DEFAULT_POPULATION,
            radius: DEFAULT_RADIUS,
            frames: None,
            capture: None,
            width: DEFAULT_WIDTH,
            height: DEFAULT_HEIGHT,
            backend: Backend::Auto,
            assert_motion: false,
        }
    }
}

/// What the arguments said to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    /// Print the usage and exit.
    Help,
    /// Run the example.
    Run(Options),
}

/// Why the example could not run.
#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    /// The command line was wrong.
    Usage(String),
    /// The rig could not be built.
    Rig(RigError),
    /// The capture path could not be made.
    Io(String),
    /// The crowd did not animate.
    Still(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Usage(message) | Self::Io(message) | Self::Still(message) => {
                formatter.write_str(message)
            }
            Self::Rig(error) => write!(formatter, "the rig could not be built: {error:?}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<ClipError> for Error {
    fn from(error: ClipError) -> Self {
        Self::Rig(RigError::Clips(error))
    }
}

/// The flags.
pub const USAGE: &str = "\
skinned — two thousand skinned NPCs on the district, from a clean checkout

USAGE:
    skinned [OPTIONS]

OPTIONS:
    --npc N            bodies to spawn (default 2000)
    --radius M         plaza radius in metres (default 22)
    --frames N         draw N frames and exit (default: run until the window closes)
    --capture PATH     write a BMP of the last frame to PATH
    --assert-motion    exit non-zero unless the crowd moved, blended, gestured and looked
    --width N          window width in pixels (default 1280)
    --height N         window height in pixels (default 800)
    --backend NAME     auto, metal, vulkan, gl or noop (default auto)
    -h, --help         print this text

The example is the phase-2 gate: it spawns the crowd, animates it from the simulation's own
locomotion, gaze and gesture state, and draws it through Bevy's PBR pipeline. Nothing is downloaded:
the body, the clips and the bind pose are all built in code.";

/// Why an argument could not be used.
fn usage(message: impl Into<String>) -> Error {
    Error::Usage(message.into())
}

/// Parse a number, naming the flag in the error rather than the number.
fn number<T: std::str::FromStr>(flag: &str, raw: &str) -> Result<T, Error> {
    raw.parse()
        .map_err(|_| usage(format!("{flag} wants a number, not `{raw}`")))
}

impl Options {
    /// Parse the arguments after the program name.
    ///
    /// # Errors
    ///
    /// [`Error::Usage`] for an unknown flag, a missing value or a value that does not parse.
    pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Request, Error> {
        let mut options = Self::default();
        let mut args = args.into_iter();
        while let Some(flag) = args.next() {
            let mut value = || {
                args.next()
                    .ok_or_else(|| usage(format!("{flag} wants a value")))
            };
            match flag.as_str() {
                "-h" | "--help" => return Ok(Request::Help),
                "--npc" => options.population = number(&flag, &value()?)?,
                "--radius" => options.radius = number(&flag, &value()?)?,
                "--frames" => options.frames = Some(number(&flag, &value()?)?),
                "--capture" => options.capture = Some(PathBuf::from(value()?)),
                "--width" => options.width = number(&flag, &value()?)?,
                "--height" => options.height = number(&flag, &value()?)?,
                "--backend" => {
                    let name = value()?;
                    options.backend = Backend::parse(&name)
                        .ok_or_else(|| usage(format!("no backend called `{name}`")))?;
                }
                "--assert-motion" => options.assert_motion = true,
                other => return Err(usage(format!("no flag called `{other}`"))),
            }
        }
        if options.population == 0 {
            return Err(usage("--npc 0 spawns nothing to look at"));
        }
        if options.width == 0 || options.height == 0 {
            return Err(usage("a window needs a width and a height"));
        }
        if !(options.radius.is_finite() && options.radius > 1.0) {
            return Err(usage("--radius wants a number of metres above one"));
        }
        Ok(Request::Run(options))
    }
}

/// What the crowd did, sampled every frame, so the run can be asserted on rather than eyeballed.
#[derive(Debug, Default, Clone, Copy, PartialEq, Resource)]
pub struct Telemetry {
    /// Bodies whose walk or run weight was above half on this frame.
    pub blended: usize,
    /// Bodies holding a gesture cue on this frame.
    pub gesturing: usize,
    /// Bodies with a non-idle gaze on this frame.
    pub looking: usize,
    /// Bodies that have moved since spawn.
    pub moved: usize,
    /// The largest per-frame move seen, in metres.
    pub largest_step: f32,
}

impl Telemetry {
    /// Whether the crowd is provably animating, and if not, what was missing.
    ///
    /// # Errors
    ///
    /// [`Error::Still`] naming every test that failed, because a CI failure that says "nothing
    /// moved" without saying which of the four things was missing sends the reader to the wrong file.
    pub fn check(&self) -> Result<(), Error> {
        let mut missing = Vec::new();
        if self.moved == 0 {
            missing.push("no body moved");
        }
        if self.blended == 0 {
            missing.push("no body blended walk or run");
        }
        if self.gesturing == 0 {
            missing.push("no gesture reached a clip");
        }
        if self.looking == 0 {
            missing.push("no body looked at anything");
        }
        if missing.is_empty() {
            return Ok(());
        }
        Err(Error::Still(missing.join("; ")))
    }

    /// One line for the console.
    #[must_use]
    pub fn describe(&self) -> String {
        format!(
            "{} moved, {} blended, {} gesturing, {} looking, biggest step {:.3} m",
            self.moved, self.blended, self.gesturing, self.looking, self.largest_step
        )
    }
}

/// The entity every third body looks at, which is what makes gaze legible in a still frame.
#[derive(Component, Debug, Clone, Copy)]
pub struct Landmark;

/// Bookkeeping for the run: frames left, capture, and the telemetry the assertion reads.
#[derive(Resource, Debug)]
struct Run {
    options: Options,
    frames_drawn: u32,
    captured: bool,
    grace: u32,
    settled: bool,
}

/// Where each body started, keyed by entity, so "moved" means moved rather than "exists".
///
/// Keyed rather than ordered because query iteration is archetype order, which has nothing to do
/// with spawn order once bodies start gaining and losing components.
#[derive(Resource, Debug, Default)]
struct SpawnPositions(std::collections::HashMap<Entity, Vec3>);

/// Build the app: plugins, resources, and the systems that make the crowd walk, blend and look.
pub fn build_app(options: &Options) -> App {
    let mut app = App::new();
    // Before the plugins, because `PbrPlugin` reads the ambient light while it builds its pipeline:
    // set afterwards, the resource says 120 and the shader keeps the default it was compiled with,
    // which reads as "the ambient setting does nothing" for an afternoon.
    app.insert_resource(GlobalAmbientLight {
        color: Color::srgb(0.58, 0.64, 0.78),
        brightness: 120.0,
        ..GlobalAmbientLight::default()
    });
    app.add_plugins((
        TaskPoolPlugin::default(),
        LogPlugin::default(),
        TimePlugin,
        InputPlugin,
        AccessibilityPlugin,
        TransformPlugin,
        FrameCountPlugin,
    ));
    app.add_plugins((
        AssetPlugin::default(),
        ImagePlugin::default(),
        MeshPlugin,
        AnimationPlugin,
        CameraPlugin,
        LightPlugin,
        RenderPlugin {
            render_creation: RenderCreation::Automatic(Box::new(WgpuSettings {
                backends: Some(options.backend.mask()),
                ..WgpuSettings::default()
            })),
            ..RenderPlugin::default()
        },
        CorePipelinePlugin,
        PbrPlugin::default(),
    ));
    app.add_plugins((
        bevy_window::WindowPlugin {
            primary_window: Some(Window {
                title: format!(
                    "skinned district — {} — {} bodies",
                    options.backend.name(),
                    options.population
                ),
                resolution: WindowResolution::new(options.width, options.height),
                ..Window::default()
            }),
            ..WindowPlugin::default()
        },
        WinitPlugin::default(),
    ));

    // The simulation, then the animation layer that reads it. Both are the shipped plugins: this
    // example adds no glue of its own between the crowd and its clips.
    app.add_plugins(FlockPlugin::default());
    app.add_plugins((AnimPlugin, GazePlugin, GesturePlugin));

    app.insert_resource(ClearColor(Color::srgb(0.36, 0.40, 0.47)));
    app.insert_resource(GlobalAmbientLight {
        color: Color::srgb(0.62, 0.68, 0.80),
        brightness: 120.0,
        ..GlobalAmbientLight::default()
    });
    app.insert_resource(GestureChannel::new(4_096));
    app.insert_resource(GestureConfig::default());
    app.insert_resource(Run {
        options: options.clone(),
        frames_drawn: 0,
        captured: false,
        grace: 0,
        settled: false,
    });
    app.init_resource::<SpawnPositions>();
    app.init_resource::<Telemetry>();

    app.add_systems(
        Startup,
        (
            spawn_camera,
            spawn_sun,
            spawn_ground,
            spawn_landmark,
            build_and_spawn_crowd,
        ),
    );
    app.add_systems(
        Update,
        (
            mill_around,
            face_travel,
            look_at_the_landmark,
            wave_now_and_then,
        ),
    );
    app.add_systems(Update, sample_crowd);
    app.add_systems(Last, lifecycle);
    app
}

/// Build the shared rig assets and spawn the crowd.
///
/// An exclusive system because spawning a body also spawns its five joints and then inserts the
/// skin on the body: five entities per body is not a shape `Commands` expresses without a bundle per
/// joint, and a bundle per joint would be a struct whose only job is to be inserted once.
fn build_and_spawn_crowd(world: &mut World) {
    let population = world
        .get_resource::<Run>()
        .map_or(DEFAULT_POPULATION, |run| run.options.population);
    let radius = world
        .get_resource::<Run>()
        .map_or(DEFAULT_RADIUS, |run| run.options.radius);

    let handles = build_handles(world);
    let handles: RigHandles = match handles {
        Ok(handles) => handles,
        Err(error) => {
            // The rig is constants, so this is a programming error. Report and stop rather than
            // spawn a crowd with no animation and let the assertion be the first to notice.
            eprintln!("skinned: the rig could not be built: {error:?}");
            world.write_message(AppExit::error());
            return;
        }
    };

    let CrowdReport { bodies, joints } = spawn_crowd(world, &handles, population, radius);
    let positions = collect_positions(world);
    world.insert_resource(SpawnPositions(positions));
    world.insert_resource(handles);
    println!("skinned: spawned {bodies} bodies and {joints} joints");
}

/// Every body's spawn position, for the movement test.
fn collect_positions(world: &mut World) -> std::collections::HashMap<Entity, Vec3> {
    let mut query = world.query::<(Entity, &Transform)>();
    query
        .iter(world)
        .map(|(entity, transform)| (entity, transform.translation))
        .collect()
}

/// A camera on the plaza, high enough to see the crowd as a crowd.
fn spawn_camera(mut commands: Commands) {
    commands.spawn((
        Camera3d::default(),
        Camera::default(),
        Projection::Perspective(PerspectiveProjection {
            fov: 0.85,
            ..PerspectiveProjection::default()
        }),
        Transform::from_xyz(15.0, 8.5, 15.0).looking_at(Vec3::new(0.0, 1.0, 0.0), Vec3::Y),
        // Not `TonyMcMapface`: that tonemapper needs the `tonemapping_luts` feature, and without it
        // the whole frame comes out magenta — a look that is easy to mistake for a material bug.
        Tonemapping::None,
        DistanceFog {
            color: Color::srgb(0.36, 0.40, 0.47),
            falloff: FogFalloff::Linear {
                start: 30.0,
                end: 95.0,
            },
            ..DistanceFog::default()
        },
    ));
}

/// A low sun, so the crowd casts shadows and the boxes read as bodies rather than as sprites.
fn spawn_sun(mut commands: Commands) {
    commands.spawn((
        DirectionalLight {
            illuminance: 8_000.0,
            ..DirectionalLight::default()
        },
        Transform::from_xyz(26.0, 22.0, -14.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));
}

/// The plaza: a plane the crowd stands on, so a shadow has somewhere to land.
fn spawn_ground(
    mut commands: Commands,
    mut meshes: ResMut<Assets<bevy_mesh::Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let plane = ground(160.0);
    commands.spawn((
        Mesh3d(meshes.add(plane)),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color: Color::srgb(0.30, 0.31, 0.30),
            perceptual_roughness: 0.95,
            ..StandardMaterial::default()
        })),
        Transform::from_xyz(0.0, -0.01, 0.0),
    ));
}

/// A monolith in the middle of the plaza, which is what the crowd looks at.
fn spawn_landmark(
    mut commands: Commands,
    mut meshes: ResMut<Assets<bevy_mesh::Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    commands.spawn((
        Landmark,
        Mesh3d(meshes.add(static_box(
            Vec3::new(-0.7, 0.0, -0.7),
            Vec3::new(0.7, 6.0, 0.7),
        ))),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color: Color::srgb(0.55, 0.48, 0.40),
            perceptual_roughness: 0.7,
            ..StandardMaterial::default()
        })),
        Transform::from_xyz(0.0, 3.0, 0.0),
    ));
}

/// Every third body looks at the monolith, with an interest that varies by body.
///
/// Written through the same component a behaviour leaf would write, so the gaze solve cannot tell
/// the difference between this example and a game.
fn look_at_the_landmark(
    clock: Res<SimClock>,
    landmark: Query<Entity, With<Landmark>>,
    mut bodies: Query<(&Npc, &mut GazeTarget)>,
) {
    let Some(target) = landmark.iter().next() else {
        return;
    };
    for (npc, mut gaze) in &mut bodies {
        let index = bevy_flock::domain::num::u32_to_usize(npc.0.to_bits());
        if !index.is_multiple_of(3) {
            // The rest look away, so the crowd is not a chorus line facing the same way.
            gaze.release(clock.tick);
            continue;
        }
        let interest = 0.45 + bevy_flock::domain::num::usize_to_f32(index % 5) * 0.1;
        gaze.look_at(target, interest, clock.tick);
    }
}

/// One body in ninety-seven waves on a slow rotation, so gestures are in evidence in any long run.
///
/// Requested exactly as a behaviour leaf would: through the blackboard, which the core's channel then
/// adjudicates. A refused request (on cooldown) is a fact the run report can see.
fn wave_now_and_then(clock: Res<SimClock>, mut bodies: Query<(&Npc, &mut NpcBlackboard)>) {
    let tick = clock.tick.get();
    for (npc, mut board) in &mut bodies {
        let index = bevy_flock::domain::num::u32_to_usize(npc.0.to_bits());
        if !index.is_multiple_of(97) || !tick.is_multiple_of(120) {
            continue;
        }
        // A body alternates between a wave and a nod, so both gesture clips get played.
        let kind = if (tick / 120).is_multiple_of(2) {
            GestureKind::Wave
        } else {
            GestureKind::Nod
        };
        request_gesture(&mut board.0, kind);
    }
}

/// Measure what the crowd is doing this frame.
fn sample_crowd(
    mut telemetry: ResMut<Telemetry>,
    bodies: Query<(
        Entity,
        &Transform,
        &NpcGestures,
        &GazeAim,
        &AnimationPlayer,
        &ClipBindings,
    )>,
    starts: Res<SpawnPositions>,
) {
    let mut moved = 0;
    let mut largest = 0.0_f32;
    let mut gesturing = 0;
    let mut looking = 0;
    let mut blended = 0;
    for (entity, transform, gestures, aim, player, bindings) in &bodies {
        if let Some(start) = starts.0.get(&entity) {
            let step = (transform.translation - *start).length();
            if step > 0.02 {
                moved += 1;
            }
            largest = largest.max(step);
        }
        if gestures.cue().is_some() {
            gesturing += 1;
        }
        if !aim.is_idle() {
            looking += 1;
        }
        // The blend's own weights are the honest test of "the clips are running": a node at zero
        // weight is a clip that was never asked to draw anything, however animated the body looks.
        let stride_node = bindings.node(ClipSlot::Walk);
        let run_node = bindings.node(ClipSlot::Run);
        let stride_weight = [stride_node, run_node]
            .into_iter()
            .flatten()
            .filter_map(|node| {
                player
                    .playing_animations()
                    .find(|(active, _)| **active == node)
                    .map(|(_, playing)| playing.weight())
            })
            .fold(0.0_f32, f32::max);
        if stride_weight > 0.5 {
            blended += 1;
        }
    }
    telemetry.moved = telemetry.moved.max(moved);
    telemetry.largest_step = telemetry.largest_step.max(largest);
    telemetry.gesturing = telemetry.gesturing.max(gesturing);
    telemetry.looking = telemetry.looking.max(looking);
    telemetry.blended = telemetry.blended.max(blended);
}

/// Count down, capture, report, exit.
fn lifecycle(
    mut run: ResMut<Run>,
    telemetry: Res<Telemetry>,
    mut commands: Commands,
    mut exit: MessageWriter<AppExit>,
) {
    run.frames_drawn = run.frames_drawn.saturating_add(1);
    let Some(limit) = run.options.frames else {
        return;
    };
    if run.frames_drawn < limit {
        return;
    }
    if let Some(path) = run.options.capture.clone()
        && !run.captured
    {
        commands
            .spawn(Screenshot::primary_window())
            .observe(save_to_disk(path));
        run.captured = true;
        run.grace = CAPTURE_GRACE_FRAMES;
        return;
    }
    if run.grace > 0 {
        run.grace = run.grace.saturating_sub(1);
        return;
    }
    if run.settled {
        return;
    }
    run.settled = true;

    println!(
        "skinned: {} frames — {}",
        run.frames_drawn,
        telemetry.describe()
    );
    let verdict = if run.options.assert_motion {
        telemetry.check()
    } else {
        Ok(())
    };
    match verdict {
        Ok(()) => {
            world_exit(&mut exit, AppExit::Success);
        }
        Err(error) => {
            eprintln!("skinned: the crowd did not animate: {error}");
            world_exit(&mut exit, AppExit::error());
        }
    }
}

/// Send an exit message, which is the one place the two branches above agree.
fn world_exit(exit: &mut MessageWriter<AppExit>, how: AppExit) {
    exit.write(how);
}

/// Build the app and run it.
///
/// # Errors
///
/// [`Error::Rig`] when the rig cannot be built, [`Error::Io`] when the capture directory cannot be
/// made, [`Error::Still`] when `--assert-motion` was given and the crowd did not animate.
pub fn run(options: &Options) -> Result<(), Error> {
    if let Some(path) = options.capture.as_ref() {
        let extension = path
            .extension()
            .and_then(std::ffi::OsStr::to_str)
            .map(str::to_ascii_lowercase);
        if extension.as_deref() != Some("bmp") {
            return Err(Error::Usage(
                "this build writes BMP captures: give a `.bmp` path".to_string(),
            ));
        }
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .map_err(|error| Error::Io(format!("{}: {error}", parent.display())))?;
        }
    }
    let mut app = build_app(options);
    app.run();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Backend, Error, Options, Request, Telemetry};

    fn parse(args: &[&str]) -> Result<Request, Error> {
        Options::parse(args.iter().map(|arg| (*arg).to_owned()))
    }

    fn options(args: &[&str]) -> Options {
        match parse(args) {
            Ok(Request::Run(options)) => options,
            other => unreachable!("expected a run, got {other:?}"),
        }
    }

    #[test]
    fn the_defaults_are_a_populated_plaza() {
        let options = options(&[]);
        assert_eq!(options.population, 2_000);
        assert!(options.radius > 1.0);
        assert_eq!(options.backend, Backend::Auto);
        assert!(!options.assert_motion);
        assert_eq!(options.frames, None);
    }

    #[test]
    fn every_flag_lands_on_its_field() {
        let options = options(&[
            "--npc",
            "17",
            "--radius",
            "9.5",
            "--frames",
            "40",
            "--capture",
            "/tmp/x.bmp",
            "--width",
            "320",
            "--height",
            "200",
            "--backend",
            "vulkan",
            "--assert-motion",
        ]);
        assert_eq!(options.population, 17);
        assert!(options.radius > 9.4);
        assert_eq!(options.frames, Some(40));
        assert_eq!(options.width, 320);
        assert_eq!(options.height, 200);
        assert_eq!(options.backend, Backend::Vulkan);
        assert!(options.assert_motion);
        assert!(options.capture.is_some());
    }

    #[test]
    fn help_is_not_a_run() {
        assert!(matches!(parse(&["--help"]), Ok(Request::Help)));
        assert!(matches!(parse(&["-h"]), Ok(Request::Help)));
    }

    #[test]
    fn bad_input_is_refused_rather_than_defaulted() {
        for args in [
            vec!["--npc"],
            vec!["--npc", "many"],
            vec!["--npc", "0"],
            vec!["--width", "0"],
            vec!["--radius", "0.5"],
            vec!["--backend", "toaster"],
            vec!["--wat"],
        ] {
            let error = parse(&args);
            assert!(
                matches!(error, Err(Error::Usage(_))),
                "{args:?} was accepted"
            );
        }
    }

    #[test]
    fn a_still_crowd_names_every_failure() {
        let error = Telemetry::default().check();
        let Err(Error::Still(reason)) = error else {
            unreachable!("an empty telemetry must fail");
        };
        for phrase in ["moved", "blended", "gesture", "looked"] {
            assert!(
                reason.contains(phrase),
                "{reason} does not mention {phrase}"
            );
        }
    }

    #[test]
    fn one_body_doing_everything_is_enough() {
        let telemetry = Telemetry {
            moved: 1,
            blended: 1,
            gesturing: 1,
            looking: 1,
            largest_step: 0.25,
        };
        assert!(telemetry.check().is_ok());
        assert!(telemetry.describe().contains("biggest step 0.250 m"));
    }
}
