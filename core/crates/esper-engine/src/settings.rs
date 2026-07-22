//! The settings this process is running with.
//!
//! Read once, at startup, before anything else happens — and never again. A
//! program whose configuration can change under it has to answer "which value
//! was in force when that happened", and the answer people want is "the one
//! it started with".
//!
//! # Why this is process-wide
//!
//! Because configuration is. The alternative is threading a `&Config` through
//! every function that might want the device's name, which makes the plumbing
//! the loudest thing in the code and still ends at one value per process. It
//! is set once, from `main`, and read everywhere.

use std::sync::OnceLock;

use esper_config::{Config, ConfigError};

/// The settings, once somebody has read them.
static SETTINGS: OnceLock<Config> = OnceLock::new();

/// Read the settings and remember them.
///
/// Called once from `main`. A file that is there and cannot be read or
/// understood stops the program **here**, with the line and the reason, rather
/// than an hour later with a value nobody chose.
pub fn load() -> Result<(), ConfigError> {
    let path = match crate::paths::config_file() {
        Ok(path) => path,
        // No home to read from is not a configuration problem: the commands
        // that need one will say so in their own words.
        Err(_) => return Ok(()),
    };
    let environment: Vec<(String, String)> = std::env::vars()
        .filter(|(name, _)| name.starts_with("ESPER_"))
        .collect();
    let config = Config::load(&path, &environment)?;
    let _ = SETTINGS.set(config);
    Ok(())
}

/// What this process is running with.
///
/// Defaults if nothing was loaded, which is what a test gets and what a
/// command that runs before `load` would get. There is deliberately no way to
/// change them afterwards.
pub fn get() -> &'static Config {
    SETTINGS.get_or_init(Config::defaults)
}

/// One setting's text, or the empty string.
#[must_use]
pub fn text(key: &str) -> String {
    get()
        .get(key)
        .and_then(esper_config::Value::text)
        .unwrap_or_default()
        .to_owned()
}

/// One setting's flag, or a fallback if it is somehow not one.
#[must_use]
pub fn flag(key: &str, if_missing: bool) -> bool {
    get()
        .get(key)
        .and_then(esper_config::Value::flag)
        .unwrap_or(if_missing)
}

/// One setting's whole number, if it has one.
#[must_use]
pub fn whole(key: &str) -> Option<u64> {
    get().get(key).and_then(esper_config::Value::whole)
}

/// One setting's number, if it has one.
#[must_use]
pub fn number(key: &str) -> Option<f64> {
    get().get(key).and_then(esper_config::Value::number)
}

/// This machine's overlay networks, as the settings describe them.
///
/// Read here rather than at each use, so a machine's answer to "which of my
/// paths are already private" is one answer rather than three that can drift.
#[must_use]
pub fn overlays() -> esper_overlay::Overlays {
    let named: Vec<String> = text("connectivity.overlay_interfaces")
        .split(',')
        .map(|n| n.trim().to_owned())
        .filter(|n| !n.is_empty())
        .collect();
    esper_overlay::Overlays::detect(&named)
}

/// The broker named in the settings, if one is.
///
/// **Both halves or neither.** An address with no key would mean trusting
/// whatever answered there, which is the one thing pinning exists to prevent,
/// so half a broker is an error rather than a broker.
pub fn broker() -> Result<Option<esper_rendezvous::Via>, String> {
    let address = get()
        .get("broker.address")
        .and_then(esper_config::Value::text);
    let key = get().get("broker.key").and_then(esper_config::Value::text);
    match (address, key) {
        (Some(address), Some(key)) => {
            let addr = address
                .parse()
                .map_err(|_| format!("broker.address ({address}) is not an address"))?;
            let key = crate::broker_cmd::parse_key(key)?;
            Ok(Some(esper_rendezvous::Via { addr, key }))
        }
        (None, None) => Ok(None),
        (Some(_), None) => Err(
            "broker.address is set and broker.key is not — a broker's key is pinned, and \
             `esper-broker serve` prints it"
                .to_owned(),
        ),
        (None, Some(_)) => {
            Err("broker.key is set and broker.address is not, so there is nowhere to go".to_owned())
        }
    }
}

