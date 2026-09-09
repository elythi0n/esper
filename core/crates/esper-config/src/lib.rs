//! Settings: what they are, where each came from, and what happens when one is
//! wrong.
//!
//! # Why this exists at all
//!
//! The shipped `systemd` unit runs `esper host` with no arguments, because a
//! service manager has nowhere to put them. Everything this program can be
//! told — which port, which screen, how much CPU, which broker — was a flag,
//! which means a machine that runs it as a service runs it with the defaults
//! and no way to say otherwise.
//!
//! # The rules
//!
//! **Flags beat the environment, the environment beats the file, and the file
//! beats the defaults.** In that order, always, and every value remembers
//! which of the four it came from — because "why is it doing that" is a
//! question about *where a value came from*, and a program that cannot answer
//! it makes somebody guess.
//!
//! **An empty file produces a working system.** Every setting has a default
//! and the defaults are the secure set, so a file is a list of deliberate
//! departures rather than a thing you must write before anything runs.
//!
//! **An unknown key is a startup error.** Silently ignoring one is how a
//! typo becomes a machine that has been running without the setting somebody
//! thought they had turned on — and a security setting nobody notices is off
//! is worse than one that was never offered.
//!
//! # The format
//!
//! `key = value`, one per line, `#` for a comment, dotted keys for grouping:
//!
//! ```text
//! # Where to listen. The default is every address on the standard port.
//! host.listen = "0.0.0.0:7645"
//! discovery.mdns = false
//! ```
//!
//! That is a **subset of TOML**, deliberately: it is a format people already
//! know, the parser for the subset is a hundred lines, and a file written now
//! is one a real TOML parser reads later if this ever needs sections, arrays
//! or dates. Hand-rolling the general case would be the mistake; hand-rolling
//! a subset that is forward-compatible with the general case is not.

#![forbid(unsafe_code)]

mod parse;
mod settings;
mod value;
mod write;

pub use parse::{ParseError, parse};
pub use settings::{Kind, SETTINGS, Setting};
pub use value::Value;
pub use write::{Changed, set_in, unset_in};

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Where a value came from.
///
/// Ordered by precedence, lowest first, so the highest wins by comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Source {
    /// Nobody said, so this is what the program chose.
    Default,
    /// From the configuration file.
    File,
    /// From the environment.
    Environment,
    /// From the command line.
    Flag,
}

impl Source {
    /// A phrase for a person reading `config show`.
    #[must_use]
    pub const fn describe(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::File => "config file",
            Self::Environment => "environment",
            Self::Flag => "command line",
        }
    }
}

/// What can go wrong reading settings.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The file could not be read.
    #[error("{path}: {source}")]
    Unreadable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// The file could not be understood.
    #[error("{path}, line {}: {}", .source.line, .source.what)]
    Malformed { path: PathBuf, source: ParseError },

    /// A key nobody knows.
    #[error("{where_from} sets {key:?}, which is not a setting{}", suggestion(.nearest))]
    Unknown {
        key: String,
        where_from: String,
        nearest: Option<&'static str>,
    },

    /// A value of the wrong shape.
    #[error("{where_from} sets {key} to {value:?}, which {why}")]
    BadValue {
        key: String,
        value: String,
        why: String,
        where_from: String,
    },
}

/// The tail of an unknown-key message.
fn suggestion(nearest: &Option<&'static str>) -> String {
    match nearest {
        Some(near) => format!(" — did you mean {near}?"),
        None => " — `config show` lists every setting".to_owned(),
    }
}

/// Every setting, resolved, with where each value came from.
#[derive(Debug, Clone)]
pub struct Config {
    values: BTreeMap<&'static str, (Value, Source)>,
    /// Where the file was looked for, whether or not it existed.
    path: Option<PathBuf>,
    /// Whether it was there.
    found: bool,
}

impl Config {
    /// Every setting at its default.
    #[must_use]
    pub fn defaults() -> Self {
        Self {
            values: SETTINGS
                .iter()
                .map(|s| (s.key, (s.default.clone(), Source::Default)))
                .collect(),
            path: None,
            found: false,
        }
    }

