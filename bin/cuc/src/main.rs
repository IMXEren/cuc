use std::process::exit;

use clap::{CommandFactory, Parser};

mod cli;
mod mbase64;
mod spec;
mod string;
use crate::cli::Cli;

fn main() {
    let cli = Cli::parse();
    if let Some(subcmd) = cli.command {
        match subcmd.run() {
            Err(e) => {
                eprintln!("[ERROR] {:?}", e);
                exit(1)
            }
            _ => {}
        }
        exit(0);
    }

    Cli::command().print_help().unwrap();
    exit(1);
}
