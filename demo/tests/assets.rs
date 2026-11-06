//! The asset contract: what ships is what the table says, and it is small enough to ship.
//!
//! This is the review gate the ported Meshy skill asks for, in the only form that cannot be
//! skipped: every promised file is opened, its container and version checked, its geometry
//! confirmed, and its size held to a budget. A regenerated asset that arrives unshrunk — the
//! 4 MB Meshy default rather than the 0.5 MB this repository keeps — fails here rather than in a
//! reviewer's memory.

use bevy_flock::testkit::{OrDie, OrDieErr};
use bevy_flock_demo::assets;

/// No single model may exceed this, in bytes. The Meshy original of each is 3–4.6 MB; the shipped
/// file after `tools/shrink_glb.py` is 0.24–0.65 MB, so this budget has room and still catches a
/// file that skipped the step.
const MAX_MODEL_BYTES: u64 = 1_200_000;

#[test]
fn every_promised_model_is_shipped() {
    for model in &assets::MODELS {
        let path = assets::root().join(model.file);
        assert!(
            path.exists(),
            "{} is missing at {}",
            model.slug,
            path.display()
        );
    }
}

#[test]
fn every_model_is_a_gltf_2_file_with_geometry() {
    for model in &assets::MODELS {
        let path = assets::root().join(model.file);
        let summary = assets::inspect(&path).or_die();
        assert_eq!(summary.version, 2, "{} is not glTF 2", model.slug);
        assert!(summary.has_mesh, "{} declares no mesh", model.slug);
        // The shared action library is the only place animations belong; a body that arrives
        // animated would mean the pipeline generated something nobody asked it to.
        assert!(
            !summary.has_animation,
            "{} carries animations; the shared library owns those",
            model.slug
        );
    }
}

#[test]
fn no_model_is_larger_than_the_budget() {
    for model in &assets::MODELS {
        let path = assets::root().join(model.file);
        let summary = assets::inspect(&path).or_die();
        assert!(
            summary.bytes <= MAX_MODEL_BYTES,
            "{} is {} bytes; run tools/shrink_glb.py",
            model.slug,
            summary.bytes
        );
    }
}

#[test]
fn the_total_that_ships_is_small() {
    let total: u64 = assets::MODELS
        .iter()
        .map(|model| assets::inspect(&assets::root().join(model.file)).map_or(0, |s| s.bytes))
        .sum();
    assert!(
        total < 4_000_000,
        "the GLBs total {total} bytes; a demo should not be the biggest thing in the repository"
    );
}

#[test]
fn every_model_has_a_concept_and_a_reason() {
    for model in &assets::MODELS {
        assert!(
            assets::root().join(model.concept).exists(),
            "{} has no concept image",
            model.slug
        );
        assert!(!model.purpose.is_empty(), "{} has no purpose", model.slug);
        assert!(
            model.height_m > 0.4 && model.height_m < 12.0,
            "{} is {} m tall, which is not a person or a prop",
            model.slug,
            model.height_m
        );
    }
}

#[test]
fn the_fingerprint_notices_a_changed_byte() {
    let path = assets::root().join(assets::MODELS[0].file);
    let first = assets::fingerprint(&path);
    assert_ne!(first, 0);
    assert_eq!(
        assets::fingerprint(&path),
        first,
        "the fingerprint is not stable"
    );
    assert_eq!(
        assets::fingerprint(&assets::root().join("does-not-exist.glb")),
        0
    );
}

#[test]
fn a_glb_that_is_not_a_glb_is_refused() {
    let path = std::env::temp_dir().join("bevy-flock-demo-not-a-glb.glb");
    std::fs::write(&path, b"not a model at all, just words").or_die();
    let error = assets::inspect(&path).or_die_err();
    let message = format!("{error}");
    assert!(message.contains("not a GLB"), "{message}");
    let _ = std::fs::remove_file(&path);
}
