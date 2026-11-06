//! `ashfall` — generate a city, run a crowd in it, write the artefacts.
//!
//! ```text
//! cargo run --release --manifest-path demo/Cargo.toml -- --population 4000 --ticks 900
//! ```
//!
//! The binary is deliberately dull: parse five flags, build a [`Playground`], step it, save what
//! came out. Everything interesting lives in the modules it calls, which is what makes the demo
//! testable without a window — and the tests drive the same `Playground`, so the thing that is
//! tested is the thing that runs.

use bevy_flock_demo::DemoError;
use bevy_flock_demo::app::Playground;
use bevy_flock_demo::assets;
use bevy_flock_demo::json;
use bevy_flock_demo::plan;
use bevy_flock_demo::report::RunReport;
use bevy_flock_demo::scene::{self, Scene};
use std::path::{Path, PathBuf};
use std::time::Instant;

/// What the command line asked for.
struct Options {
    population: u32,
    ticks: u64,
    seed: u64,
    extent: f32,
    out: PathBuf,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            population: 2_000,
            ticks: 600,
            seed: 7,
            extent: 900.0,
            out: Path::new(env!("CARGO_MANIFEST_DIR")).join("target/demo"),
        }
    }
}

fn main() -> std::process::ExitCode {
    match run(std::env::args().skip(1)) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("ashfall: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run(arguments: impl Iterator<Item = String>) -> Result<(), DemoError> {
    let Some(options) = parse_arguments(arguments)? else {
        return Ok(());
    };

    std::fs::create_dir_all(&options.out).map_err(|error| DemoError::Io {
        path: options.out.display().to_string(),
        reason: error.to_string(),
    })?;

    // Assets first: a demo that runs on a broken asset and reports success is worse than one that
    // refuses to start.
    let mut shipped = Vec::new();
    for model in &assets::MODELS {
        let path = assets::root().join(model.file);
        let summary = assets::inspect(&path)?;
        shipped.push((
            model.slug.to_owned(),
            summary.bytes,
            assets::fingerprint(&path),
        ));
    }

    let mut playground = Playground::new(options.seed, options.population, options.extent)?;
    print!("{}", plan::summary(&playground.city));

    let spawn_started = Instant::now();
    let spawn_ticks = playground.drain_spawn();
    let spawn_ms = spawn_started.elapsed().as_secs_f64() * 1_000.0;
    playground.place_crowd();

    let measured = Instant::now();
    playground.tick(options.ticks);
    let wall_ms = measured.elapsed().as_secs_f64() * 1_000.0;

    let bodies = playground.bodies();
    let diagnostics = playground.diagnostics();
    let city = &playground.city;

    let mut report = RunReport::from_diagnostics(&diagnostics);
    report.seed = options.seed;
    report.extent = options.extent;
    report.population = playground.live().max(report.population);
    report.spawn_ticks = spawn_ticks;
    report.wall_ms = wall_ms;
    report.ms_per_tick = wall_ms / options.ticks.max(1) as f64;
    report.crowd = scene::crowd_fingerprint(&bodies);
    report.counts = [
        city.districts.len(),
        city.blocks.len(),
        city.shelters.len(),
        city.pylons.len(),
        city.stalls.len(),
    ];
    report.assets = shipped;

    let scene = Scene::build(city, &bodies);
    write(&options.out.join("manifest.json"), &manifest())?;
    write(
        &options.out.join("ashfall.scene.json"),
        &scene.to_json(city, report.digest),
    )?;
    write(
        &options.out.join("ashfall.svg"),
        &plan::render(city, &bodies, report.digest, options.ticks),
    )?;
    write(&options.out.join("ashfall.run.json"), &report.to_json())?;

    println!();
    println!("{}", report.headline());
    println!(
        "visitor ({:.0}, {:.0}) — the anchor the cognition ladder measures from",
        playground.anchor.x, playground.anchor.z
    );
    println!("spawn   {spawn_ticks} ticks · {spawn_ms:.0} ms");
    println!(
        "scene   {} bodies · {} props · {} blocks",
        scene.bodies.len(),
        scene.pylons.len() + scene.shelters.len() + scene.stalls.len(),
        scene.blocks.len()
    );
    println!();
    for name in [
        "ashfall.svg",
        "ashfall.scene.json",
        "ashfall.run.json",
        "manifest.json",
    ] {
        println!("wrote   {}", options.out.join(name).display());
    }
    Ok(())
}

/// The asset manifest, generated from the table rather than kept beside it.
fn manifest() -> String {
    let models: Vec<String> = assets::MODELS
        .iter()
        .map(|model| {
            json::object(
                &[
                    ("slug", json::string(model.slug)),
                    ("file", json::string(model.file)),
                    (
                        "kind",
                        json::string(match model.kind {
                            assets::ModelKind::Body => "body",
                            assets::ModelKind::Prop => "prop",
                        }),
                    ),
                    ("height_m", json::number(model.height_m, 2)),
                    ("concept", json::string(model.concept)),
                    ("purpose", json::string(model.purpose)),
                ],
                2,
            )
        })
        .collect();
    json::object(
        &[
            ("format", json::string("bevy-flock-demo-manifest/1")),
            (
                "provenance",
                json::string(
                    "Meshy image-to-3d from the concept images beside them, then shrinkage of the \
                     embedded textures by tools/shrink_glb.py. Original designs: no third-party \
                     character, franchise or likeness is reproduced.",
                ),
            ),
            ("models", json::array_block(&models, 0)),
        ],
        0,
    )
}

fn write(path: &Path, contents: &str) -> Result<(), DemoError> {
    std::fs::write(path, contents).map_err(|error| DemoError::Io {
        path: path.display().to_string(),
        reason: error.to_string(),
    })
}

/// Parse the flags. `Ok(None)` means the help was printed and there is nothing to do.
fn parse_arguments(
    mut arguments: impl Iterator<Item = String>,
) -> Result<Option<Options>, DemoError> {
    let mut options = Options::default();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--help" | "-h" => {
                println!(
                    "ashfall — a bevy-flock playground\n\n\
                     USAGE:\n  ashfall [OPTIONS]\n\n\
                     OPTIONS:\n\
                     \x20 --population <N>  NPCs to simulate (default 2000)\n\
                     \x20 --ticks <N>       ticks to run after spawning (default 600)\n\
                     \x20 --seed <N>        city and crowd seed (default 7)\n\
                     \x20 --extent <M>      half-width of the city in metres (default 900)\n\
                     \x20 --out <DIR>       where the artefacts go (default demo/target/demo)"
                );
                return Ok(None);
            }
            "--population" => {
                options.population = next_value(&mut arguments, "--population")?
                    .parse()
                    .map_err(|_| DemoError::Usage("--population wants a number".to_owned()))?;
            }
            "--ticks" => {
                options.ticks = next_value(&mut arguments, "--ticks")?
                    .parse()
                    .map_err(|_| DemoError::Usage("--ticks wants a number".to_owned()))?;
            }
            "--seed" => {
                options.seed = next_value(&mut arguments, "--seed")?
                    .parse()
                    .map_err(|_| DemoError::Usage("--seed wants a number".to_owned()))?;
            }
            "--extent" => {
                options.extent = next_value(&mut arguments, "--extent")?
                    .parse()
                    .map_err(|_| {
                        DemoError::Usage("--extent wants a number of metres".to_owned())
                    })?;
            }
            "--out" => {
                options.out = PathBuf::from(next_value(&mut arguments, "--out")?);
            }
            other => {
                return Err(DemoError::Usage(format!(
                    "unknown argument `{other}`; try --help"
                )));
            }
        }
    }
    if options.ticks == 0 {
        return Err(DemoError::Usage("--ticks must be at least 1".to_owned()));
    }
    if options.population == 0 {
        return Err(DemoError::Usage(
            "--population must be at least 1".to_owned(),
        ));
    }
    Ok(Some(options))
}

fn next_value(
    arguments: &mut impl Iterator<Item = String>,
    flag: &str,
) -> Result<String, DemoError> {
    arguments
        .next()
        .ok_or_else(|| DemoError::Usage(format!("{flag} wants a value")))
}
