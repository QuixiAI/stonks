//! Crontab management. The stonks entry lives between two marker comments so
//! it can be replaced or removed without touching the rest of the crontab.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, bail};

use crate::config::Config;

const BEGIN: &str = "# >>> stonks (managed by `stonks cron`; do not edit) >>>";
const END: &str = "# <<< stonks <<<";

/// Installs (or replaces) the entry and returns the crontab line.
pub fn install(config: &Config, config_path: &Path) -> anyhow::Result<String> {
    let exe = std::env::current_exe().context("cannot locate the stonks binary")?;
    let config_path = std::path::absolute(config_path)?;
    if let Some(dir) = config.paths.log.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let line = format!(
        "{} {} --config {} run >> {} 2>&1",
        config.cron.schedule,
        quote(&exe),
        quote(&config_path),
        quote(&config.paths.log),
    );
    let (mut rest, _) = split(&read()?);
    rest.extend([BEGIN.to_owned(), line.clone(), END.to_owned()]);
    write(&rest)?;
    Ok(line)
}

/// Removes the entry. Returns whether one was present.
pub fn uninstall() -> anyhow::Result<bool> {
    let (rest, ours) = split(&read()?);
    if ours.is_empty() {
        return Ok(false);
    }
    write(&rest)?;
    Ok(true)
}

/// The installed crontab line, if any.
pub fn installed() -> anyhow::Result<Option<String>> {
    Ok(split(&read()?).1.into_iter().next())
}

/// Splits a crontab into (lines that are not ours, lines inside our block).
fn split(crontab: &str) -> (Vec<String>, Vec<String>) {
    let (mut rest, mut ours, mut inside) = (Vec::new(), Vec::new(), false);
    for line in crontab.lines() {
        match line {
            BEGIN => inside = true,
            END => inside = false,
            _ if inside => ours.push(line.to_owned()),
            _ => rest.push(line.to_owned()),
        }
    }
    (rest, ours)
}

fn read() -> anyhow::Result<String> {
    let out = Command::new("crontab").arg("-l").output().context("cannot run crontab")?;
    // `crontab -l` exits non-zero when the user has no crontab yet.
    Ok(if out.status.success() { String::from_utf8(out.stdout)? } else { String::new() })
}

fn write(lines: &[String]) -> anyhow::Result<()> {
    let mut child = Command::new("crontab")
        .arg("-")
        .stdin(Stdio::piped())
        .spawn()
        .context("cannot run crontab")?;
    let mut text = lines.join("\n");
    text.push('\n');
    child.stdin.take().context("crontab stdin")?.write_all(text.as_bytes())?;
    if !child.wait()?.success() {
        bail!("crontab rejected the new table");
    }
    Ok(())
}

/// Single-quotes a path for the shell cron runs, and escapes `%`, which cron
/// would otherwise turn into a newline.
fn quote(path: &Path) -> String {
    let escaped = path.display().to_string().replace('\'', r"'\''").replace('%', r"\%");
    format!("'{escaped}'")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_separates_managed_block() {
        let table = format!("0 1 * * * other\n{BEGIN}\n*/15 * * * 1-5 stonks run\n{END}\n# tail");
        let (rest, ours) = split(&table);
        assert_eq!(rest, ["0 1 * * * other", "# tail"]);
        assert_eq!(ours, ["*/15 * * * 1-5 stonks run"]);
    }
}
