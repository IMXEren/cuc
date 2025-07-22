use clap::Args;
use std::process::{Command, Stdio, exit};

use crate::mbase64;

#[derive(Debug, Args)]
#[clap(about = "Get completions by running the specified command as args")]
pub struct Complete {
    #[arg(
        long,
        help = "The index of the word currently being typed, combine with words to get the current word e.g. words[current]."
    )]
    pub current: usize,

    #[arg(long, help = "The current line in its entirety.")]
    pub line: String,

    #[arg(
        short,
        long,
        help = "Shell to use for completing the command.",
        long_help = "Shell to use for completing the command. 'jdx/usage' requires bash/fish/zsh to run the completor and get it's output. So, a workaround was to use git-bash as git can be commonly found."
    )]
    pub shell: String,

    #[arg(
        last = true,
        help = "List of args to run and output completions to stdout per line.",
        required = true
    )]
    pub args: Vec<String>,
}

impl Complete {
    pub fn run(self) -> anyhow::Result<()> {
        let mut words = winsplit::split(&self.line);
        let run_template = mbase64::decode(&self.args[0])?;
        let run_script = self.render_run(&run_template, &mut words)?;
        self.complete(&run_script)?;
        Ok(())
    }

    fn render_run(&self, run_template: &str, words: &mut Vec<String>) -> anyhow::Result<String> {
        let current = self.current;
        let prev = if self.current > 0 {
            self.current - 1
        } else {
            0
        };

        // If list of words is less than the (current index + 1) then
        // push empty strings to the list so, that the rendering doesn't fail
        if words.len() - 1 < current {
            words.reserve(current - words.len() + 1);
            while words.len() - 1 < current {
                words.push(String::new());
            }
        }

        let mut context = tera::Context::new();
        context.insert("words", words);
        context.insert("CURRENT", &current);
        context.insert("PREV", &prev);
        Ok(tera::Tera::one_off(run_template, &context, true)?)
    }

    fn complete<S>(&self, run: S) -> anyhow::Result<()>
    where
        S: AsRef<str>,
    {
        let mut child = Command::new(&self.shell)
            .arg("-c")
            .arg(run.as_ref())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()?;
        let status = child.wait()?;
        exit(status.code().unwrap_or(1));
    }
}
