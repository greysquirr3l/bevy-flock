//! `skinned`: the phase-2 example, opened from a shell and handed to the library.
//!
//! Everything that can be tested lives in the library — the flags, the rig, the clips, the crowd and
//! the telemetry the run is asserted on. This file is the exit code.
use bevy_flock_playground::skinned::{Options, Request, USAGE, run};
use std::process::ExitCode;

fn main() -> ExitCode {
    match Options::parse(std::env::args().skip(1)) {
        Ok(Request::Help) => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Ok(Request::Run(options)) => match run(&options) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("skinned: {error}");
                ExitCode::FAILURE
            }
        },
        Err(error) => {
            eprintln!("skinned: {error}\n\n{USAGE}");
            ExitCode::from(2)
        }
    }
}
