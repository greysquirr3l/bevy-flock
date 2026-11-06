//! `ashfall-play` — fly the playground in a terminal, or watch it fly itself.
//!
//! ```text
//! cargo run --release --manifest-path demo/Cargo.toml --bin ashfall-play
//! cargo run --release --manifest-path demo/Cargo.toml --bin ashfall-play -- --play 24 --out target/demo/flight
//! ```
//!
//! The picture is the same software rasteriser that writes the PNGs: no GPU, no window, no widget
//! toolkit. A cell is split by the upper-half block `▀`, so a terminal of `cols` by `rows` cells
//! shows an image of `cols` by `rows * 2` pixels with square pixels.
//!
//! Two modes, one loop:
//!
//! * **Interactive** (a tty on stdin): `wasd` walk, `e`/`c` climb and descend, arrows look, `1`-`5`
//!   jump to a view, `p` watch the scripted flight, `[`/`]` change the lens, `+`/`-` change speed,
//!   `h` help, `q` quit.
//! * **Playback** (`--play N`): the same flight the `p` key runs, `N` frames per view, written out as
//!   PNGs with a `playback.json` table of timings and fingerprints. No tty needed, so CI can run it.
//!
//! The interactive path needs a terminal for raw mode; when stdin is a pipe the binary says so and
//! points at `--play` rather than failing obscurely.
use bevy_flock::domain::num;
use bevy_flock_demo::DemoError;
use bevy_flock_demo::app::Playground;
use bevy_flock_demo::assets;
use bevy_flock_demo::input::{Command, Decoder};
use bevy_flock_demo::playback;
use bevy_flock_demo::render3d::{self, Meshes, Shot, Walker};
use bevy_flock_demo::scene::Scene;
use bevy_flock_demo::session::{Budget, Session};
use bevy_flock_demo::skinned;
use bevy_flock_demo::terminal::{self, RawTerminal};
use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

/// How long a frame may take before the loop stops pacing itself, seconds.
const FRAME_BUDGET: f32 = 1.0 / 30.0;

/// How long a lone escape byte waits for the rest of a sequence before it reads as Escape, seconds.
const ESCAPE_TIMEOUT: Duration = Duration::from_millis(140);

/// How many walkers the procession draws.
const WALKERS: usize = 14;

/// Beyond this distance the animated column is skipped, metres.
///
/// Skinning is per instance and per triangle, so a column a kilometre away costs the same as one at
/// arm's length for less than a pixel. The same LOD argument the renderer makes for bodies.
const WALKER_REACH: f32 = 140.0;

/// What the command line asked for.
struct Options {
    population: u32,
    ticks: u64,
    seed: u64,
    extent: f32,
    view: usize,
    columns: Option<u32>,
    rows: Option<u32>,
    play: Option<usize>,
    /// Draw the playback frames under the interactive budget instead of at full detail.
    budget: bool,
    out: PathBuf,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            population: 2_000,
            ticks: 600,
            seed: 7,
            extent: 900.0,
            view: 0,
            columns: None,
            rows: None,
            play: None,
            budget: false,
            out: Path::new(env!("CARGO_MANIFEST_DIR")).join("target/demo/flight"),
        }
    }
}

