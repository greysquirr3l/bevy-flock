//! Plugging your own logic into the crowd.
//!
//! Run with:
//!
//! ```text
//! cargo run --example custom_behavior
//! ```
//!
//! Three seams, all of them public and all of them the same shape: a trait, a cost
//! declaration, and no requirement that you use the built-in implementations.
//!
//! 1. A [`LeafBehavior`] — one action, shared by every NPC that runs it, with all
//!    per-NPC memory in the blackboard.
//! 2. A [`Director`] — a policy for the whole crowd that returns commands instead of
//!    touching the world.
//! 3. A [`GroupDirector`] — a policy for one group.
//!
//! The code is short on purpose: none of these is meant to be hard.

// An example's job is to be read: it prints, it unwraps, and it keeps its setup next to
// the thing it demonstrates rather than hoisting every constant to the top.
use bevy_flock::application::director::{
    Director, DirectorBoard, DirectorCommand, DirectorCommands, DirectorInputs,
};
use bevy_flock::behavior::blackboard::{Blackboard, BlackboardKey, Value};
use bevy_flock::behavior::registry::LeafBehavior;
use bevy_flock::behavior::tree::{LeafContext, Status};
use bevy_flock::domain::cost::VirtualCost;
use bevy_flock::domain::ids::NpcId;
use bevy_flock::domain::tick::Tick;

/// Set by the guard leaf while it is watching a door.
const WATCHING: BlackboardKey = BlackboardKey::new(60);

/// Stand at a door for a while, then go back to whatever you were doing.
///
/// The countdown lives in the blackboard, not in the leaf: one leaf instance serves
/// the entire crowd, and a counter in a field would give every NPC the same countdown.
struct WatchDoor {
    ticks: u64,
}

impl LeafBehavior for WatchDoor {
    fn tick(&self, ctx: &mut LeafContext<'_>) -> Status {
        let watched = ctx.number(WATCHING).map_or(0.0, |value| value.max(0.0)) as u64;
        if watched >= self.ticks {
            let _ = ctx.blackboard.remove(WATCHING);
            return Status::Success;
        }
        let _ = ctx
            .blackboard
            .set(WATCHING, Value::Float((watched + 1) as f32));
        Status::Running
    }

    fn abort(&self, ctx: &mut LeafContext<'_>) {
        // Releasing state on abort is what stops an NPC resuming a stale watch.
        let _ = ctx.blackboard.remove(WATCHING);
    }

    fn cost_hint(&self) -> VirtualCost {
        // Declared low because it is measured low: three slot operations. An
        // undeclared leaf is charged the pessimistic default instead.
        VirtualCost::new(5)
    }

    fn label(&self) -> &'static str {
        "watch_door"
    }
}

/// A director that makes it rain, and then makes people cross about it.
struct RainDirector {
    mood: f32,
}

impl Director for RainDirector {
    fn decide(&mut self, inputs: &DirectorInputs, out: &mut DirectorCommands) {
        // Once a minute of simulated time, the weather has an opinion.
        if !inputs.tick.get().is_multiple_of(600) {
            return;
        }
        self.mood = (self.mood - 0.05).clamp(-1.0, 1.0);
        out.push(DirectorCommand::Mood {
            value: self.mood,
            ttl_ticks: 600,
        });
        if self.mood < -0.2 {
            out.push(DirectorCommand::Quiesce { ttl_ticks: 120 });
        }
    }

    fn label(&self) -> &'static str {
        "rain"
    }
}

fn main() {
    let leaf = WatchDoor { ticks: 3 };
    let mut blackboard = Blackboard::new();
    let npc = NpcId::unique(1);
    println!("watching a door for three ticks:");
    for tick in 0..5 {
        let mut ctx = LeafContext {
            blackboard: &mut blackboard,
            tick: Tick::new(tick),
            npc,
        };
        let status = leaf.tick(&mut ctx);
        println!(
            "  tick {tick}: {status:?} (watched {:?})",
            ctx.number(WATCHING)
        );
        if status == Status::Success {
            break;
        }
    }

    let mut director = RainDirector { mood: 0.0 };
    let mut board = DirectorBoard::new();
    println!("\nthe weather, over an hour:");
    for tick in 0..1_801_u64 {
        let inputs = DirectorInputs {
            tick: Tick::new(tick),
            population: 500,
            tiers: [100, 200, 150, 50],
            mean_mood: 0.0,
            tension: 0.0,
            alarms: 0,
            ticks_since_player: None,
        };
        let commands = board.decide(&mut director, &inputs);
        if !commands.is_empty() {
            for command in commands.as_slice() {
                println!("  tick {tick:>5}: {}", command.as_str());
            }
        }
    }
    println!(
        "applied {} commands, {} in the trace",
        board.applied(),
        board.trace().len()
    );
}
