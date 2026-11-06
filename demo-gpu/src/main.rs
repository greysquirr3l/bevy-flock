//! `ashfall-gpu` — render the Ashfall views with a real graphics pipeline, and time the frames.
//!
//! Same city, same crowd, same seeds and the same [`Shot`] table as `ashfall-3d`'s software render,
//! so the two sets of images are comparable and the two timings are of the same work. One thing
//! differs and has to: the software renderer integrates over four sub-pixel taps and this one does
//! not, so a GPU frame at the same resolution is aliased where a software frame is smoothed. That is
//! a property of the comparison, not a bug in either.
//!
//! ```text
//! ashfall-gpu --backend auto --view district --width 1200 --height 750 --frames 30
//! ```

use bevy_flock_demo::app::Playground;
use bevy_flock_demo::render3d::{self, Meshes, Shot};
use bevy_flock_demo::scene::Scene;
use bevy_flock_demo_gpu::{Backend, GpuError, GpuRenderer};
use std::path::{Path, PathBuf};
use std::time::Instant;

/// What to render and how long to time it for.
struct Options {
    /// Population to simulate.
    population: u32,
    /// Ticks to run before the first frame.
    ticks: u64,
    /// Layout seed.
    seed: u64,
    /// Layout half-extent, metres.
    extent: f32,
    /// Frame width, pixels.
    width: u32,
    /// Frame height, pixels.
    height: u32,
    /// Frames to time per view, after two warm-up frames.
    frames: u32,
    /// Which backend to open.
    backend: Backend,
    /// A single view's name, or `all`.
    view: Option<String>,
    /// Where the PNGs go.
    out: PathBuf,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            population: 2000,
            ticks: 600,
            seed: 7,
            extent: 900.0,
            width: 600,
            height: 375,
            frames: 20,
            backend: Backend::Auto,
            view: None,
            out: Path::new(env!("CARGO_MANIFEST_DIR")).join("target/gpu"),
        }
    }
}