fn main() -> std::process::ExitCode {
    match run(std::env::args().skip(1)) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("ashfall-play: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run(arguments: impl Iterator<Item = String>) -> Result<(), DemoError> {
    let Some(options) = parse_arguments(arguments)? else {
        return Ok(());
    };
    let mut playground = Playground::new(options.seed, options.population, options.extent)?;
    let _spawn_ticks = playground.drain_spawn();
    playground.place_crowd();
    playground.tick(options.ticks);
    let bodies = playground.bodies();
    let scene = Scene::build(&playground.city, &bodies);
    let meshes = Meshes::load()?;
    let mut shots = render3d::shots(&playground.city, &bodies);
    if options.budget {
        // A video is watched at a few hundred pixels a side, where the full-detail meshes are four
        // hundred triangles a pixel: the same argument the interactive front end makes.
        shots = shots
            .iter()
            .map(|shot| Budget::default().apply(shot))
            .collect();
    }
    if shots.is_empty() {
        return Err(DemoError::Config("the city framed no views".to_owned()));
    }
    let rig = skinned::Rig::load(&assets::root().join("anim/kaykit-walker.glb"))?;
    let centre = render3d::settlement_centre(&playground.city);
    let column = render3d::procession(centre, &rig, "Walking_B", WALKERS);

    match options.play {
        Some(steps) => {
            let report = playback::fly(
                &shots,
                &scene,
                &meshes,
                steps,
                options.columns.unwrap_or(640),
                options.rows.unwrap_or(400),
                Some(&options.out),
            )?;
            println!(
                "flight  {} frames · {:.1} ms total · {:.1} ms mean · {}",
                report.frames.len(),
                report.total_ms(),
                report.mean_ms(),
                options.out.display()
            );
            for frame in report.frames.iter().take(5) {
                println!(
                    "  {:>3} {:<9} {:>7.1} ms  {:016x}",
                    frame.index, frame.view, frame.ms, frame.fingerprint
                );
            }
            Ok(())
        }
        None => interactive(&options, &shots, &scene, &meshes, (&rig, &column), centre),
    }
}

/// The terminal loop.
fn interactive(
    options: &Options,
    shots: &[Shot],
    scene: &Scene,
    meshes: &Meshes,
    walkers: (&skinned::Rig, &[Walker]),
    centre: bevy_math::Vec3,
) -> Result<(), DemoError> {
    let (rig, column) = walkers;
    if !std::io::stdin().is_terminal() {
        return Err(DemoError::Usage(
            "interactive mode needs a terminal; use --play <frames> for a scripted flight"
                .to_owned(),
        ));
    }
    let (columns, rows) = options
        .columns
        .zip(options.rows)
        .unwrap_or_else(|| terminal::size().unwrap_or((120, 34)));
    // Two pixels per cell, minus the status line and a spare row so the terminal does not scroll.
    let width = columns.max(16);
    let height = rows.saturating_sub(2).max(8) * 2;

    let mut session = Session::new(shots, options.view);
    let mut decoder = Decoder::new();
    let bytes = spawn_reader();
    let mut console = RawTerminal::enter().map_err(|error| DemoError::Usage(error.to_string()))?;

    let mut last = Instant::now();
    let mut last_render = Instant::now();
    let mut frames = 0_u64;
    let mut last_ms = 0.0_f64;
    let mut last_input = Instant::now();
    let mut running = true;
    while running {
        // Input first: everything the reader has, in one batch, so a fast typist does not queue a
        // frame per key.
        let mut received = false;
        loop {
            match bytes.try_recv() {
                Ok(chunk) => {
                    received = true;
                    for command in decoder.feed(&chunk) {
                        if command == Command::Quit {
                            running = false;
                        }
                        session.apply(command, shots);
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    running = false;
                    break;
                }
            }
        }
        if received {
            last_input = Instant::now();
        } else if !decoder.pending().is_empty() && last_input.elapsed() > ESCAPE_TIMEOUT {
            // A bare Escape is an escape sequence that never finished.
            for command in decoder.flush() {
                if command == Command::Quit {
                    running = false;
                }
                session.apply(command, shots);
            }
        }
        if !running {
            break;
        }

        let now = Instant::now();
        let step = now.duration_since(last).as_secs_f32();
        last = now;
        session.advance(&decoder.camera_input(), step);

        let Some(shot) = session.shot(shots) else {
            break;
        };
        let procession: &[(&skinned::Rig, &str, &[Walker])] =
            if shot.camera.eye.distance(centre) < WALKER_REACH {
                &[(rig, "Walking_B", column)]
            } else {
                &[]
            };
        let started = Instant::now();
        let (image, _stats) =
            render3d::render_with(scene, meshes, procession, &shot, width, height);
        last_ms = started.elapsed().as_secs_f64() * 1000.0;
        frames += 1;

        let elapsed = last_render.elapsed().as_secs_f64();
        last_render = Instant::now();
        let fps = if elapsed > 0.0 { 1.0 / elapsed } else { 0.0 };
        let mut screen = terminal::paint(&image, &session.status(shots, last_ms, fps));
        if session.help {
            screen.push_str(&help_overlay());
        }
        RawTerminal::draw(&screen).map_err(|error| DemoError::Io {
            path: "stdout".to_owned(),
            reason: error.to_string(),
        })?;

        // Pace the loop: a rasteriser this size runs faster than a person can read, and a terminal
        // that is rewritten a thousand times a second is a terminal nobody can use.
        let spent = last_ms / 1000.0;
        if spent < f64::from(FRAME_BUDGET) {
            std::thread::sleep(Duration::from_secs_f32(
                (FRAME_BUDGET - num::f64_to_f32(spent)).max(0.0),
            ));
        }
    }
    console.restore();
    println!(
        "ashfall-play: {frames} frames · {last_ms:.1} ms last frame · {} views · seed {}",
        shots.len(),
        options.seed
    );
    Ok(())
}

/// The help page, painted over the bottom of the screen.
fn help_overlay() -> String {
    let mut text = String::from("\x1b[2K");
    for line in Session::help_text() {
        text.push_str(line);
        text.push_str("\x1b[K\r\n");
    }
    text
}

/// Read stdin on a thread and hand the bytes over as they arrive.
///
/// A blocking read in the render loop would make the frame rate a function of the typist; a thread
/// that does nothing but read costs one stack and turns the loop's input into a non-blocking drain.
fn spawn_reader() -> Receiver<Vec<u8>> {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin();
        let mut buffer = [0_u8; 256];
        loop {
            // A zero read is end of file and an error is a closed stdin: both end the thread.
            match stdin.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    let chunk = buffer.get(..read).unwrap_or_default().to_vec();
                    if sender.send(chunk).is_err() {
                        break;
                    }
                }
            }
        }
    });
    receiver
}

