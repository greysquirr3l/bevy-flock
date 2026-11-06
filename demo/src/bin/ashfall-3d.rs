//! `ashfall-3d` — render the same city the plan draws, in three dimensions.
//!
//! ```text
//! cargo run --release --manifest-path demo/Cargo.toml --bin ashfall-3d -- --population 2000
//! ```
//!
//! Four views, four PNGs, and a report that says what each one cost and what it contained. No
//! window and no GPU: the rasteriser in `raster.rs` is part of the crate, so the images regenerate
//! on a CI runner and a reader can check that the picture came from the code beside it.
//!
//! The city, the crowd and the tick count are the same flags as the plan bin, so the two artefacts
//! describe the same run: `--seed 7 --population 2000 --ticks 600` is the pair in `demo/README.md`.
use bevy_flock::domain::num;
use bevy_flock_demo::DemoError;
use bevy_flock_demo::app::Playground;
use bevy_flock_demo::assets;
use bevy_flock_demo::json;
use bevy_flock_demo::render3d::{self, Meshes, Shot, Stats};
use bevy_flock_demo::scene::{self, Scene};
use bevy_flock_demo::skinned;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// What the command line asked for.
struct Options {
    population: u32,
    ticks: u64,
    seed: u64,
    extent: f32,
    width: u32,
    height: u32,
    supersample: u32,
    out: PathBuf,
    only: Option<String>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            population: 2_000,
            ticks: 600,
            seed: 7,
            extent: 900.0,
            width: 1200,
            height: 750,
            supersample: 2,
            out: Path::new(env!("CARGO_MANIFEST_DIR")).join("target/demo"),
            only: None,
        }
    }
}

fn main() -> std::process::ExitCode {
    match run(std::env::args().skip(1)) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("ashfall-3d: {error}");
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

    let started = Instant::now();
    let mut playground = Playground::new(options.seed, options.population, options.extent)?;
    let spawn_ticks = playground.drain_spawn();
    playground.place_crowd();
    playground.tick(options.ticks);
    let bodies = playground.bodies();
    let scene = Scene::build(&playground.city, &bodies);
    let meshes = Meshes::load()?;

    // One rig for the whole run: the walkers share a skeleton and differ only by phase, size and
    // tint, which is exactly the trade the crowd architecture makes.
    let rig = skinned::Rig::load(&assets::root().join("anim/kaykit-walker.glb"))?;
    let column = render3d::procession(
        render3d::settlement_centre(&playground.city),
        &rig,
        "Walking_B",
        18,
    );

    let shots = render3d::shots(&playground.city, &bodies);
    let mut rendered: Vec<(&Shot, Stats, u64)> = Vec::new();
    let mut total = 0.0_f64;
    for shot in &shots {
        if let Some(only) = options.only.as_deref()
            && only != shot.name
        {
            continue;
        }
        // Two taps per axis, averaged down: a kerb, a window mullion and a distant body are all
        // sub-pixel without it, and the difference is the difference between a diagram and a
        // photograph.
        let wide = options.width * options.supersample.max(1);
        let tall = options.height * options.supersample.max(1);
        let (image, stats) = if shot.name == "procession" {
            render3d::render_with(
                &scene,
                &meshes,
                &[(&rig, "Walking_B", column.as_slice())],
                shot,
                wide,
                tall,
            )
        } else {
            render3d::render(&scene, &meshes, shot, wide, tall)
        };
        let image = image.downsample(options.supersample.max(1));
        let path = options.out.join(format!("ashfall-3d-{}.png", shot.name));
        std::fs::write(&path, image.to_png()).map_err(|error| DemoError::Io {
            path: path.display().to_string(),
            reason: error.to_string(),
        })?;
        println!(
            "{:<9} {:>7} bodies {:>4} props {:>4} blocks {:>8} triangles {:>6.0} ms  {}",
            shot.name,
            stats.bodies,
            stats.props,
            stats.blocks,
            stats.triangles,
            stats.ms,
            path.display()
        );
        total += stats.ms;
        rendered.push((shot, stats, image.fingerprint()));
    }

    println!(
        "city    {} districts · {} blocks · seed {} · {} NPCs after {} ticks",
        playground.city.districts.len(),
        playground.city.blocks.len(),
        options.seed,
        bodies.len(),
        options.ticks
    );
    println!(
        "render  {} views · {:.1} ms total · {} triangles in the models",
        rendered.len(),
        total,
        meshes.triangles()
    );
    println!(
        "scene   {} bodies · {} props · spawn {} ticks · wall {:.2} s",
        scene.bodies.len(),
        scene.pylons.len() + scene.shelters.len() + scene.stalls.len(),
        spawn_ticks,
        started.elapsed().as_secs_f64()
    );

    if rendered.len() == shots.len() {
        let report = report_json(&options, &scene, &bodies, &meshes, &rendered);
        let path = options.out.join("ashfall-3d.json");
        std::fs::write(&path, report).map_err(|error| DemoError::Io {
            path: path.display().to_string(),
            reason: error.to_string(),
        })?;
        println!("wrote   {}", path.display());
    }
    Ok(())
}

