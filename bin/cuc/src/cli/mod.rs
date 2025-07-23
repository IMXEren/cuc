use clap::{Parser, Subcommand};

mod complete;
mod generate;
mod last_modified;
mod usage;

#[derive(Debug, Parser)]
#[clap(
    name = "cuc",
    author,
    version,
    about,
    long_about,
    arg_required_else_help = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    Generate(generate::Generate),
    Complete(complete::Complete),
    Usage(usage::Usage),
    LastModified(last_modified::LastModified),
}

impl Commands {
    pub fn run(self) -> anyhow::Result<()> {
        let result = match self {
            Commands::Generate(cmd) => cmd.run()?,
            Commands::Complete(cmd) => cmd.run()?,
            Commands::Usage(cmd) => cmd.run()?,
            Commands::LastModified(cmd) => cmd.run()?,
        };
        Ok(result)
    }
}
