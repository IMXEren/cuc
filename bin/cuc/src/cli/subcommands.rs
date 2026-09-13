use std::io::{Read, Write};

use clap::Args;

/// Print the subcommand names of a usage spec read from stdin.
///
/// Generated completions pipe a dynamic mount's discovery command into this, so
/// the mounted commands are discovered while completing instead of being frozen
/// into the generated file.
#[derive(Debug, Args)]
#[clap(about = "Print the subcommand names of a usage spec read from stdin")]
pub struct Subcommands {}

impl Subcommands {
    pub fn run(self) -> anyhow::Result<()> {
        let mut source = String::new();
        std::io::stdin().read_to_string(&mut source)?;
        // A mount that prints nothing means "no commands", not an error: that is
        // what a task runner returns in a directory without tasks.
        let spec = source.parse::<usage::Spec>()?;

        let stdout = std::io::stdout();
        let mut stdout = stdout.lock();
        for name in spec
            .cmd
            .subcommands
            .values()
            .filter(|cmd| !cmd.hide)
            .map(|cmd| &cmd.name)
        {
            writeln!(stdout, "{name}")?;
        }
        Ok(())
    }
}
