# Behaviour

Four ways to decide what an NPC does, in increasing order of cost. All of them are
extension points; the crate supplies the machinery, you supply the judgement.

| Mechanism | Good at | Cost |
| --- | --- | --- |
| Behaviour tree | sequences with state ("walk there, then look around") | one leaf per tick |
| Utility scoring | choosing among goals ("eat, sleep, work, flee") | all considerations, once |
| State machine | modes with dwell times ("patrol ↔ alert") | a comparison |
| World director | decisions that are not any NPC's business (alarms, mood, events) | one pass per tick |

They compose: a tree can be a utility option, a state machine can own which tree is running,
and the director can tell everyone that it is raining.

## Behaviour trees

A tree is authored as data or built in code, then *compiled* into a flat node array with
child ranges. Ticking is a cursor walk over that array: no allocation, no recursion, no
`Box<dyn Node>` per node.

```rust
use bevy_flock::prelude::*;

// Register the catalogue once; every field is a `LeafId` a tree can name.
let leaves = registry.register_standard_leaves()?;
let spec = selector(vec![
    sequence(vec![action(leaves.talk_to), action(leaves.idle)]),
    action(leaves.wander),
]);
let tree = BehaviorTree::try_from_spec(spec)?;
```

The node set (ADR 0035):

- **Composites:** `selector` (first success), `sequence` (first failure), `parallel` (all
  children, policy on failure count), `random_selector`, `random_sequence`, `if_else`.
- **Decorators:** `invert`, `succeed` (always succeed), `fail` (always fail), `cooldown`
  (cannot re-run until a tick), `timeout` (fails after a tick budget), `repeat`.
- **Leaves:** anything implementing `LeafBehavior`, plus the `StandardLeaves` catalogue.

A tick returns `Status::{Success, Failure, Running}`. `Running` is the important one: it is
how a tree says "I am still doing that thing", and the cursor stays on that branch until the
leaf finishes or a higher-priority branch preempts it.

Three properties the tests pin:

- **A tick never allocates** (`tests/tree_no_alloc.rs` counts allocations with a custom global
  allocator).
- **Depth is bounded at compile time**, a tree deeper than `MAX_DEPTH` fails to compile with
  an error naming the depth, rather than blowing the stack at runtime.
- **A panic in a leaf is a panic**, not a silently failed branch. Leaf authors get to be
  wrong loudly.

### Authored trees

`NodeSpec::from_ron` loads a tree from text, and the loader is fuzzed (`fuzz/fuzz_targets/tree_ron.rs`):

```ron
Selector([
    Sequence([Action(0), Cooldown(seconds: 4.0, child: Action(1))]),
    Invert(Action(2)),
])
```

An authored tree that names a leaf the registry does not know is an error at load, with the
name and the position. That is the difference between "the guard does nothing" and "the guard
does nothing and the log says why".

## Senses: losing sight of someone

Encounter perception records who met whom, which is what social memory needs. A guard that
searches needs something else: a memory of *seeing*, with an age (ADR 0047).

```rust,ignore
use bevy_flock::prelude::*;

// A player-shaped emitter, refreshed every tick it is visible.
commands.spawn((StimulusSource::sight(30.0, 1.0), Transform::from_translation(player)));

// Every NPC that should notice it is an observer.
commands.spawn((NpcBundle::default_for(id), Perception, PerceivedStimuli::default()));
```

`PerceivedStimuli` is rewritten on the perception interval (six ticks by default) and is a
snapshot, so a tree can read it repeatedly during one think without the world changing under it.
The guard's search falls out of the data:

```rust,ignore
// "Chase while I can see them; search where I last saw them when I cannot."
selector((
    sequence((
        condition(|ctx| ctx.sees::<Player>()),
        action(StandardLeaves::Pursue),
    )),
    sequence((
        condition(|ctx| ctx.remembers::<Player>()),   // the sighting is still live
        action(StandardLeaves::SearchLastKnown),
    )),
    action(StandardLeaves::Patrol),
))
```

Hearing works the same way with `StimulusSource::sound`, which is not narrowed by the sight cone
and ignores occlusion: a shout through a wall is still a shout. Anything the tree needs beyond
that, "is it my team", "is it the alarm bell", is a filter at the leaf, which keeps the hot loop
free of per-game predicates.

## Utility scoring

When the choice is "what should I want", trees get awkward: priorities interleave, and adding
a goal means editing every branch. Utility scoring answers with arithmetic instead (ADR 0036).

```rust
use bevy_flock::behavior::leaves::KEY_ALARM;
use bevy_flock::behavior::utility::{
    BehaviorId, Consideration, Curve, Input, UtilityEntry, UtilitySet,
};

let mut set = UtilitySet::new();
// Each consideration reads one input (a blackboard slot or a constant) and shapes it.
set.push(Consideration::new(Input::Key(KEY_ALARM)).with_curve(Curve::exponential(2.0)))?;
set.push(Consideration::new(Input::Constant(0.8)).with_curve(Curve::linear()))?;
let entry = UtilityEntry::new(BehaviorId::new(0), "flee", set);
let score = entry.set.score(&blackboard);
```

- **Curves** shape a raw input into an opinion: linear, exponential, inverse, logistic, step.
- **Inhibition** lets one option *cancel* another rather than merely outscore it, a
  "flee" option with an inhibition against "eat" means a frightened NPC does not eat, no
  matter how hungry.
