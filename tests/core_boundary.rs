//! Guards the line between the simulation core and the Bevy adapters.
//!
//! The crate is laid out as a hexagon — `domain`, `ports`, `behavior` and `application` are meant to be a
//! simulation that knows nothing about Bevy's app runtime or renderer, and `adapters` is the only layer that
//! touches them. Nothing enforced that but habit, and habit is what a crate split would eventually replace with a
//! compiler error. Until the split lands (phase 6, `plan.phase6.toml`), this test is the enforcement: it parses
//! the core layers and fails when one of them names an engine crate.
//!
//! It is deliberately a source scan rather than a dependency scan. `cargo tree` would only see the crates the
//! manifest names, and the whole point is that the manifest already names them for the adapter layer's benefit —
//! one crate's dependencies are available to every module in it, so the boundary can only be checked per file.
//!
//! Two kinds of failure are reported, and both matter:
//!
//! - A core file names a forbidden engine crate. Move the code that needs it into `adapters`.
//! - A file listed as a known `bevy_ecs` exception no longer uses it, or a new one appears. The list below is a
//!   to-do ledger for the crate split, not permission: when the split lands it should be empty.

use std::fs;
use std::path::{Path, PathBuf};

use bevy_flock::testkit::OrDie;

/// Layers that must not depend on the engine's app, transform, render or asset crates.
const CORE_LAYERS: [&str; 4] = ["domain", "ports", "behavior", "application"];

/// Engine crates no core file may name. `bevy_math` and `bevy_tasks` are absent on purpose: they are
/// engine-agnostic (a vector type and a task pool) and the core legitimately uses both.
const FORBIDDEN: [&str; 7] = [
    "bevy_app",
    "bevy_asset",
    "bevy_core_pipeline",
    "bevy_pbr",
    "bevy_render",
    "bevy_transform",
    "bevy_window",
];

/// Core files permitted to name `bevy_ecs` today, as paths relative to `src/`.
///
/// Every entry is a `#[derive(Resource)]` or a prelude import that exists so the adapter layer can put these types
/// in the ECS. The crate split re-homes them; until then the count is pinned, so a fourth one has to be argued for
/// rather than added.
const ECS_EXCEPTIONS: [&str; 3] = [
    "application/mod.rs",
    "behavior/registry.rs",
    "domain/memory.rs",
];

/// A core file naming something it should not.
#[derive(Debug)]
struct Violation {
    file: String,
    crate_name: String,
}

/// Tokens in `source` that name `crate_name`, ignoring comment lines.
///
/// Line-based because that is the granularity a reviewer reads at, and comment-aware because these files discuss
/// the boundary in prose — a doc comment saying "this must not reach into `bevy_transform`" is the opposite of a
/// violation, and a scanner that cannot tell the difference would punish the documentation that keeps the rule
/// alive.
fn names(source: &str, crate_name: &str) -> bool {
    source.lines().any(|line| {
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") {
            return false;
        }
        trimmed
            .split(|c: char| !(c.is_alphanumeric() || c == '_'))
            .any(|token| token == crate_name)
    })
}

