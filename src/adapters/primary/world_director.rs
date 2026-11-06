//! The director, on the schedule.
//!
//! [`crate::application::director`] decides *what* to order; this module decides
//! *when* it runs and where the orders land in the world:
//!
//! * [`CrowdDirector`] owns the board, the policy and the last batch of commands.
//! * [`AmbientMood`] is the crowd-wide mood the director sets, which the utility
//!   scoring and the bark picker read.
//! * [`CrowdAlarms`] is the shared alarm field, so a game can ask "how afraid is
//!   someone standing here" without touching the director.
//!
//! Nothing here iterates the crowd. The director's inputs come from the pulse
//! (sampled once a second) and the spawn statistics, and its outputs are a handful
//! of commands; a crowd of a million costs the same to direct as a crowd of ten.

use crate::adapters::primary::observability::CrowdPulse;
use crate::application::SimClock;
use crate::application::director::{
    AmbientDirector, Director, DirectorBoard, DirectorCommand, DirectorCommands, DirectorInputs,
};
use crate::domain::tick::Tick;
use bevy_ecs::prelude::*;

/// How many ticks make an in-game hour by default.
pub const DEFAULT_TICKS_PER_HOUR: u64 = 600;

/// The crowd-wide mood the director is holding, in `-1..=1`.
///
/// Written by the director, read by anything that wants the mood of the *town* as
/// opposed to the mood of a person.
#[derive(Resource, Debug, Clone, Copy, PartialEq, Default)]
pub struct AmbientMood(pub f32);

/// The shared alarm field.
#[derive(Resource, Debug, Default)]
pub struct CrowdAlarms(pub crate::application::director::AlarmField);

/// The director, its board and its most recent orders.
#[derive(Resource)]
pub struct CrowdDirector {
    board: DirectorBoard,
    policy: Box<dyn Director + Send + Sync>,
    commands: DirectorCommands,
    ticks_per_hour: u64,
    decisions: u64,
}

impl core::fmt::Debug for CrowdDirector {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("CrowdDirector")
            .field("policy", &self.policy.label())
            .field("decisions", &self.decisions)
            .field("ticks_per_hour", &self.ticks_per_hour)
            .field("commands", &self.commands.as_slice())
            .finish_non_exhaustive()
    }
}

impl CrowdDirector {
    /// A director with an explicit policy.
    #[must_use]
    pub fn new(policy: Box<dyn Director + Send + Sync>, ticks_per_hour: u64) -> Self {
        Self {
            board: DirectorBoard::new(),
            policy,
            commands: DirectorCommands::new(),
            ticks_per_hour,
            decisions: 0,
        }
    }

    /// The default director: a town with a working day.
    #[must_use]
    pub fn ambient() -> Self {
        Self::new(
            Box::new(AmbientDirector::new(DEFAULT_TICKS_PER_HOUR)),
            DEFAULT_TICKS_PER_HOUR,
        )
    }

    /// Swap the policy without losing the board or the alarm field.
    pub fn set_policy(&mut self, policy: Box<dyn Director + Send + Sync>) {
        self.policy = policy;
    }

    /// How many ticks this director treats as an in-game hour.
    #[must_use]
    pub const fn ticks_per_hour(&self) -> u64 {
        self.ticks_per_hour
    }

    /// The policy's label.
    #[must_use]
    pub fn label(&self) -> &'static str {
        self.policy.label()
    }

    /// The orders issued most recently.
    #[must_use]
    pub const fn commands(&self) -> &DirectorCommands {
        &self.commands
    }

    /// How many decisions have been taken.
    #[must_use]
    pub const fn decisions(&self) -> u64 {
        self.decisions
    }

    /// The board, for the trace and the field.
    #[must_use]
    pub const fn board(&self) -> &DirectorBoard {
        &self.board
    }

    /// How afraid someone at a point should be.
    #[must_use]
    pub fn fear_at(&self, position: bevy_math::Vec3, now: Tick) -> f32 {
        self.board.fear_at(position, now)
    }

    /// Issue an order from game code — a scripted fire, a festival.
    pub fn order(&mut self, command: DirectorCommand, now: Tick) {
        self.board.apply(command, now);
        self.commands.push(command);
    }

    /// Advance the board's clock.
    pub fn tick(&mut self, now: Tick) {
        self.board.tick(now);
    }
}

impl Default for CrowdDirector {
    fn default() -> Self {
        Self::ambient()
    }
}