- **Count compensation** divides an option's score by how many NPCs already chose it, which
  is what stops a hundred people converging on one chair.
- **`UtilityTable::explain`** returns the arithmetic: every consideration, its raw value, its
  curved value, the product, the inhibition and the final score. When the NPC does something
  odd, this is the answer.

## State machines

A guarded state machine with explicit dwell times, for the cases where the shape is a mode
rather than a plan:

```rust
use bevy_flock::behavior::state_machine::{always, StateMachine};
use bevy_flock::prelude::*;

let machine = StateMachine::new(Mode::Patrol, Tick::ZERO)
    // A guard is a boxed closure over the blackboard; `always()` and `never()` exist too.
    .transition(Mode::Patrol, Mode::Alert, Box::new(|board| board.get(KEY_ALARM).is_some()))
    // Six hundred ticks of dwell before it is even allowed back to patrol.
    .transition_after(Mode::Alert, Mode::Patrol, 600, always());
```

Guards are closures over the blackboard, transitions are checked in order, and dwell times
mean a state cannot be re-entered at a frequency that would thrash a cooldown.

## The director

Some decisions belong to the world, not to any NPC: an alarm, a festival, a plague, a change
of mood. `WorldDirector` is the seam:

```rust
use bevy_flock::application::director::{Director, DirectorCommands, DirectorInputs, DirectorCommand};

struct Raid;
impl Director for Raid {
    fn decide(&mut self, inputs: &DirectorInputs<'_>, out: &mut DirectorCommands) {
        if inputs.tick().raw() % 3_600 == 0 {
            // A command is a value: the board applies it, the trace records it.
            out.push(DirectorCommand::Mood { value: 0.2, ttl_ticks: 600 });
        }
    }

    fn label(&self) -> &'static str {
        "raid"
    }
}
```

The director *decides*; the board *applies*. Commands are values in a fixed-size batch (an
overflow is counted, not queued), the trace is bounded and ring-buffered, and a policy that
panics is caught and reported rather than taking the frame down. That split is what makes a crowd's mood replayable: feed the same decisions to the same
world and you get the same afternoon.

## Dialogue and barks

- **`DialogGraph`** is authored as RON, validated at load (dangling choice, unreachable node,
  a loop with no exit), and played as a session with a cursor. Facts gate choices.
- **Barks** are a separate, bounded channel: a queue with per-category cooldowns and a
  per-frame cap, so a market of four hundred people does not produce four hundred lines when
  the player walks in. Two, and the rest are dropped and counted.
- **`DialogProvider`** is the seam for a slow line source, a language model, a localiser, a
  table that lives on disk, with a reply queue drained on the tick, so latency never blocks
  a frame.

## Groups and factions

`domain::groups` gives a group a formation with stable slots (so the third member stays third
when the second dies), plus faction relations: ally, neutral, hostile. A tree asks the group
the same way it asks the blackboard, and the group answers for the whole formation.

## Personality that moves people

A temperament is not decoration. Every NPC carries a `Temperament` and four `Needs`, and the
steering pass scales that NPC's weights by what they say (ADR 0051):

```text
effective weight = SteeringParams weight x bias
```

The bias is centred on `1.0` and clamped to `0.25 ..= 4.0`, so a game's tuning stays the baseline
and a personality is a statement about that tuning rather than a replacement for it. Leaving the
knob alone gets you a crowd where a guard holds rank and an urchin scatters:

| Archetype | Separation | Alignment | Cohesion | Arrive |
| --- | --- | --- | --- | --- |
| Guard | 0.918 | 1.164 | 1.118 | 1.231 |
| Merchant | 0.991 | 1.061 | 1.237 | 1.000 |
| Urchin | 1.062 | 0.918 | 1.122 | 0.868 |
| Noble | 0.933 | 1.089 | 1.127 | 1.198 |
| Wanderer | 1.056 | 1.003 | 1.051 | 1.000 |

Unmet needs push the same way, harder, and they move on their own: `BoidsConfig::needs` decays
every `NeedsDecay` interval (eight ticks by default) at a rate the temperament sets, so a gregarious
NPC runs out of company faster than a hermit. A hungry or lonely NPC jostles more and clusters
harder, whatever its personality was.

```rust
# use bevy_app::App;
# use bevy_flock::prelude::*;
# use bevy_flock::boids::{BoidsConfig, BoidsPlugin, NeedsDecay};
let config = BoidsConfig::default()
    // Personality steers, but the game owns the needs economy.
    .with_needs_decay(None);

let mut app = App::new();
app.add_plugins(BoidsPlugin::new(config));
```

Turn the whole thing off with `BoidsConfig::default().with_temperament(None)`, which restores one
weight set for the crowd and costs nothing to have left in the build: the bias buffer stays empty
and the kernel reads the configured weights, as it always did.

## Where to look

| Want | Read |
| --- | --- |
| Node set and compiler | `src/behavior/tree.rs` |
| Standard leaves | `src/behavior/leaves.rs` |
| Archetype trees | `src/behavior/archetypes.rs` |
| Utility scoring | `src/behavior/utility.rs` |
| Director | `src/application/director.rs` |
| Dialogue | `src/behavior/dialog.rs` |
| Barks | `src/behavior/bark.rs` |
| Temperament and needs | `src/domain/temperament.rs` |
| Bias plumbing | `src/application/crowd.rs`, `src/boids.rs` |
