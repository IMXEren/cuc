use clap::{Args, CommandFactory, ValueHint};
use std::{
    fs::OpenOptions,
    io::{self, Write},
    path::PathBuf,
};

use crate::cli::Cli;

#[derive(Debug, Args)]
#[clap(about = "Generate usage.kdl for this CLI itself")]
pub struct Usage {
    #[arg(
        short,
        long,
        value_name = "USAGE_KDL",
        value_hint = ValueHint::FilePath,
        help = "Path to usage.kdl to write to, else write to stdout."
    )]
    pub out: Option<PathBuf>,
}

impl Usage {
    pub fn run(self) -> Result<(), io::Error> {
        let mut cmd = Cli::command();
        let bin_name = cmd.get_bin_name().unwrap_or("cuc").to_string();
        eprintln!("Generating usage spec...");
        let mut buf: Box<dyn Write> = if let Some(path) = self.out {
            let file = OpenOptions::new().create(true).write(true).open(path)?;
            Box::new(file)
        } else {
            Box::new(std::io::stdout())
        };
        clap_usage::generate(&mut cmd, bin_name, &mut buf);
        Ok(())
    }
}
