//! Global settings loaded from `~/.mcp/settings`.
//!
//! ```text
//! timeout 420
//! ```

use anyhow::{bail, Context, Result};
use std::fs;
use std::time::Duration;

pub const DEFAULT_TIMEOUT_SECONDS: i64 = 420;

pub fn timeout() -> Result<Option<Duration>> {
    let home = dirs::home_dir().context("could not determine home directory")?;
    let path = home.join(".mcp/settings");
    if !path.exists() {
        return seconds_to_duration(DEFAULT_TIMEOUT_SECONDS);
    }

    let contents = fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    parse(&contents, &path.display().to_string())
}

fn parse(contents: &str, source: &str) -> Result<Option<Duration>> {
    let mut timeout = DEFAULT_TIMEOUT_SECONDS;
    for (index, raw) in contents.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.splitn(2, char::is_whitespace);
        let key = parts.next().unwrap_or("");
        let value = parts.next().unwrap_or("").trim();
        if key != "timeout" {
            bail!("{source}:{}: unknown setting '{key}'", index + 1);
        }
        timeout = value.parse().with_context(|| format!("{source}:{}: timeout must be an integer", index + 1))?;
    }
    seconds_to_duration(timeout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uses_the_seven_minute_default() {
        assert_eq!(seconds_to_duration(DEFAULT_TIMEOUT_SECONDS).unwrap(), Some(Duration::from_secs(420)));
    }

    #[test]
    fn parses_timeout_and_no_timeout_settings() {
        assert_eq!(parse("timeout 60\n", "test").unwrap(), Some(Duration::from_secs(60)));
        assert_eq!(parse("timeout -1\n", "test").unwrap(), None);
    }

    #[test]
    fn rejects_unknown_or_invalid_settings() {
        assert!(parse("other 1\n", "test").is_err());
        assert!(parse("timeout -2\n", "test").is_err());
    }
}

pub fn seconds_to_duration(seconds: i64) -> Result<Option<Duration>> {
    match seconds {
        -1 => Ok(None),
        0.. => Ok(Some(Duration::from_secs(seconds as u64))),
        _ => bail!("timeout must be -1 (no timeout) or a non-negative number of seconds"),
    }
}
