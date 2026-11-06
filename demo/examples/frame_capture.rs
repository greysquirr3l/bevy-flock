//! Capture one interactive frame as a PNG, the way the terminal client draws it.
//!
//! The interactive front end paints one framebuffer pixel per half cell, so a terminal of `cols` by
//! `rows` cells shows `cols` by `rows * 2` pixels. That is the right size on a terminal and far too
//! small in a document, so this example renders the same frame through the same [`Session`] — with
//! the same budget, and therefore the same level-of-detail switch and cross-fade — and blows each
//! pixel up into a 4 by 4 block. `docs/demo/ashfall-play.png` is what a 120 by 44 capture of that
//! looks like, and this is how to make another one:
//!
//! ```text
//! cargo run --release --manifest-path demo/Cargo.toml --example frame_capture -- \
//!     --columns 120 --rows 44 --view street --out docs/demo/ashfall-play.png
//! ```
//!
//! `--band 0` renders the same frame with the level-of-detail switch and no cross-fade, which is the
//! comparison that shows the band is doing something rather than being configured and ignored.

use bevy_flock_demo::DemoError;
use bevy_flock_demo::app::Playground;
use bevy_flock_demo::render3d;
use bevy_flock_demo::session::Session;
use bevy_flock_demo::skinned;
use std::path::PathBuf;

/// Pixels per framebuffer pixel in the written image.
///
/// Four is chosen to match the height of a terminal half cell at the sizes the book uses: a cell is
/// four pixels wide and eight tall, which is what a monospace glyph looks like.
const BLOCK: u32 = 4;

/// Walkers in the procession, the same count the client draws.
const WALKERS: usize = 14;

/// What the command line asked for.
struct Options {
    /// Terminal cells across, which is also the framebuffer width.
    columns: u32,
    /// Terminal cells down, which is also the frame height in cells. The client's status line is
    /// chrome and is not part of a capture, so it is not subtracted here.
    rows: u32,
    /// Which shipped view to start on, by name.
    view: String,
    /// Cross-fade band in metres; zero is the hard switch.
    band: f32,
    /// Where to write the PNG.
    out: PathBuf,
    /// Crowd size.
    population: u32,
    /// Simulation seed.
    seed: u64,
    /// Ticks to run before the frame is taken.
    ticks: u64,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            columns: 120,
            rows: 44,
            view: "street".to_owned(),
            band: 8.0,
            out: PathBuf::from("frame.png"),
            population: 2_000,
            seed: 7,
            ticks: 600,
        }
    }
}

fn main() -> Result<(), DemoError> {
    let Some(options) = parse_arguments(std::env::args().skip(1))? else {
        return Ok(());
    };
    capture(&options)
}

/// Build the crowd, render one frame the way the client would, and write it out.
fn capture(options: &Options) -> Result<(), DemoError> {
    let mut playground = Playground::new(options.seed, options.population, 900.0)?;
    let _spawn_ticks = playground.drain_spawn();
    playground.place_crowd();
    playground.tick(options.ticks);
    let bodies = playground.bodies();
    let scene = bevy_flock_demo::scene::Scene::build(&playground.city, &bodies);
    let meshes = render3d::Meshes::load()?;
    let shots = render3d::shots(&playground.city, &bodies);
    if shots.is_empty() {
        return Err(DemoError::Config("the city framed no views".to_owned()));
    }
    let index = shots
        .iter()
        .position(|shot| shot.name == options.view)
        .ok_or_else(|| {
            DemoError::Config(format!(
                "`{}` is not a view; the views are district, street, procession, hero, skyline",
                options.view
            ))
        })?;
    let session = Session::new(&shots, index);
    let mut shot = session
        .shot(&shots)
        .ok_or_else(|| DemoError::Config("the chosen view has no shot".to_owned()))?;
    shot.impostor_band = options.band.max(0.0);
    // One pixel per half cell: a cell is drawn with the upper half block, so it carries two pixels.
    let width = options.columns.max(16);
    let height = options.rows.max(8) * 2;
    // The client draws the animated column *and* the crowd; a capture without it would be a picture
    // of a different scene than the one the text describes.
    let rig = skinned::Rig::load(&bevy_flock_demo::assets::root().join("anim/kaykit-walker.glb"))?;
    let centre = render3d::settlement_centre(&playground.city);
    let column = render3d::procession(centre, &rig, "Walking_B", WALKERS);
    let (image, stats) = render3d::render_with(
        &scene,
        &meshes,
        &[(&rig, "Walking_B", column.as_slice())],
        &shot,
        width,
        height,
    );
    let scaled = image.scaled(BLOCK);
    if let Some(parent) = options.out.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(|error| DemoError::Io {
            path: parent.display().to_string(),
            reason: error.to_string(),
        })?;
    }
    std::fs::write(&options.out, scaled.to_png()).map_err(|error| DemoError::Io {
        path: options.out.display().to_string(),
        reason: error.to_string(),
    })?;
    println!(
        "capture  {} x {} cells -> {} x {} px · view {} · band {:.1} m · {} bodies, {} triangles · {}",
        options.columns,
        options.rows,
        scaled.width,
        scaled.height,
        shot.name,
        shot.impostor_band,
        stats.bodies,
        stats.triangles,
        options.out.display()
    );
    Ok(())
}

/// Read the flags, or answer `--help` with `None`.
fn parse_arguments(arguments: impl Iterator<Item = String>) -> Result<Option<Options>, DemoError> {
    let mut options = Options::default();
    let mut arguments = arguments;
    while let Some(argument) = arguments.next() {
        let mut value = || {
            arguments
                .next()
                .ok_or_else(|| DemoError::Config(format!("{argument} needs a value")))
        };
        match argument.as_str() {
            "--columns" | "--cols" => options.columns = parse_number(&value()?)?,
            "--rows" => options.rows = parse_number(&value()?)?,
            "--view" => options.view = value()?,
            "--band" => options.band = parse_number(&value()?)?,
            "--out" => options.out = PathBuf::from(value()?),
            "--population" => options.population = parse_number(&value()?)?,
            "--seed" => options.seed = parse_number(&value()?)?,
            "--ticks" => options.ticks = parse_number(&value()?)?,
            "--help" | "-h" => {
                println!(
                    "frame_capture: one interactive frame as a PNG\n\
                     \n\
                     --columns N     terminal cells across (default 120)\n\
                     --rows N        terminal cells down (default 44)\n\
                     --view NAME     district, street, procession, hero or skyline (default street)\n\
                     --band METRES   impostor cross-fade band (default 8, 0 is the hard switch)\n\
                     --out PATH      where to write the PNG (default frame.png)\n\
                     --population N  crowd size (default 2000)\n\
                     --seed N        simulation seed (default 7)\n\
                     --ticks N       ticks before the frame (default 600)"
                );
                return Ok(None);
            }
            other => {
                return Err(DemoError::Config(format!(
                    "`{other}` is not a flag; try --help"
                )));
            }
        }
    }
    Ok(Some(options))
}

/// Parse a number, naming the flag that failed.
fn parse_number<T: std::str::FromStr>(text: &str) -> Result<T, DemoError> {
    text.parse::<T>()
        .map_err(|_| DemoError::Config(format!("`{text}` is not a number")))
}