    /// Read a file, then the environment, over the defaults.
    ///
    /// A file that is not there is not an error: **an empty file produces a
    /// working system**, and so does no file at all. A file that is there and
    /// cannot be read or understood *is* an error, because somebody wrote it
    /// meaning something.
    pub fn load(path: &Path, environment: &[(String, String)]) -> Result<Self, ConfigError> {
        let mut config = Self::defaults();
        config.path = Some(path.to_path_buf());

        match std::fs::read_to_string(path) {
            Ok(text) => {
                config.found = true;
                let pairs = parse(&text).map_err(|source| ConfigError::Malformed {
                    path: path.to_path_buf(),
                    source,
                })?;
                for (key, raw, line) in pairs {
                    config.set(
                        &key,
                        &raw,
                        Source::File,
                        &format!("{}, line {line}", path.display()),
                    )?;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(ConfigError::Unreadable {
                    path: path.to_path_buf(),
                    source,
                });
            }
        }

        // **Derived from the key, not from a table.** `host.listen` is
        // `ESPER_HOST_LISTEN`, always, so a setting cannot exist in one place
        // and be spelled differently in the other.
        for (name, raw) in environment {
            let Some(key) = key_for_environment(name) else {
                continue;
            };
            config.set(&key, raw, Source::Environment, "the environment")?;
        }

        Ok(config)
    }

    /// Set one value, if it is a setting and the value fits.
    fn set(
        &mut self,
        key: &str,
        raw: &str,
        source: Source,
        where_from: &str,
    ) -> Result<(), ConfigError> {
        let Some(setting) = SETTINGS.iter().find(|s| s.key == key) else {
            return Err(ConfigError::Unknown {
                key: key.to_owned(),
                where_from: where_from.to_owned(),
                nearest: nearest_setting(key),
            });
        };
        let value = setting
            .kind
            .read(raw)
            .map_err(|why| ConfigError::BadValue {
                key: setting.key.to_owned(),
                value: raw.to_owned(),
                why,
                where_from: where_from.to_owned(),
            })?;
        self.values.insert(setting.key, (value, source));
        Ok(())
    }

    /// Put a value from the command line over whatever is there.
    ///
    /// Flags win, so this never has to check what it is replacing.
    pub fn from_flag(&mut self, key: &'static str, value: Value) {
        self.values.insert(key, (value, Source::Flag));
    }

    /// One setting's value.
    ///
    /// Panic-free by construction: every key in [`SETTINGS`] is present from
    /// [`Config::defaults`] onwards, and nothing can remove one.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.values.get(key).map(|(v, _)| v)
    }

    /// Where one setting's value came from.
    #[must_use]
    pub fn source(&self, key: &str) -> Option<Source> {
        self.values.get(key).map(|(_, s)| *s)
    }

    /// Every setting, in order, with its value and where it came from.
    pub fn effective(&self) -> impl Iterator<Item = (&'static Setting, &Value, Source)> {
        SETTINGS.iter().filter_map(move |s| {
            self.values
                .get(s.key)
                .map(|(value, source)| (s, value, *source))
        })
    }

    /// Where the file was looked for.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Whether there was one.
    #[must_use]
    pub const fn found(&self) -> bool {
        self.found
    }
}

/// The setting an environment variable names, if it names one.
///
/// `ESPER_HOST_LISTEN` is `host.listen`. Anything that is not `ESPER_`
/// followed by a known setting's shape is not ours to complain about: the
/// environment belongs to the whole machine, and `ESPER_HOME` is not a setting.
#[must_use]
pub fn key_for_environment(name: &str) -> Option<String> {
    let rest = name.strip_prefix("ESPER_")?;
    let candidate = rest.to_ascii_lowercase().replace('_', ".");
    // Only what a setting could be. `ESPER_HOME` becomes `home`, which is not
    // a setting and never will be — it says where the settings *are*.
    SETTINGS
        .iter()
        .find(|s| s.key == candidate)
        .map(|s| s.key.to_owned())
        .or_else(|| {
            // A dotted key whose own segments contain underscores —
            // `session.idle_timeout` — cannot be recovered by replacing every
            // underscore, so the known keys are matched against instead.
            SETTINGS
                .iter()
                .find(|s| environment_name(s.key) == name)
                .map(|s| s.key.to_owned())
        })
}

