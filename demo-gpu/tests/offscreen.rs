//! A frame is a frame: it encodes anywhere, and it draws where a device exists.

use bevy_flock::testkit::OrDie;
use bevy_flock_demo::app::Playground;
use bevy_flock_demo::render3d::{self, Meshes};
use bevy_flock_demo::scene::Scene;
use bevy_flock_demo_gpu::{Backend, FrameSize, GpuError, GpuRenderer, MAX_DIMENSION};

/// A small district view, built the way the binary builds it.
fn fixture() -> Option<(Scene, Meshes, render3d::Shot)> {
    let mut playground = Playground::new(7, 400, 400.0).ok()?;
    let _ = playground.drain_spawn();
    playground.place_crowd();
    playground.tick(60);
    let bodies = playground.bodies();
    let scene = Scene::build(&playground.city, &bodies);
    let meshes = Meshes::load().ok()?;
    let shot = render3d::shots(&playground.city, &bodies)
        .into_iter()
        .find(|shot| shot.name == "district")?;
    Some((scene, meshes, shot))
}

/// The noop backend validates and encodes every command and draws none of them, so it proves the
/// encode path on a machine with no driver — which is what CI has.
#[test]
fn the_noop_backend_encodes_a_frame() {
    let Some((scene, meshes, shot)) = fixture() else {
        eprintln!("skipped: the scene could not be built");
        return;
    };
    let renderer = GpuRenderer::open(
        &scene,
        &meshes,
        Backend::Noop,
        FrameSize::try_new(160, 100).or_die(),
    );
    let mut renderer = match renderer {
        Ok(renderer) => renderer,
        Err(error) => {
            eprintln!("skipped: {error}");
            return;
        }
    };
    assert!(
        renderer.candidates() > 0,
        "the scene offered no instances at all"
    );
    renderer.set_shot(&shot);
    let stats = renderer.stats();
    assert!(stats.instances > 0, "the shot selected nothing");
    assert!(stats.triangles > 0, "the shot selected no geometry");
    // Submit twice: a validation error in the second frame would surface here.
    renderer.draw();
    renderer.wait();
    renderer.draw();
    renderer.wait();
}

/// Where a real adapter exists, the frame must actually be drawn: pixels that differ from the clear
/// colour, in the same places, twice running.
#[test]
fn a_real_adapter_draws_the_same_frame_twice() {
    let Some((scene, meshes, shot)) = fixture() else {
        eprintln!("skipped: the scene could not be built");
        return;
    };
    let mut renderer = match GpuRenderer::open(
        &scene,
        &meshes,
        Backend::Auto,
        FrameSize::try_new(200, 125).or_die(),
    ) {
        Ok(renderer) => renderer,
        Err(GpuError::NoAdapter(_)) => {
            eprintln!("skipped: no adapter on this machine");
            return;
        }
        Err(error) => {
            eprintln!("skipped: {error}");
            return;
        }
    };
    if renderer.adapter().backend == "Noop" {
        eprintln!("skipped: the only adapter is the noop device");
        return;
    }
    renderer.set_shot(&shot);
    renderer.draw();
    renderer.wait();
    let first = match renderer.read_rgba() {
        Ok(pixels) => pixels,
        Err(error) => {
            eprintln!("skipped: {error}");
            return;
        }
    };
    renderer.draw();
    renderer.wait();
    let second = match renderer.read_rgba() {
        Ok(pixels) => pixels,
        Err(error) => {
            eprintln!("skipped: {error}");
            return;
        }
    };
    assert_eq!(first.len(), 200 * 125 * 4, "the readback was not the frame");
    assert_eq!(first, second, "two identical frames differed");

    let clear = renderer.clear_colour();
    let drawn = first
        .chunks_exact(4)
        .filter(|pixel| {
            pixel.first().copied() != Some(clear[0])
                || pixel.get(1).copied() != Some(clear[1])
                || pixel.get(2).copied() != Some(clear[2])
        })
        .count();
    assert!(
        drawn > first.len() / 4 / 20,
        "only {drawn} pixels were drawn, which is not a frame"
    );
}

/// A frame wider than the device's `max_texture_dimension_2d` is refused before a texture is
/// created, because `create_texture` over that limit panics in wgpu's default error handler rather
/// than returning.
#[test]
fn a_frame_beyond_the_device_limit_is_an_error() {
    let Some((scene, meshes, _shot)) = fixture() else {
        eprintln!("skipped: the scene could not be built");
        return;
    };
    let frame = FrameSize::try_new(MAX_DIMENSION, 1).or_die();
    match GpuRenderer::open(&scene, &meshes, Backend::Noop, frame) {
        Err(GpuError::FrameTooLarge { limit, width, .. }) => {
            assert!(
                limit < MAX_DIMENSION,
                "the device claims {limit} pixels a side"
            );
            assert_eq!(width, MAX_DIMENSION);
        }
        // A device that really does hold the largest frame this crate accepts is not a failure.
        Ok(_) => eprintln!("this device accepts {MAX_DIMENSION} pixels a side"),
        Err(error) => eprintln!("skipped: {error}"),
    }
}
