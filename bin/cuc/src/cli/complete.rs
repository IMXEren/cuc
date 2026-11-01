use clap::Args;
use std::{
    process::{Command, Stdio, exit},
    sync::LazyLock,
};

use crate::mbase64;

static COMPLETER_TERA: LazyLock<tera::Tera> = LazyLock::new(|| {
    let mut tera = tera::Tera::default();
    tera.register_filter(
        "shell_quote",
        |value: &tera::Value, _: tera::Kwargs, _: &tera::State| -> tera::TeraResult<String> {
            let word = value
                .as_str()
                .ok_or_else(|| tera::Error::message("shell_quote expects a string"))?;
            Ok(shell_words::quote(word).into_owned())
        },
    );
    tera.register_filter(
        "shell_join",
        |value: &tera::Value, _: tera::Kwargs, _: &tera::State| -> tera::TeraResult<String> {
            let words = value
                .as_array()
                .ok_or_else(|| tera::Error::message("shell_join expects a list of strings"))?
                .iter()
                .map(|word| {
                    word.as_str()
                        .ok_or_else(|| tera::Error::message("shell_join expects a list of strings"))
                })
                .collect::<tera::TeraResult<Vec<_>>>()?;
            Ok(shell_words::join(words))
        },
    );
    tera
});

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
        let decoded_line = mbase64::decode(&self.line)?;
        let mut words = winsplit::split(&decoded_line);
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
        Ok(COMPLETER_TERA.render_str(run_template, &context, false)?)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_usage_shell_filters_without_evaluating_word_contents() {
        let complete = Complete {
            current: 2,
            line: String::new(),
            shell: String::new(),
            args: Vec::new(),
        };
        let mut words = vec!["mise".into(), "use".into(), "a' $(exit 1)".into()];
        let quoted = complete
            .render_run(
                "printf '%s\\n' {{ words[CURRENT] | shell_quote }}",
                &mut words,
            )
            .unwrap();
        assert_eq!(
            quoted,
            format!("printf '%s\\n' {}", shell_words::quote(&words[2]))
        );

        let joined = complete
            .render_run("{{ words | shell_join | shell_quote }}", &mut words)
            .unwrap();
        assert_eq!(joined, shell_words::quote(&shell_words::join(&words)));
    }
}