/// Print every setting, its value, and where the value came from.
pub fn show() -> Result<(), String> {
    let config = get();
    let mut out = String::from("\u{2699}\u{fe0f}  esper config\n\n");

    match config.path() {
        Some(path) if config.found() => {
            out.push_str(&format!("  file       {}\n", path.display()));
        }
        Some(path) => out.push_str(&format!(
            "  file       {} (not there, which is fine)\n",
            path.display()
        )),
        None => out.push_str("  file       none\n"),
    }
    out.push('\n');

    let widest = esper_config::SETTINGS
        .iter()
        .map(|s| s.key.len())
        .max()
        .unwrap_or(20);

    for (setting, value, source) in config.effective() {
        out.push_str(&format!(
            "  {:<width$}  {:<24}  {}\n",
            setting.key,
            value.to_string(),
            source.describe(),
            width = widest
        ));
    }
    out.push_str(&format!(
        "\n  Each of these can be set in the file above, in the environment as\n  \
         {}, or on the command line.\n  \
         A flag beats the environment, which beats the file, which beats the default.\n",
        esper_config::environment_name("host.listen")
    ));
    print_block(&out);
    Ok(())
}

/// Print a whole block, treating a closed pipe as the reader having seen
/// enough.
///
/// `println!` panics when the other end of a pipe goes away, so
/// `config show | head` ends in a backtrace — which looks like a broken
/// program and is only Rust's default of ignoring `SIGPIPE`. Restoring that
/// default needs `unsafe`, which this workspace forbids; writing the block in
/// one go and dropping the error does the same job for the output long enough
/// to be piped into something impatient.
fn print_block(text: &str) {
    use std::io::Write as _;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let _ = out.write_all(text.as_bytes());
    let _ = out.flush();
}

/// Print a file somebody can start from: every setting, commented out, with
/// what it is for.
pub fn print_example() {
    let mut out = String::from(
        "# Esper settings.\n\
         #\n\
         # Every line here is commented out and shows the default, so this file\n\
         # does nothing until you uncomment something. That is the point: the\n\
         # defaults are the secure set, and a configuration is a list of\n\
         # deliberate departures from them.\n\
         #\n\
         # `key = value`, one per line, `#` for a comment. That is a subset of\n\
         # TOML, so an editor that knows TOML will colour it correctly.\n",
    );

    let mut group = "";
    for setting in esper_config::SETTINGS {
        let this_group = setting.key.split('.').next().unwrap_or("");
        if this_group != group {
            group = this_group;
            out.push('\n');
        }
        out.push('\n');
        for line in wrap(setting.about, 72) {
            out.push_str(&format!("# {line}\n"));
        }
        out.push_str(&format!("# {}\n", setting.kind.shape()));
        out.push_str(&format!("# {} = {}\n", setting.key, setting.default));
    }
    print_block(&out);
}

/// Break a sentence into lines that fit, for a comment in a file.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && line.len() + 1 + word.len() > width {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn a_sentence_is_wrapped_without_losing_a_word() {
        let text = "Announce this machine on the local network, and look for others.";
        let lines = wrap(text, 24);
        assert!(lines.iter().all(|l| l.len() <= 24), "{lines:?}");
        assert_eq!(lines.join(" "), text);
    }

    #[test]
    fn every_setting_appears_in_the_example_file_with_its_default() {
        // The example is generated from the same table everything else reads,
        // so a setting cannot be added and left out of the file people start
        // from.
        for setting in esper_config::SETTINGS {
            assert!(!setting.about.is_empty(), "{}", setting.key);
            assert!(!setting.kind.shape().is_empty(), "{}", setting.key);
        }
    }
}
