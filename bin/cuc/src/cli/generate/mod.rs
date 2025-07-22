use clap::{Args, ValueHint};
use std::io::Write;
use std::{fs::OpenOptions, path::PathBuf};

use crate::{cli::generate::generator::Completor, spec::UsageSpecExt};

mod formatter;
mod generator;
use generator::{Generator, GeneratorView};

#[derive(Debug, Args)]
#[clap(about = "Generate clink argmatcher completions from the usage spec")]
pub struct Generate {
    #[arg(help = "Path to usage.spec.kdl. Reads file content from stdin if none provided.", value_hint = ValueHint::FilePath)]
    pub usage_spec: Option<PathBuf>,

    #[arg(
        long = "arg-matcher",
        help = "List of command names to generate the clink.argmatcher() for. Overrides the name in the usage spec."
    )]
    pub arg_matchers: Vec<String>,

    #[arg(
        short,
        long,
        value_name = "LUA",
        value_hint = ValueHint::FilePath,
        help = "Path to usage_completions.lua to write to, else write to stdout."
    )]
    pub out: Option<PathBuf>,

    #[arg(
        long,
        help = "Generate dynamic completion for args from 'complete' node."
    )]
    pub complete: bool,

    #[arg(
        long,
        help = "The shell that'll be used to run the completion command."
    )]
    pub shell: Option<PathBuf>,
}

impl Generate {
    pub fn run(self) -> anyhow::Result<()> {
        let usage_spec = cuc::usage::UsageSpec::load(self.usage_spec.as_ref())?;
        let mut genrtr = Generator::default();
        genrtr.spec = usage_spec;
        cuc::usage::UsageSpec::add_default_completes(&mut genrtr.spec.completes);
        if self.complete {
            genrtr.completor = Some(Completor {
                exe_path: std::env::current_exe()?,
                shell: self.find_shell()?,
            });
        }
        genrtr.arg_matchers = self.arg_matchers;

        let mut genv = GeneratorView {
            spec: &genrtr.spec,
            cached_functions: &mut genrtr.cached_functions,
            completor: genrtr.completor.as_ref(),
            arg_matchers: &genrtr.arg_matchers,
        };
        let usage_completions = genv.generate();
        if let Some(out) = self.out {
            let mut file = OpenOptions::new().create(true).write(true).open(&out)?;
            write!(file, "{}", usage_completions)?;
        } else {
            write!(std::io::stdout(), "{}", usage_completions)?;
        }
        Ok(())
    }

    fn find_shell(&self) -> anyhow::Result<PathBuf> {
        if let Some(ref path) = self.shell {
            return Ok(which::which(path)?.canonicalize()?);
        }

        let git_bin = which::which("git.exe")?;
        if let Some(git_bin_dir) = git_bin.parent() {
            match git_bin_dir.file_name().map(|ostr| ostr.to_str().unwrap()) {
                Some("cmd") => {
                    return Ok(which::which(git_bin_dir.join("..\\bin\\bash.exe"))?.canonicalize()?);
                }
                Some("bin") => {
                    return Ok(which::which(git_bin_dir.join("bash.exe"))?.canonicalize()?);
                }
                _ => {}
            };
        }
        anyhow::bail!("failed to find bash shell! Try again with inputting the shell flag");
    }
}