fn report_json(
    options: &Options,
    scene: &Scene,
    bodies: &[scene::Body],
    meshes: &Meshes,
    rendered: &[(&Shot, Stats, u64)],
) -> String {
    let views = rendered
        .iter()
        .map(|(shot, stats, fingerprint)| {
            json::inline(&[
                ("view", json::string(shot.name)),
                ("note", json::string(shot.note)),
                ("eye", json::vec3(shot.camera.eye)),
                ("target", json::vec3(shot.camera.target)),
                ("fov_y_deg", json::number(shot.camera.fov_y.to_degrees(), 1)),
                ("bodies", stats.bodies.to_string()),
                ("props", stats.props.to_string()),
                ("blocks", stats.blocks.to_string()),
                ("triangles", stats.triangles.to_string()),
                ("coverage", json::number(stats.coverage, 3)),
                ("ms", json::number(num::f64_to_f32(stats.ms), 1)),
                ("supersample", options.supersample.to_string()),
                ("pixels", format!("\"0x{fingerprint:016x}\"")),
            ])
        })
        .collect::<Vec<_>>();

    let models = assets::MODELS
        .iter()
        .map(|model| {
            let triangles = meshes.get(model.slug).map_or(0, gltf_triangles);
            json::inline(&[
                ("model", json::string(model.slug)),
                (
                    "kind",
                    json::string(match model.kind {
                        assets::ModelKind::Body => "body",
                        assets::ModelKind::Prop => "prop",
                    }),
                ),
                ("triangles", triangles.to_string()),
                ("height_m", json::number(model.height_m, 2)),
            ])
        })
        .collect::<Vec<_>>();

    json::object(
        &[
            ("format", json::string("bevy-flock-demo-3d/1")),
            ("seed", options.seed.to_string()),
            ("population", options.population.to_string()),
            ("ticks", options.ticks.to_string()),
            ("width", options.width.to_string()),
            ("height", options.height.to_string()),
            (
                "scene",
                json::inline(&[
                    ("bodies", scene.bodies.len().to_string()),
                    ("pylons", scene.pylons.len().to_string()),
                    ("shelters", scene.shelters.len().to_string()),
                    ("stalls", scene.stalls.len().to_string()),
                    ("blocks", scene.blocks.len().to_string()),
                    (
                        "crowd_fingerprint",
                        format!("\"0x{:016x}\"", scene::crowd_fingerprint(bodies)),
                    ),
                ]),
            ),
            ("views", json::array_block(&views, 2)),
            ("models", json::array_block(&models, 2)),
            (
                "renderer",
                json::string(
                    "raster.rs: perspective projection, depth buffer, one directional light, \
                     hemispheric ambient, distance fog; no texture sampling",
                ),
            ),
        ],
        0,
    )
}

const fn gltf_triangles(mesh: &bevy_flock_demo::gltf::Mesh) -> usize {
    mesh.triangles()
}

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
            "--width" => options.width = parse(&value("--width")?)?,
            "--height" => options.height = parse(&value("--height")?)?,
            "--supersample" => options.supersample = parse(&value("--supersample")?)?,
            "--out" => options.out = PathBuf::from(value("--out")?),
            "--view" => options.only = Some(value("--view")?),
            "--help" | "-h" => {
                println!(
                    "ashfall-3d [--population N] [--ticks N] [--seed N] [--extent M] \
                     [--width PX] [--height PX] [--supersample N] [--view NAME] [--out DIR]\n\
                     views: district, street, hero, skyline"
                );
                return Ok(None);
            }
            other => {
                return Err(DemoError::Usage(format!(
                    "unknown flag `{other}`; try --help"
                )));
            }
        }
    }
    if options.width < 64 || options.height < 64 {
        return Err(DemoError::Usage(
            "the image must be at least 64 by 64".to_owned(),
        ));
    }
    Ok(Some(options))
}

fn parse<T: std::str::FromStr>(text: &str) -> Result<T, DemoError> {
    text.parse()
        .map_err(|_| DemoError::Usage(format!("`{text}` is not a number")))
}