/// Every `.rs` file under `dir`, recursively, in a stable order.
fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .collect();
    paths.sort();
    for path in paths {
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// The core source tree, rooted at `src/`.
fn core_sources() -> Vec<(String, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    for layer in CORE_LAYERS {
        let mut found = Vec::new();
        rust_files(&root.join(layer), &mut found);
        for path in found {
            let relative = path
                .strip_prefix(&root)
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default();
            let source = fs::read_to_string(&path).or_die();
            files.push((relative, source));
        }
    }
    files
}

#[test]
fn core_layers_do_not_name_the_engine() {
    let violations: Vec<Violation> = core_sources()
        .into_iter()
        .flat_map(|(file, source)| {
            FORBIDDEN
                .into_iter()
                .filter(move |crate_name| names(&source, crate_name))
                .map(move |crate_name| Violation {
                    file: file.clone(),
                    crate_name: crate_name.to_owned(),
                })
        })
        .collect();

    assert!(
        violations.is_empty(),
        "core layers named engine crates: {:?}\n\
         The simulation core is meant to run without Bevy's app runtime. Move the offending code into \
         `src/adapters`, or wrap the core type there, rather than widening the core's dependencies.",
        violations
            .iter()
            .map(|v| format!("{} -> {}", v.file, v.crate_name))
            .collect::<Vec<_>>()
    );
}

#[test]
fn bevy_ecs_appears_only_where_the_ledger_says() {
    let mut found: Vec<String> = core_sources()
        .into_iter()
        .filter(|(_, source)| names(source, "bevy_ecs"))
        .map(|(file, _)| file)
        .collect();
    found.sort();

    let expected: Vec<String> = ECS_EXCEPTIONS.iter().map(|s| (*s).to_owned()).collect();
    assert_eq!(
        found, expected,
        "the bevy_ecs exception ledger is out of date.\n\
         A new entry means a core type grew an ECS derive: either move that type's registration into \
         `src/adapters` or add the file to ECS_EXCEPTIONS with a note on which split task removes it.\n\
         A missing entry means one is gone, which is progress — shorten the ledger and say so in the commit."
    );
}

#[test]
fn core_layers_do_not_reach_into_the_adapters() {
    let leaks: Vec<String> = core_sources()
        .into_iter()
        .filter(|(_, source)| names(source, "adapters"))
        .map(|(file, _)| file)
        .collect();

    assert!(
        leaks.is_empty(),
        "core layers import the adapter layer, which inverts the hexagon: {leaks:?}"
    );
}

#[test]
fn the_scanner_ignores_comments_and_catches_attributes() {
    // The guard is only worth having if it fails for the right reasons, so the two cases it gets wrong most
    // easily are checked here rather than discovered in a review.
    assert!(!names(
        "// this must never reach into bevy_transform",
        "bevy_transform"
    ));
    assert!(!names(
        "/// `bevy_render` stays in the adapter layer",
        "bevy_render"
    ));
    assert!(names("#[derive(bevy_ecs::prelude::Resource)]", "bevy_ecs"));
    assert!(names("use bevy_app::prelude::*;", "bevy_app"));
    // A token, not a prefix: the crate has a `bevy_flock_app`-shaped name nowhere, but a substring match would
    // also flag `bevy_application` and make the guard cry wolf.
    assert!(!names("use bevy_application_shim::Thing;", "bevy_app"));
}

/// Every `bevy_ecs` mention left in a core file is gated by the `ecs` feature.
///
/// The ledger says *which* files may mention the engine. This says they may only mention it inside a
/// `#[cfg_attr(feature = "ecs", derive(...))]`, which is the difference between a temporary exception and a permanent
/// one: a bare `#[derive(Resource)]` would compile, pass the ledger check, and quietly put the engine back in the
/// core's dependency graph the moment the core crate stops enabling the feature.
///
/// This is the test that lets `bevy-flock-core` build with `default-features = false` and no `bevy_ecs` at all: if a
/// future derive creeps in ungated, it fails here rather than in a downstream crate that cannot compile.
#[test]
fn core_mentions_of_the_engine_are_feature_gated() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut checked = 0usize;
    for relative in ECS_EXCEPTIONS {
        let path = root.join(relative);
        let source = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(!source.is_empty(), "{} is unreadable", path.display());
        for (number, line) in source.lines().enumerate() {
            if !line.contains("bevy_ecs") {
                continue;
            }
            assert!(
                line.contains("feature = \"ecs\""),
                "{}:{} names the engine without gating it on the `ecs` feature: {line}",
                path.display(),
                number + 1
            );
            checked += 1;
        }
    }
    assert!(
        checked >= ECS_EXCEPTIONS.len(),
        "the gate check found nothing to check"
    );
}