/// Parse the command line. `Ok(None)` means `--help` was printed.
fn parse_arguments(arguments: impl Iterator<Item = String>) -> Result<Option<Options>, DemoError> {
    let mut options = Options::default();
    let mut arguments = arguments;
    while let Some(argument) = arguments.next() {
        let mut value = |name: &str| -> Result<String, DemoError> {
            arguments
                .next()
                .ok_or_else(|| DemoError::Usage(format!("{name} needs a value")))
        };
        match argument.as_str() {
            "--population" => options.population = parse(&value("--population")?)?,
            "--ticks" => options.ticks = parse(&value("--ticks")?)?,
            "--seed" => options.seed = parse(&value("--seed")?)?,
            "--extent" => options.extent = parse(&value("--extent")?)?,
            "--view" => options.view = view_index(&value("--view")?)?,
            "--columns" | "--cols" => options.columns = Some(parse(&value("--cols")?)?),
            "--rows" => options.rows = Some(parse(&value("--rows")?)?),
            "--play" => options.play = Some(parse(&value("--play")?)?),
            "--budget" => options.budget = true,
            "--out" => options.out = PathBuf::from(value("--out")?),
            "--help" | "-h" => {
                print_help();
                return Ok(None);
            }
            other => {
                return Err(DemoError::Usage(format!(
                    "unknown argument {other}; try --help"
                )));
            }
        }
    }
    Ok(Some(options))
}

/// Parse one number out of an argument.
fn parse<T: std::str::FromStr>(text: &str) -> Result<T, DemoError> {
    text.parse()
        .map_err(|_| DemoError::Usage(format!("{text} is not a number")))
}

/// A view by name or by number, counting from one as the keys do.
fn view_index(text: &str) -> Result<usize, DemoError> {
    const NAMES: [&str; 5] = ["district", "street", "procession", "hero", "skyline"];
    if let Some(index) = NAMES.iter().position(|name| *name == text) {
        return Ok(index);
    }
    let number: usize = parse(text)?;
    if number == 0 || number > NAMES.len() {
        return Err(DemoError::Usage(format!(
            "view {number} is not one of 1..={}",
            NAMES.len()
        )));
    }
    Ok(number - 1)
}

fn print_help() {
    println!("ashfall-play — fly the Ashfall Verge playground in a terminal");
    println!();
    println!("usage: ashfall-play [options]");
    println!();
    println!("  --view <name|1-5>   start on a view (district, street, procession, hero, skyline)");
    println!("  --population <n>    NPCs to place (default 2000)");
    println!("  --ticks <n>         simulation ticks before the picture (default 600)");
    println!("  --seed <n>          city and crowd seed (default 7)");
    println!("  --extent <m>        half-width of the city, metres (default 900)");
    println!("  --cols/--rows <n>   cells to render into; defaults to the terminal's own size");
    println!("  --play <n>          run n frames per view as a scripted flight, write PNGs, exit");
    println!("  --out <dir>         where --play writes (default demo/target/demo/flight)");
    println!("  --budget            draw under the interactive budget: video, not artefacts");
}
