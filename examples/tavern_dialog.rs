//! A conversation that is authored, validated, and then played.
//!
//! Run with:
//!
//! ```text
//! cargo run --example tavern_dialog
//! ```
//!
//! The interesting part is not the text. It is that the graph is checked before it
//! is played: a dangling choice, a node nobody can reach and a loop with no way out
//! are all load errors, not surprises in a playtest.

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
use bevy_flock::behavior::bark::{BarkCategory, BarkRequest, BarkTable};
use bevy_flock::behavior::blackboard::{Blackboard, BlackboardKey, Value};
use bevy_flock::behavior::dialog::DialogGraph;
use bevy_flock::behavior::provider::{
    DialogContext, DialogPrompt, ProviderQueue, TemplateProvider,
};
use bevy_flock::domain::ids::{NpcId, SimSeed};
use bevy_flock::domain::tick::Tick;

const RUMOUR: BlackboardKey = BlackboardKey::new(7);

const GRAPH: &str = r#"(
    root: 0,
    nodes: [
        (id: 0, speaker: Npc, text: "You're not from the quay.",
         choices: [
            (text: "What happened at the quay?", next: 1),
            (text: "Just passing.", next: 2),
            (text: "About that debt...", next: 3, requires: Some((key: 7, at_least: 1.0))),
         ]),
        (id: 1, speaker: Npc, text: "Fire. Two barges and a warehouse. Nobody says whose.",
         choices: [(text: "Who would know?", next: 3)]),
        (id: 2, speaker: Npc, text: "Then mind the step and mind your purse.", choices: []),
        (id: 3, speaker: Npc, text: "Ask the harbourmaster. And don't mention my name.",
         choices: [(text: "Understood.", next: 2)]),
    ],
)"#;

fn main() {
    let graph = DialogGraph::from_ron(GRAPH).expect("the graph is valid");
    println!("graph: {} nodes, root {}\n", graph.len(), graph.root);

    let mut blackboard = Blackboard::new();
    let mut session = graph.open();
    let mut step = 0;
    while let Some(node) = session.line(&graph) {
        step += 1;
        println!("{step:>2}. {:?}: {}", node.speaker, node.text);
        let choices = session.choices(&graph, &blackboard);
        if choices.is_empty() {
            break;
        }
        for (index, choice) in choices.iter().enumerate() {
            println!("     [{index}] {}", choice.text);
        }
        // A fixed policy rather than input, so the example is reproducible: take the
        // first option. A game feeds a player's choice here instead.
        let picked = session
            .choose(&graph, &blackboard, 0)
            .expect("the first choice is always offered");
        if picked == 3 {
            // The debt line only shows once the blackboard knows about it.
            let _ = blackboard.set(RUMOUR, Value::Float(1.0));
        }
        println!();
        if step > 16 {
            println!("(stopping to avoid a long conversation)");
            break;
        }
    }

    println!("\nnow the same question through the provider seam, with no provider:\n");
    let table = BarkTable::starter().expect("starter barks");
    let mut queue = ProviderQueue::new();
    let mut fallback = TemplateProvider::new(table, SimSeed::new(3).get());
    let npc = NpcId::unique(1);
    let _ = queue.submit(
        DialogPrompt {
            npc,
            category: BarkCategory::Trade,
            deadline: Tick::ZERO,
            context: DialogContext {
                met_before: true,
                ..DialogContext::default()
            },
        },
        Tick::ZERO,
    );
    // No provider attached: the deadline is already met, so the fallback speaks.
    let mut silent = bevy_flock::behavior::provider::TemplateProvider::new(
        BarkTable::starter().expect("barks"),
        SimSeed::new(9).get(),
    );
    let replies = queue.tick(&mut silent, &mut fallback, Tick::ZERO);
    for reply in &replies {
        println!("{npc:?}: {} ({:?})", reply.text, reply.source);
    }
    println!("\nrequest the same bark twice:");
    let table = BarkTable::starter().expect("barks");
    let mut stream = bevy_flock::domain::rng::RngStream::new(SimSeed::new(1), 0);
    for _ in 0..2 {
        let line = table
            .pick(BarkCategory::Greeting, None, &mut stream)
            .expect("a line");
        println!("  {}", table.text(line).unwrap_or("?"));
    }
    let _ = BarkRequest {
        npc,
        category: BarkCategory::Greeting,
        at: Tick::ZERO,
    };
}
