//! What a GPU frame costs, split into the half the CPU pays and the half it waits for.
//!
//! The three cases answer three different questions, and the first is the one most benchmarks get
//! wrong:
//!
//! - `frame` is submit **and wait**: the latency of one frame, which is what a game feels.
//! - `eight_frames_per_drain` is eight frames per wait: throughput. Splitting the CPU half from the
//!   GPU half is only meaningful when the two are different processors, and on a CPU-backed adapter
//!   (`lavapipe`) they are not — the submit *is* the rasterisation. Writing a case called `encode`
//!   here measured 992 ms per iteration against 0.1 ms of measured submit time inside the binary,
//!   because the queue had nowhere to drain: the loop was measuring saturation, not encoding. That
//!   mistake is why the cases are named after what they do rather than after the phase they are
//!   supposed to isolate.
//! - `frame_with_readback` adds the copy of the frame out of the texture. A windowed front end never
//!   pays it; a headless one that writes PNGs pays it every frame.
//!
//! Run with `ASHFALL_BACKEND=noop` on a machine with no driver: every command is validated and none
//! is executed, so the numbers measure encoding alone and the images are blank. The printed adapter
//! line is part of the result, not decoration.
//!
//! ```text
//! cargo bench --bench frame                       # auto: Vulkan, then GL, then noop
//! ASHFALL_BACKEND=noop cargo bench --bench frame   # encode only, anywhere
//! ```

use bevy_flock_demo::app::Playground;
use bevy_flock_demo::render3d::{self, Meshes};
use bevy_flock_demo::scene::Scene;
use bevy_flock_demo_gpu::{Backend, GpuRenderer};
use criterion::{Criterion, criterion_group, criterion_main};

/// The population and tick count the other Ashfall artefacts use.
const POPULATION: u32 = 2000;
/// Ticks before the first frame, so the crowd is in the same place as the shipped images.
const TICKS: u64 = 600;
/// The layout seed the shipped images use.
const SEED: u64 = 7;
/// Layout half-extent, metres.
const EXTENT: f32 = 900.0;
/// Frame width: the software comparison's district width, halved.
const WIDTH: u32 = 600;
/// Frame height.
const HEIGHT: u32 = 375;

fn bench_frames(criterion: &mut Criterion) {
    let backend = std::env::var("ASHFALL_BACKEND").map_or(Backend::Auto, |name| {
        Backend::parse(&name).unwrap_or_else(|| {
            eprintln!("ASHFALL_BACKEND=`{name}` is not a backend; using auto");
            Backend::Auto
        })
    });
    let Some(fixture) = fixture() else {
        eprintln!("benchmark skipped: the scene could not be built");
        return;
    };
    let mut renderer =
        match GpuRenderer::open(&fixture.scene, &fixture.meshes, backend, WIDTH, HEIGHT) {
            Ok(renderer) => renderer,
            Err(error) => {
                // A machine with no driver is a machine that cannot run this benchmark; say so rather
                // than reporting zero, which would read as "free".
                eprintln!("benchmark skipped: {error}");
                return;
            }
        };
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
    renderer.set_shot(&fixture.shot);
    // Warm up the pipeline and the crowd upload before any case is timed.
    renderer.draw();
    renderer.wait();

    let mut group = criterion.benchmark_group(format!(
        "gpu/{}/{}x{}",
        adapter.backend.to_lowercase(),
        WIDTH,
        HEIGHT
    ));
    group.sample_size(20);
    group.bench_function("frame", |bencher| {
        bencher.iter(|| {
            renderer.draw();
            renderer.wait();
        });
    });
    group.bench_function("eight_frames_per_drain", |bencher| {
        bencher.iter(|| {
            for _ in 0..8 {
                renderer.draw();
            }
            renderer.wait();
        });
    });
    group.bench_function("frame_with_readback", |bencher| {
        bencher.iter(|| {
            renderer.draw();
            renderer.wait();
            let _ = std::hint::black_box(renderer.read_rgba());
        });
    });
    group.finish();
}

/// The scene, the meshes and the shot the benchmark draws.
struct Fixture {
    /// What to draw.
    scene: Scene,
    /// The models it names.
    meshes: Meshes,
    /// The camera, light and fog.
    shot: bevy_flock_demo::render3d::Shot,
}

/// Build the same city, crowd and view the shipped artefacts use.
fn fixture() -> Option<Fixture> {
    let mut playground = Playground::new(SEED, POPULATION, EXTENT).ok()?;
    let _ = playground.drain_spawn();
    playground.place_crowd();
    playground.tick(TICKS);
    let bodies = playground.bodies();
    let scene = Scene::build(&playground.city, &bodies);
    let meshes = Meshes::load().ok()?;
    let shot = render3d::shots(&playground.city, &bodies)
        .into_iter()
        .find(|shot| shot.name == "district")?;
    Some(Fixture {
        scene,
        meshes,
        shot,
    })
}

criterion_group!(benches, bench_frames);
criterion_main!(benches);
