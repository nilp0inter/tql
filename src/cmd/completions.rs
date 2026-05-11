//! `tql completions <shell>` — emit a shell completion script to stdout.
//!
//! Operators install completions by piping the output into the appropriate
//! shell-specific location, e.g.
//!   tql completions bash > /etc/bash_completion.d/tql
//!   tql completions zsh  > ~/.zsh/completions/_tql
//!   tql completions fish > ~/.config/fish/completions/tql.fish

use std::io;

use clap::{CommandFactory, Parser};
use clap_complete::{generate, Shell};

#[derive(Parser, Debug)]
pub struct Args {
    /// Target shell.
    #[arg(value_enum)]
    pub shell: Shell,
}

pub fn run(args: Args) -> Result<(), u8> {
    let mut cmd = crate::Cli::command();
    let bin_name = cmd.get_name().to_string();
    generate(args.shell, &mut cmd, bin_name, &mut io::stdout());
    Ok(())
}

#[cfg(test)]
pub fn render(shell: Shell) -> String {
    let mut cmd = crate::Cli::command();
    let bin_name = cmd.get_name().to_string();
    let mut buf: Vec<u8> = Vec::new();
    generate(shell, &mut cmd, bin_name, &mut buf);
    String::from_utf8(buf).expect("clap_complete emits valid UTF-8")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bash_completion_mentions_binary_name_and_subcommand() {
        let out = render(Shell::Bash);
        assert!(out.contains("tql"));
        assert!(out.contains("sidecar"));
    }

    #[test]
    fn zsh_completion_is_nonempty() {
        let out = render(Shell::Zsh);
        assert!(out.len() > 200);
        assert!(out.contains("tql"));
    }

    #[test]
    fn fish_completion_is_nonempty() {
        let out = render(Shell::Fish);
        assert!(!out.is_empty());
        assert!(out.contains("tql"));
    }
}