/// Everything `run` can fail with.
#[derive(Debug)]
enum Error {
    /// The demo's own error.
    Demo(bevy_flock_demo::DemoError),
    /// The GPU layer's error.
    Gpu(GpuError),
    /// A file could not be written.
    Io(String),
    /// A flag was given a value that is not valid for it.
    Usage(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Demo(error) => write!(formatter, "demo: {error}"),
            Self::Gpu(error) => write!(formatter, "gpu: {error}"),
            Self::Io(why) => write!(formatter, "io: {why}"),
            Self::Usage(why) => write!(formatter, "usage: {why}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<bevy_flock_demo::DemoError> for Error {
    fn from(error: bevy_flock_demo::DemoError) -> Self {
        Self::Demo(error)
    }
}

impl From<GpuError> for Error {
    fn from(error: GpuError) -> Self {
        Self::Gpu(error)
    }
}

fn main() -> std::process::ExitCode {
    match run(std::env::args().skip(1)) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("ashfall-gpu: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

/// Parse the flags, render, and print one JSON line per view.
fn run(arguments: impl Iterator<Item = String>) -> Result<(), Error> {
    let mut options = Options::default();
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
            "--width" => options.width = parse(&value()?)?,
            "--height" => options.height = parse(&value()?)?,
            "--frames" => options.frames = parse(&value()?)?,
            "--view" => options.view = Some(value()?),
            "--out" => options.out = PathBuf::from(value()?),
            "--backend" => {
                let name = value()?;
                options.backend = Backend::parse(&name)
                    .ok_or_else(|| Error::Usage(format!("unknown backend `{name}`")))?;
            }
            "--help" | "-h" => {
                println!("{USAGE}");
                return Ok(());
            }
            other => return Err(Error::Usage(format!("unknown flag `{other}`"))),
        }
    }
    std::fs::create_dir_all(&options.out)
        .map_err(|error| Error::Io(format!("{}: {error}", options.out.display())))?;

    let started = Instant::now();
    let mut playground = Playground::new(options.seed, options.population, options.extent)?;
    let _ = playground.drain_spawn();
    playground.place_crowd();
    playground.tick(options.ticks);
    let bodies = playground.bodies();
    let scene = Scene::build(&playground.city, &bodies);
    let meshes = Meshes::load()?;
    let shots = render3d::shots(&playground.city, &bodies);
    println!(
        "sim     {} districts · {} blocks · seed {} · {} NPCs after {} ticks, in {:.2} s",
        playground.city.districts.len(),
        playground.city.blocks.len(),
        options.seed,
        bodies.len(),
        options.ticks,
        started.elapsed().as_secs_f64()
    );

    let mut renderer = GpuRenderer::open(
        &scene,
        &meshes,
        options.backend,
        options.width,
        options.height,
    )?;
    let adapter = renderer.adapter().clone();
    println!(
        "adapter {} {}",
        adapter.label(),
        if adapter.is_software() {
            "(software)"
        } else {
            ""
        }
    );
    println!(
        "scene   {}x{} · {} candidates (bodies, ruins, ground) before a shot selects",
        options.width,
        options.height,
        renderer.candidates()
    );

    for shot in &shots {
        if let Some(only) = options.view.as_deref()
            && only != "all"
            && only != shot.name
        {
            continue;
        }
        render_view(&mut renderer, shot, &options)?;
    }
    println!("done    in {:.2} s", started.elapsed().as_secs_f64());
    Ok(())
}

/// Time one view, read it back, and write it as a PNG.
fn render_view(renderer: &mut GpuRenderer, shot: &Shot, options: &Options) -> Result<(), Error> {
    renderer.set_shot(shot);
    // Two frames before the clock starts: the first pays for the pipeline's lazy driver work and the
    // upload of the crowd, and both belong to warm-up, not to the frame budget.
    for _ in 0..2 {
        renderer.draw();
        renderer.wait();
    }
    let frames = options.frames.max(1);
    let mut cpu = Vec::with_capacity(frames as usize);
    let mut wall = Vec::with_capacity(frames as usize);
    for _ in 0..frames {
        let start = Instant::now();
        renderer.draw();
        cpu.push(start.elapsed().as_secs_f64() * 1e3);
        renderer.wait();
        wall.push(start.elapsed().as_secs_f64() * 1e3);
    }
    let pixels = renderer.read_rgba()?;
    let path = options.out.join(format!("ashfall-gpu-{}.png", shot.name));
    std::fs::write(&path, png(&pixels, options.width, options.height))
        .map_err(|error| Error::Io(format!("{}: {error}", path.display())))?;
    // Pixels that differ from the clear colour are the ones the pass actually drew; counting non-zero
    // bytes would count the fog as a crowd.
    let clear = renderer.clear_colour();
    let drawn = pixels
        .chunks_exact(4)
        .filter(|pixel| {
            pixel.first().copied().unwrap_or(0) != clear[0]
                || pixel.get(1).copied().unwrap_or(0) != clear[1]
                || pixel.get(2).copied().unwrap_or(0) != clear[2]
        })
        .count();
    let stats = renderer.stats();
    println!(
        r#"{{"view":"{}","cpu_ms":{:.3},"frame_ms":{:.3},"instances":{},"draws":{},"triangles":{},"pixels_drawn":{},"bytes":{},"path":"{}"}}"#,
        shot.name,
        median(&mut cpu),
        median(&mut wall),
        stats.instances,
        stats.draws,
        stats.triangles,
        drawn,
        pixels.len(),
        path.display()
    );
    Ok(())
}

/// A flag's value, parsed.
fn parse<T: std::str::FromStr>(text: &str) -> Result<T, Error>
where
    T::Err: std::fmt::Display,
{
    text.parse::<T>()
        .map_err(|error| Error::Usage(format!("`{text}`: {error}")))
}

/// The middle of a set of samples, which is what a frame budget should report.
fn median(samples: &mut [f64]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    samples.sort_by(f64::total_cmp);
    samples.get(samples.len() / 2).copied().unwrap_or_default()
}

/// A PNG of RGBA pixels, top row first.
///
/// The demo crate already carries a deflate implementation for the software renderer's own PNGs
/// ([`bevy_flock_demo::raster::zlib`]); this reuses it rather than pulling in an image crate for one
/// function.
fn png(rgba: &[u8], width: u32, height: u32) -> Vec<u8> {
    let mut raw = Vec::with_capacity(rgba.len() + height as usize);
    let stride = (width * 4) as usize;
    for row in 0..height as usize {
        raw.push(0);
        let start = row * stride;
        match rgba.get(start..start + stride) {
            Some(slice) => raw.extend_from_slice(slice),
            // A short readback is a driver bug, and a truncated PNG is the honest report of it.
            None => break,
        }
    }
    let mut out = Vec::with_capacity(raw.len() / 4 + 64);
    out.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
    let mut header = Vec::with_capacity(13);
    header.extend_from_slice(&width.to_be_bytes());
    header.extend_from_slice(&height.to_be_bytes());
    header.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(&mut out, *b"IHDR", &header);
    chunk(&mut out, *b"IDAT", &bevy_flock_demo::raster::zlib(&raw));
    chunk(&mut out, *b"IEND", &[]);
    out
}

/// Append one PNG chunk: length, type, data, and the CRC of type and data.
fn chunk(out: &mut Vec<u8>, kind: [u8; 4], data: &[u8]) {
    // A PNG chunk cannot exceed 2^31-1 bytes, and this one is a frame; saturating keeps the narrowing
    // total without pretending a larger chunk could be written.
    let length = u32::try_from(data.len()).unwrap_or(u32::MAX);
    out.extend_from_slice(&length.to_be_bytes());
    out.extend_from_slice(&kind);
    out.extend_from_slice(data);
    let mut crc = 0xffff_ffff_u32;
    for byte in kind.iter().chain(data.iter()) {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    out.extend_from_slice(&(crc ^ 0xffff_ffff).to_be_bytes());
}

/// The flags, printed by `--help`.
const USAGE: &str = "\
ashfall-gpu — render the Ashfall views on a graphics device

  --backend auto|vulkan|gl|noop   which wgpu backend to open (default auto)
  --view district|street|hero|skyline|procession|all
  --width N --height N            frame size in pixels (default 600x375)
  --frames N                      frames to time after two warm-up frames (default 20)
  --population N --ticks N --seed N --extent M
  --out DIR                       where the PNGs go (default target/gpu)

Prints one JSON line per view with the median CPU submit time, the median
submit-and-wait time, and the number of lit pixels. `--backend noop` encodes
every frame and draws none, so it runs anywhere but produces a blank image.
";