/// The environment variable one setting is read from.
#[must_use]
pub fn environment_name(key: &str) -> String {
    format!("ESPER_{}", key.replace('.', "_").to_ascii_uppercase())
}

/// The known setting most like this one, for a message about a typo.
fn nearest_setting(key: &str) -> Option<&'static str> {
    SETTINGS
        .iter()
        .map(|s| (s.key, distance(key, s.key)))
        // Only if it is *close*: a suggestion that is nothing like what
        // somebody typed is worse than none.
        .filter(|(_, d)| *d <= 3)
        .min_by_key(|(_, d)| *d)
        .map(|(key, _)| key)
}

/// Edit distance, for suggesting the key somebody meant.
///
/// The plain dynamic-programming one, written with a pair of rows rather than
/// a matrix because the only thing it is ever asked about is a setting's name.
fn distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut previous: Vec<usize> = (0..=b.len()).collect();

    for (i, ca) in a.iter().enumerate() {
        let mut current: Vec<usize> = Vec::with_capacity(b.len() + 1);
        current.push(i + 1);
        for (j, cb) in b.iter().enumerate() {
            let replace = previous.get(j).copied().unwrap_or(0) + usize::from(ca != cb);
            let delete = previous.get(j + 1).copied().unwrap_or(0) + 1;
            let insert = current.get(j).copied().unwrap_or(0) + 1;
            current.push(replace.min(delete).min(insert));
        }
        previous = current;
    }
    previous.last().copied().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    /// A file in a directory that goes away with the test.
    fn with_file(text: &str) -> (tempdir::Dir, PathBuf) {
        let dir = tempdir::Dir::new();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, text).expect("write");
        (dir, path)
    }

    fn env(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn no_file_at_all_is_a_working_system() {
        // An empty file produces a working system, and so does no file: a
        // configuration is a list of deliberate departures, not a thing you
        // must write before anything runs.
        let dir = tempdir::Dir::new();
        let config = Config::load(&dir.path().join("nothing.toml"), &[]).expect("no file is fine");
        assert!(!config.found());
        assert_eq!(config.source("host.fps"), Some(Source::Default));
        assert_eq!(config.get("host.fps").and_then(Value::whole), Some(30));
    }

    #[test]
    fn the_order_is_flags_then_environment_then_file_then_defaults() {
        let (_dir, path) = with_file("host.fps = 45\ndiscovery.mdns = false\n");
        let mut config = Config::load(&path, &env(&[("ESPER_HOST_FPS", "60")])).expect("load");

        // The environment beat the file.
        assert_eq!(config.get("host.fps").and_then(Value::whole), Some(60));
        assert_eq!(config.source("host.fps"), Some(Source::Environment));

        // The file beat the default, where the environment said nothing.
        assert_eq!(
            config.get("discovery.mdns").and_then(Value::flag),
            Some(false)
        );
        assert_eq!(config.source("discovery.mdns"), Some(Source::File));

        // And a flag beats both.
        config.from_flag("host.fps", Value::Whole(24));
        assert_eq!(config.get("host.fps").and_then(Value::whole), Some(24));
        assert_eq!(config.source("host.fps"), Some(Source::Flag));
    }

    #[test]
    fn an_unknown_key_in_a_file_is_a_startup_error_that_says_where() {
        // Silently ignoring it is how a typo becomes a machine running
        // without the setting somebody thought they had turned on.
        let (_dir, path) = with_file("host.fsp = 30\n");
        let e = Config::load(&path, &[]).expect_err("a typo is an error");
        let said = e.to_string();
        assert!(said.contains("host.fsp"), "{said}");
        assert!(said.contains("line 1"), "{said}");
        assert!(said.contains("did you mean host.fps"), "{said}");
    }

    #[test]
    fn an_unknown_key_with_no_near_match_says_where_to_look() {
        let (_dir, path) = with_file("colour.scheme = \"dark\"\n");
        let e = Config::load(&path, &[]).expect_err("not a setting");
        assert!(e.to_string().contains("config show"), "{e}");
    }

    #[test]
    fn a_value_of_the_wrong_shape_names_the_key_the_value_and_why() {
        let (_dir, path) = with_file("host.listen = \"7645\"\n");
        let e = Config::load(&path, &[]).expect_err("not an address");
        let said = e.to_string();
        assert!(said.contains("host.listen"), "{said}");
        assert!(said.contains("7645"), "{said}");
        assert!(said.contains("host:port"), "{said}");
    }

    #[test]
    fn the_environment_is_derived_from_the_key_rather_than_from_a_table() {
        // A setting cannot exist in one place and be spelled differently in
        // the other, because there is no second list to disagree with.
        for setting in SETTINGS {
            let name = environment_name(setting.key);
            assert_eq!(
                key_for_environment(&name).as_deref(),
                Some(setting.key),
                "{name} should be {}",
                setting.key
            );
        }
        // Including the one whose own segment has an underscore in it, which
        // is where a naive replacement goes wrong.
        assert_eq!(
            key_for_environment("ESPER_SESSION_IDLE_TIMEOUT").as_deref(),
            Some("session.idle_timeout")
        );
    }

    #[test]
    fn the_environment_around_us_is_not_ours_to_complain_about() {
        // `ESPER_HOME` says where the settings are; it is not one of them.
        // And a variable belonging to something else entirely is ignored
        // rather than refused.
        assert_eq!(key_for_environment("ESPER_HOME"), None);
        assert_eq!(key_for_environment("PATH"), None);
        assert_eq!(key_for_environment("ESPER_DISPLAY_TESTS"), None);

        let dir = tempdir::Dir::new();
        Config::load(
            &dir.path().join("none.toml"),
            &env(&[("ESPER_HOME", "/somewhere"), ("PATH", "/bin")]),
        )
        .expect("an unrelated environment is not an error");
    }

    #[test]
    fn an_unknown_esper_variable_that_looks_like_a_setting_is_still_ignored() {
        // The environment is shared with the whole machine. Refusing to start
        // because something else set `ESPER_ANYTHING` would make this program
        // everybody else's problem.
        let dir = tempdir::Dir::new();
        Config::load(
            &dir.path().join("none.toml"),
            &env(&[("ESPER_NOT_A_SETTING", "1")]),
        )
        .expect("ignored");
    }

    #[test]
    fn a_bad_value_in_the_environment_says_it_came_from_there() {
        let dir = tempdir::Dir::new();
        let e = Config::load(
            &dir.path().join("none.toml"),
            &env(&[("ESPER_HOST_FPS", "lots")]),
        )
        .expect_err("not a number");
        assert!(e.to_string().contains("environment"), "{e}");
    }

    #[test]
    fn a_file_that_cannot_be_understood_says_which_line() {
        let (_dir, path) = with_file("host.fps = 30\nthis is not a setting\n");
        let e = Config::load(&path, &[]).expect_err("malformed");
        assert!(e.to_string().contains("line 2"), "{e}");
    }

    #[test]
    fn every_setting_is_shown_with_where_it_came_from() {
        let (_dir, path) = with_file("host.fps = 45\n");
        let config = Config::load(&path, &[]).expect("load");
        let shown: Vec<_> = config.effective().collect();
        assert_eq!(
            shown.len(),
            SETTINGS.len(),
            "every setting is accounted for"
        );
        let (_, value, source) = shown
            .iter()
            .find(|(s, _, _)| s.key == "host.fps")
            .expect("host.fps");
        assert_eq!(value.whole(), Some(45));
        assert_eq!(*source, Source::File);
    }

    /// A directory that removes itself, so these tests leave nothing behind.
    mod tempdir {
        use std::path::{Path, PathBuf};

        #[derive(Debug)]
        pub struct Dir(PathBuf);

        impl Dir {
            pub fn new() -> Self {
                let mut path = std::env::temp_dir();
                let stamp = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0);
                path.push(format!(
                    "esper-config-{stamp}-{:?}",
                    std::thread::current().id()
                ));
                std::fs::create_dir_all(&path).expect("mkdir");
                Self(path)
            }

            pub fn path(&self) -> &Path {
                &self.0
            }
        }

        impl Drop for Dir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }
}