/// Build the inputs, let the director decide, and publish what it ordered.
pub fn run_world_director(
    clock: Res<SimClock>,
    pulse: Res<CrowdPulse>,
    stats: Res<crate::adapters::primary::plugin::FlockStats>,
    mut director: ResMut<CrowdDirector>,
    mut mood: ResMut<AmbientMood>,
    mut alarms: ResMut<CrowdAlarms>,
    mut diagnostics: ResMut<crate::adapters::primary::observability::Diagnostics>,
) {
    let inputs = DirectorInputs {
        tick: clock.tick,
        population: pulse.population,
        tiers: pulse.tiers,
        mean_mood: pulse.mean_mood,
        tension: pulse.tension,
        alarms: pulse.alarms.max(alarms.0.len() as u32),
        ticks_since_player: None,
    };
    // Split the borrow: the board and the policy are separate fields of one resource.
    let director = &mut *director;
    let commands = director.board.decide(director.policy.as_mut(), &inputs);
    if !commands.as_slice().is_empty() {
        director.decisions = director.decisions.saturating_add(1);
        diagnostics.commands = diagnostics
            .commands
            .saturating_add(commands.as_slice().len() as u64);
    }
    for command in commands.as_slice() {
        if let DirectorCommand::Mood {
            value,
            ttl_ticks: _,
        } = command
        {
            mood.0 = (*value).clamp(-1.0, 1.0);
        }
    }
    director.board.tick(clock.tick);
    alarms.0 = director.board.alarms().clone();
    // The pulse's alarm count feeds the next decision; writing it here keeps the
    // dependency one-way (director reads the pulse, publishes into it).
    director.commands = commands;
    let _ = &stats;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::primary::observability::Diagnostics;
    use crate::adapters::primary::plugin::{FlockConfig, FlockPlugin};
    use crate::application::director::{Beat, ScriptedDirector};
    use crate::testkit::OrDie;
    use bevy_app::App;
    use bevy_math::Vec3;

    fn app_with_script(beats: Vec<Beat>) -> App {
        let mut app = App::new();
        app.add_plugins(FlockPlugin::new(FlockConfig::default()));
        let script = ScriptedDirector::new(beats).or_die();
        app.insert_resource(CrowdDirector::new(Box::new(script), DEFAULT_TICKS_PER_HOUR));
        app
    }

    fn advance(app: &mut App, ticks: u64) {
        for _ in 0..ticks {
            app.update();
            let mut clock = app.world_mut().resource_mut::<SimClock>();
            clock.tick = clock.tick.next();
        }
    }

    #[test]
    fn a_scripted_alarm_reaches_the_field() {
        let mut app = app_with_script(vec![Beat {
            tick: Tick::new(2),
            command: DirectorCommand::Alarm {
                at: Vec3::ZERO,
                strength: 1.0,
                radius: 10.0,
            },
        }]);
        advance(&mut app, 4);
        let director = app.world().resource::<CrowdDirector>();
        assert!(director.decisions() >= 1);
        let fear = director.fear_at(Vec3::new(1.0, 0.0, 0.0), Tick::new(4));
        assert!(fear > 0.0, "the alarm never reached the field");
    }

    #[test]
    fn the_director_sets_the_ambient_mood() {
        let mut app = app_with_script(vec![Beat {
            tick: Tick::ZERO,
            command: DirectorCommand::Mood {
                value: -0.5,
                ttl_ticks: 600,
            },
        }]);
        advance(&mut app, 2);
        assert!(app.world().resource::<AmbientMood>().0 < 0.0);
    }

    #[test]
    fn commands_are_counted_for_diagnostics() {
        let mut app = app_with_script(vec![Beat {
            tick: Tick::ZERO,
            command: DirectorCommand::Spawn { count: 4 },
        }]);
        advance(&mut app, 2);
        assert!(app.world().resource::<Diagnostics>().commands >= 1);
    }

    #[test]
    fn game_code_can_raise_an_alarm_directly() {
        let mut app = app_with_script(Vec::new());
        let mut director = app.world_mut().resource_mut::<CrowdDirector>();
        director.order(
            DirectorCommand::Alarm {
                at: Vec3::ZERO,
                strength: 0.8,
                radius: 5.0,
            },
            Tick::ZERO,
        );
        assert!(director.fear_at(Vec3::ZERO, Tick::ZERO) > 0.5);
        assert_eq!(director.commands().as_slice().len(), 1);
    }

    #[test]
    fn the_default_policy_is_the_ambient_town() {
        let director = CrowdDirector::default();
        assert_eq!(director.label(), "ambient");
    }
}
