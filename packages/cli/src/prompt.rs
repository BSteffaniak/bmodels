//! Terminal prompts for guided commands.
//!
//! Prompts go to stderr so stdout stays clean for pipes. When stdin or stderr is not a terminal
//! nothing prompts: callers get [`NotInteractive`] naming the flag to pass instead.

use std::io::{BufRead as _, IsTerminal as _, Read as _, Write as _};

use anyhow::Context as _;
use bmodels_model::Secret;

/// A choice was needed but there is no terminal to ask on.
#[derive(Debug, thiserror::Error)]
#[error("{what}; not running interactively, so {hint}")]
pub struct NotInteractive {
    what: String,
    hint: String,
}

/// Fails with [`NotInteractive`] unless prompting is possible.
pub fn require(what: &str, hint: &str) -> anyhow::Result<()> {
    if interactive() {
        Ok(())
    } else {
        Err(NotInteractive {
            what: what.to_owned(),
            hint: hint.to_owned(),
        }
        .into())
    }
}

/// Whether stdin and stderr are terminals.
pub fn interactive() -> bool {
    std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

/// Reads a secret without echoing it.
pub fn hidden(prompt: &str) -> anyhow::Result<Secret> {
    let value = zeroize::Zeroizing::new(
        rpassword::prompt_password(prompt).context("reading from the terminal")?,
    );
    let secret = Secret::new(value.as_str());
    anyhow::ensure!(!secret.is_empty(), "nothing entered");
    Ok(secret)
}

/// Reads a secret from stdin (all of it, trimmed).
pub fn stdin_secret() -> anyhow::Result<Secret> {
    let mut buf = zeroize::Zeroizing::new(String::new());
    std::io::stdin()
        .read_to_string(&mut buf)
        .context("reading stdin")?;
    let secret = Secret::new(buf.as_str());
    anyhow::ensure!(!secret.is_empty(), "stdin was empty");
    Ok(secret)
}

/// Asks a yes/no question.
pub fn confirm(question: &str, default: bool) -> anyhow::Result<bool> {
    let hint = if default { "Y/n" } else { "y/N" };
    eprint!("{question} [{hint}] ");
    std::io::stderr().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(match line.trim().to_ascii_lowercase().as_str() {
        "" => default,
        "y" | "yes" => true,
        _ => false,
    })
}

/// Asks the user to pick one of `labels` by number.
pub fn choose(title: &str, labels: &[String]) -> anyhow::Result<usize> {
    anyhow::ensure!(!labels.is_empty(), "{title}: nothing to choose from");
    eprintln!("{title}");
    for (i, label) in labels.iter().enumerate() {
        eprintln!("  {:>2}) {label}", i + 1);
    }
    loop {
        eprint!("Choose 1-{}: ", labels.len());
        std::io::stderr().flush()?;
        let mut line = String::new();
        if std::io::stdin().lock().read_line(&mut line)? == 0 {
            anyhow::bail!("cancelled");
        }
        if let Ok(n) = line.trim().parse::<usize>()
            && (1..=labels.len()).contains(&n)
        {
            return Ok(n - 1);
        }
    }
}
