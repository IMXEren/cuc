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
        if let Err(e) = subcmd.run() {
            eprintln!("[ERROR] {:?}", e);
            exit(1)
        }
        exit(0);
    }

    Cli::command().print_help().unwrap();
    exit(1);
}
