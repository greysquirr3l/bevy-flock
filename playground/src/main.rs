//! `ashfall-playground`: open the window and hand over to the library.
//!
//! Everything that can be tested lives in the library: the flag parsing, the scene conversion, the
//! camera. This file is the exit code.

use bevy_flock_playground::{Options, Request, USAGE, run};
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
                eprintln!("ashfall-playground: {error}");
                ExitCode::FAILURE
            }
        },
        Err(error) => {
            eprintln!("ashfall-playground: {error}\n\n{USAGE}");
            ExitCode::from(2)
        }
    }
}
