//! A city block that notices you, greets you, and gets on with its day.
//!
//! Run with:
//!
//! ```text
//! cargo run --release --example ambient_metropolis
//! ```
//!
//! This is the integration example: spawn a crowd, put a player character in the
//! middle of it, and watch perception, barks, the render plan and the world director
//! work together. Everything printed comes from the crate's own resources — a game
//! would render the plan and play the barks instead.

// An example's job is to be read: it prints, it unwraps, and it keeps its setup next to
// the thing it demonstrates rather than hoisting every constant to the top.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::missing_docs_in_private_items,
    clippy::items_after_statements,
    clippy::float_cmp
)]
use bevy_app::App;
use bevy_flock::adapters::primary::dialogue::{Barks, NpcBarks};
use bevy_flock::adapters::primary::observability::Diagnostics;
use bevy_flock::adapters::primary::perception::PlayerCharacter;
use bevy_flock::adapters::primary::render::RenderPlan;
use bevy_flock::adapters::primary::world_director::{AmbientMood, CrowdDirector};
use bevy_flock::behavior::bark::{BarkCategory, BarkRequest};
use bevy_flock::prelude::*;
use bevy_math::Vec3;
use bevy_transform::prelude::Transform;

fn main() {
    let population = PopulationSize::try_new(600).expect("600 is valid");
    let config = FlockConfig::default().with_population(population);

    let mut app = App::new();
    app.add_plugins(FlockPlugin::new(config));

    // The player arrives at noon, three metres from the middle of the market.
    app.world_mut().spawn((
        Npc(NpcId::unique(999_999)),
        Transform::from_translation(Vec3::new(3.0, 0.0, 3.0)),
        PlayerCharacter,
    ));

    // Warm up so the crowd is spawned before anyone is asked to notice anything.
    for _ in 0..40 {
        app.update();
    }

    // Ask the nearest NPCs to greet the player. In a game this would come from the
    // perception system's own change detection; asking explicitly keeps the example
    // readable.
    let nearby: Vec<NpcId> = app
        .world_mut()
        .query::<(&Npc, &Transform)>()
        .iter(app.world())
        .filter(|(_, transform)| transform.translation.distance(Vec3::new(3.0, 0.0, 3.0)) < 12.0)
        .map(|(npc, _)| npc.0)
        .take(6)
        .collect();
    {
        let mut barks = app.world_mut().resource_mut::<Barks>();
        for npc in nearby {
            barks.request(BarkRequest {
                npc,
                category: BarkCategory::Greeting,
                at: Tick::ZERO,
            });
        }
    }

    for _ in 0..120 {
        app.update();
    }

    let diagnostics = app.world().resource::<Diagnostics>().clone();
    let plan = app.world().resource::<RenderPlan>();
    let mood = app.world().resource::<AmbientMood>().0;
    let director = app.world().resource::<CrowdDirector>();

    println!("{}", diagnostics.report());
    println!(
        "render plan      {} instances in {} draw calls ({} culled, {} truncated)",
        plan.len(),
        plan.draw_calls(),
        plan.culled(),
        plan.truncated()
    );
    println!(
        "director         {} policy, {} decisions, ambient mood {mood:+.2}",
        director.label(),
        director.decisions()
    );

    let mut barks = app.world_mut().resource_mut::<Barks>();
    let table = barks.table().lines().to_vec();
    println!("\nwhat the street said:");
    let mut spoke = 0;
    let events: Vec<_> = barks.queue_mut().drain().collect();
    for event in &events {
        if let Some(line) = table.get(event.line) {
            println!("  {:?}: \"{}\"", event.category, line.text);
            spoke += 1;
        }
    }
    if spoke == 0 {
        println!("  (nothing: the greeting cooldown had already been spent)");
    }

    // The NPCs that spoke remember it, which is what stops them repeating themselves.
    let states = app
        .world_mut()
        .query::<&NpcBarks>()
        .iter(app.world())
        .filter(|state| state.0.last_line().is_some())
        .count();
    println!("\n{states} NPCs now remember their last line");
}
