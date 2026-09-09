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
    say_if_others_can_read_the_key(&path, &config);
    let _ = SETTINGS.set(config);
    Ok(())
}

/*
Say when a file holding a credential is readable by everybody.

`broker.key` is a key: whoever has it can be introduced by that broker as this
machine. The file is written 0600 by `config set`, but it is a file people also
write by hand, copy between machines, and check into places — and a mode of
0644 is invisible until somebody looks.

# Why this says something rather than refusing

Because refusing is a machine that will not start, over a file that has been
loading. The API token refuses, and is right to: a token *is* the credential
for the local control plane and there is nothing else it could be. This file
usually holds no credential at all, so the honest answer is to say so when it
does, once, where whoever starts the machine sees it.

Quiet where there is no key in it, because a line about the mode of a file
holding six ordinary settings is a line that teaches people to ignore lines.
*/
/// Whether this file holds a key and lets anybody read it.
///
/// Separate from the printing so a test can ask the question: what matters is
/// which of the three cases is the noisy one, and a test that captured stderr
/// would be testing the terminal.
#[cfg(unix)]
fn holds_a_readable_key(path: &std::path::Path, config: &Config) -> bool {
    use std::os::unix::fs::PermissionsExt as _;

    let holds_a_key = config
        .source("broker.key")
        .is_some_and(|from| matches!(from, esper_config::Source::File));
    if !holds_a_key {
        return false;
    }
    std::fs::metadata(path)
        .map(|m| m.permissions().mode() & 0o077 != 0)
        .unwrap_or(false)
}

#[cfg(unix)]
fn say_if_others_can_read_the_key(path: &std::path::Path, config: &Config) {
    use std::os::unix::fs::PermissionsExt as _;

    if !holds_a_readable_key(path, config) {
        return;
    }
    eprintln!(
        "{}: {} holds broker.key and is readable by others (mode {:04o}). \
         Whoever can read it can be introduced as this machine. Run `chmod 600` on it.",
        crate::net::me(),
        path.display(),
        std::fs::metadata(path)
            .map(|m| m.permissions().mode() & 0o777)
            .unwrap_or(0)
    );
}

#[cfg(not(unix))]
fn say_if_others_can_read_the_key(_path: &std::path::Path, _config: &Config) {}

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
pub fn overlays() -> esper_interfaces::Overlays {
    let named: Vec<String> = text("connectivity.overlay_interfaces")
        .split(',')
        .map(|n| n.trim().to_owned())
        .filter(|n| !n.is_empty())
        .collect();
    esper_interfaces::Overlays::detect(&named)
}

/// The interfaces to announce this machine on.
///
/// What somebody named, or every ordinary local network — and an empty answer
/// is a real one: the caller lets the system choose, which is what this did
/// before there was anything to choose between.
#[must_use]
pub fn mdns_interfaces() -> Vec<std::net::Ipv4Addr> {
    let named: Vec<String> = text("discovery.mdns_interfaces")
        .split(',')
        .map(|n| n.trim().to_owned())
        .filter(|n| !n.is_empty())
        .collect();
    if named.is_empty() {
        esper_interfaces::ordinary_v4()
    } else {
        esper_interfaces::named_v4(&named)
    }
}

/// Whether this machine serves file transfer at all.
pub fn files_enabled() -> bool {
    flag("files.enabled", false)
}

/// The largest single file this machine will move.
pub fn file_limits() -> esper_files::Limits {
    esper_files::Limits {
        most_bytes: whole("files.max_bytes"),
    }
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
/*
Write one setting into the file, or take it back out.

# Why this exists

Every setting in this product was, until now, something somebody edited a file
by hand to change — and the file is not created by anything, so the first
change meant knowing the path, the syntax and the key. `config show` listed
thirty-two settings and offered no way to change one of them.

# What it refuses

An unknown key, and a value the setting's own `Kind` will not read. The kind
is what decides: the bounds on a number, the words a choice may be, the shape
of an address. Nothing here re-derives any of that, so there is one answer to
"is this a valid value" and it is the same one the loader gives.

# What it says

What the value was and what it is now — and, when something *else* still
decides, that the file was written and changed nothing. A person who sets
`host.fps` in a file while `ESPER_HOST_FPS` is in their environment has made a
change that does not take effect, and being told so is the difference between
a setting and a puzzle.

# Errors

If there is no home, if the file cannot be read or written, if the key is not
one this build knows, or if the value is not one that key can hold.
*/
pub fn set(key: &str, raw: &str) -> Result<(), String> {
    set_in_file(&crate::paths::config_file()?, key, raw)
}

/// As [`set`], for a file this caller names.
///
/// The path is a parameter so a test can have one without an environment
/// variable: a helper that reads `ESPER_HOME` is a helper every test has to
/// arrange around, and the one that forgets writes into somebody's real
/// configuration.
///
/// # Errors
///
/// As [`set`].
pub fn set_in_file(path: &std::path::Path, key: &str, raw: &str) -> Result<(), String> {
    let setting = setting_named(key)?;
    // **The writer's rule, not the reader's.** A value that cannot survive
    // this file's round trip is refused here, where it costs a second attempt
    // — rather than in the loader, where it would cost somebody a machine that
    // will not start over a file that has been loading for months.
    let value = setting.kind.read_for_writing(raw).map_err(|why| {
        let shape = setting.kind.shape();
        // The shape only when the complaint has not already said it: a
        // choice's refusal names every word, and saying them twice reads as
        // two different rules.
        if why.contains(&shape) {
            format!("{key} {why}.")
        } else {
            format!("{key} {why}.{BLANK}It wants {shape}.")
        }
    })?;

    let before = read_or_empty(path)?;
    let was = get().get(key).cloned();
    let (after, changed) = esper_config::set_in(&before, key, &value);
    // **Written before the file is.** `audit.retain` and `audit.max_size`
    // decide how much of this record survives, so the line saying somebody
    // shortened it has to be in the part that was written under the old
    // answer.
    record_setting(path, key, setting, Some(&value), was.as_ref());
    write_private(path, &after)?;

    println!();
    if changed.added {
        println!("  set        {key} = {value}");
    } else {
        println!(
            "  set        {key} = {value} (was {})",
            was.map(|v| v.to_string()).unwrap_or_default()
        );
    }
    println!("  file       {}", path.display());
    say_if_overridden(key);
    say_if_the_name_is_longer_than_the_wire_carries(key, &value);
    Ok(())
}

/*
Every setting, as a program reads them.

# Why the engine answers this and not the control plane

Because the registry is here. `SETTINGS` carries each setting's kind, its
bounds, the words a choice may be and the sentence explaining it, and the
loader is what knows where each value came from. A control plane that
re-declared any of that would be a second list to keep in step, and the way
that fails is silent: a bound tightened in one place and not the other means a
screen that offers a value the file will not take.

So this prints the registry and the current values, and everything above the
engine passes it through.

# Errors

If the settings cannot be turned into JSON, which would mean a value this does
not know how to write.
*/
pub fn show_as_json() -> Result<(), String> {
    let config = get();
    let mut settings = Vec::with_capacity(esper_config::SETTINGS.len());
    for (setting, value, source) in config.effective() {
        let mut one = serde_json::Map::new();
        one.insert("key".into(), setting.key.into());
        one.insert("about".into(), setting.about.into());
        one.insert("shape".into(), setting.kind.shape().into());
        one.insert("source".into(), source.describe().into());
        // How the file would write it, which is also what `config set` takes
        // back — so a caller can round-trip a value without knowing the kind.
        one.insert("raw".into(), raw_of(value).into());
        one.insert("value".into(), as_json(value));
        one.insert("default".into(), as_json(&setting.default));
        describe_kind(setting.kind, &mut one);
        settings.push(serde_json::Value::Object(one));
    }

    let mut out = serde_json::Map::new();
    out.insert(
        "file".into(),
        config
            .path()
            .map(|p| serde_json::Value::String(p.display().to_string()))
            .unwrap_or(serde_json::Value::Null),
    );
    out.insert("present".into(), config.found().into());
    out.insert("settings".into(), serde_json::Value::Array(settings));

    let text = serde_json::to_string(&serde_json::Value::Object(out))
        .map_err(|e| format!("the settings could not be written as JSON: {e}"))?;
    println!("{text}");
    Ok(())
}

/// What kind of thing a setting holds, and the bounds that go with it.
///
/// The name and its bounds together, because a caller drawing a control needs
/// both and a caller that had to parse `shape` for them would be parsing a
/// sentence written for a person.
fn describe_kind(kind: esper_config::Kind, into: &mut serde_json::Map<String, serde_json::Value>) {
    use esper_config::Kind;
    let name = match kind {
        Kind::Text => "text",
        Kind::Flag => "flag",
        Kind::Whole { .. } => "whole",
        Kind::WholeOrNone { .. } => "whole_or_none",
        Kind::Cores => "cores",
        Kind::Address => "address",
        Kind::Key => "key",
        Kind::Choice(_) => "choice",
        Kind::Time => "time",
    };
    into.insert("kind".into(), name.into());
    if let Kind::Whole { least, most } | Kind::WholeOrNone { least, most } = kind {
        into.insert("least".into(), least.into());
        into.insert("most".into(), most.into());
    }
    if let Kind::Choice(allowed) = kind {
        into.insert(
            "choices".into(),
            serde_json::Value::Array(allowed.iter().map(|c| (*c).into()).collect()),
        );
    }
}

/// A value as JSON, in its own type rather than as text.
fn as_json(value: &esper_config::Value) -> serde_json::Value {
    use esper_config::Value;
    match value {
        Value::Text(t) => t.as_ref().into(),
        Value::Flag(b) => (*b).into(),
        Value::Whole(n) => (*n).into(),
        Value::Fraction(x) => (*x).into(),
        Value::Nothing => serde_json::Value::Null,
    }
}

/// A value as `config set` would take it back.
fn raw_of(value: &esper_config::Value) -> String {
    use esper_config::Value;
    match value {
        Value::Text(t) => t.as_ref().to_owned(),
        Value::Flag(b) => b.to_string(),
        Value::Whole(n) => n.to_string(),
        Value::Fraction(x) => x.to_string(),
        Value::Nothing => "none".to_owned(),
    }
}

/// What `config` takes.
///
/// A usage of its own, because the general help is thirty-two commands, none
/// of which says that this one can *change* a setting as well as show one.
#[must_use]
pub fn usage() -> String {
    let me = crate::net::me();
    // Written as one joined list rather than with line continuations, so the
    // four forms line up under `usage: ` whatever the binary is called — the
    // name is 5 characters through the front door and 12 through the engine.
    let pad = " ".repeat("usage: ".len());
    [
        format!("usage: {me} config [show] [--json]"),
        format!("{pad}{me} config set <key> <value>"),
        format!("{pad}{me} config unset <key>"),
        format!("{pad}{me} config example | path"),
        String::new(),
        "Every setting this machine has, where each value came from, and how to".to_owned(),
        "change one. A change is checked against what that setting can hold and".to_owned(),
        "leaves the rest of the file — comments included — as it was.".to_owned(),
        String::new(),
        "options:".to_owned(),
        "   --json   the whole registry, for a script rather than a person".to_owned(),
        String::new(),
        "subcommands:".to_owned(),
        "   show     the table. The default".to_owned(),
        "   set      write one setting".to_owned(),
        "   unset    take one out, so the default decides again".to_owned(),
        "   example  a file with every setting commented out".to_owned(),
        "   path     where the file is".to_owned(),
    ]
    .join("\n")
}

/// A blank line inside a refusal, so the shape reads as its own paragraph.
const BLANK: &str = "\n\n";

/// Take a setting out of the file, so the default decides again.
///
/// # Errors
///
/// If there is no home, if the file cannot be read or written, or if the key
/// is not one this build knows.
pub fn unset(key: &str) -> Result<(), String> {
    unset_in_file(&crate::paths::config_file()?, key)
}

/// As [`unset`], for a file this caller names.
///
/// # Errors
///
/// As [`unset`].
pub fn unset_in_file(path: &std::path::Path, key: &str) -> Result<(), String> {
    let setting = setting_named(key)?;
    let before = read_or_empty(path)?;
    let (after, changed) = esper_config::unset_in(&before, key);
    if changed.lines == 0 {
        println!();
        println!("  unset      {key} was not in the file, so nothing changed");
        println!("  default    {}", setting.default);
        return Ok(());
    }
    let was = get().get(key).cloned();
    record_setting(path, key, setting, None, was.as_ref());
    write_private(path, &after)?;
    println!();
    println!("  unset      {key} is out of the file");
    println!("  default    {} decides again", setting.default);
    println!("  file       {}", path.display());
    say_if_overridden(key);
    Ok(())
}

/// Note a setting change in the local record.
///
/// **The value of a credential is never written.** `broker.key` decides who
/// can be introduced as this machine, and a log line holding it would put it
/// in a second file with a different lifetime from the one it was typed into.
/// The record says the key changed, which is the fact an investigation needs.
///
/// A failure here is said and not fatal: a machine that refused to change a
/// setting because it could not write a log line would be a machine a full
/// disk locks somebody out of.
fn record_setting(
    file: &std::path::Path,
    key: &str,
    setting: &esper_config::Setting,
    now: Option<&esper_config::Value>,
    was: Option<&esper_config::Value>,
) {
    let shown = |value: Option<&esper_config::Value>| -> Option<String> {
        let value = value?;
        // The absence of a credential is not one, and saying "a key" about it
        // would put a key in the record that was never there. Asked of the
        // value rather than of how it prints, so a change to the spelling of
        // "nothing" cannot quietly turn this back on.
        let nothing = matches!(value, esper_config::Value::Nothing)
            || value.text().is_some_and(str::is_empty);
        if matches!(setting.kind, esper_config::Kind::Key) && !nothing {
            return Some("(a key, not written down here)".to_owned());
        }
        Some(value.to_string())
    };
    // Beside the file that changed, rather than wherever `$ESPER_HOME` points
    // now. `set_in_file` takes its path so a caller can name one; a record
    // that went somewhere else would be a record of a change to a different
    // machine's settings.
    let Some(path) = file.parent().map(|dir| dir.join("sessions.log")) else {
        eprintln!("  · not recorded: there is nowhere to write the audit log");
        return;
    };
    let entry = esper_audit::Entry::here(esper_audit::Event::SettingChanged {
        key: key.to_owned(),
        now: shown(now),
        was: shown(was),
    });
    if let Err(e) = esper_audit::Log::at(path).append(&entry) {
        eprintln!("  · not recorded in the audit log: {e}");
    }
}

/// The setting with this name, or a refusal that says what is near it.
fn setting_named(key: &str) -> Result<&'static esper_config::Setting, String> {
    if let Some(found) = esper_config::SETTINGS.iter().find(|s| s.key == key) {
        return Ok(found);
    }
    // **The nearest by spelling, not the first that shares a prefix.**
    // Somebody who typed `host.fbs` meant `host.fps`, and a list of the first
    // six `host.` settings does not contain it — which is a suggestion that
    // looks helpful and is not.
    let mut ranked: Vec<(usize, &'static str)> = esper_config::SETTINGS
        .iter()
        .map(|s| (distance(key, s.key), s.key))
        .collect();
    ranked.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(b.1)));
    // Only what is actually close. A third of the name's length is the
    // difference between a typo and a different setting.
    let how_far = key.chars().count().div_ceil(3).max(2);
    let near: Vec<&str> = ranked
        .into_iter()
        .take_while(|(d, _)| *d <= how_far)
        .map(|(_, k)| k)
        .take(4)
        .collect();
    if near.is_empty() {
        return Err(format!(
            "{key:?} is not a setting this build knows — `{} config` lists them all",
            crate::net::me()
        ));
    }
    Err(format!(
        "{key:?} is not a setting this build knows. Did you mean {}?",
        near.join(", ")
    ))
}

/// How many single-character edits turn one name into the other.
///
/// Plain Levenshtein over `char`s, on two rows rather than a whole matrix:
/// the longest setting name is thirty-odd characters and there are thirty-odd
/// of them, so this is a few thousand comparisons on the path where somebody
/// has already made a mistake.
fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, from) in a.chars().enumerate() {
        let mut next = Vec::with_capacity(row.len());
        next.push(i.saturating_add(1));
        for (j, to) in b.iter().enumerate() {
            let corner = row.get(j).copied().unwrap_or(0);
            let above = row.get(j.saturating_add(1)).copied().unwrap_or(0);
            let left = next.last().copied().unwrap_or(0);
            let swap = if from == *to {
                corner
            } else {
                corner.saturating_add(1)
            };
            next.push(
                swap.min(above.saturating_add(1))
                    .min(left.saturating_add(1)),
            );
        }
        row = next;
    }
    row.last().copied().unwrap_or(0)
}

/// Say when a device name is longer than what other machines will see.
///
/// **The file keeps it and the wire cannot.** A name travels in the handshake
/// and in this machine's advertisement, and both cut it to
/// [`esper_proto::MAX_NAME_LEN`] bytes on a character boundary. Accepting a
/// longer one silently leaves a person looking at a name here and a different
/// one on every other machine, with nothing anywhere saying why.
///
/// Said rather than refused: the truncation is what the protocol does, a long
/// name is not a mistake, and a machine that would not accept one would be
/// refusing over a limit the person cannot see from the setting's own shape.
fn say_if_the_name_is_longer_than_the_wire_carries(key: &str, value: &esper_config::Value) {
    if key != "device.name" {
        return;
    }
    let Some(name) = value.text() else {
        return;
    };
    if name.len() <= esper_proto::MAX_NAME_LEN {
        return;
    }
    let shown = shortened(name, esper_proto::MAX_NAME_LEN);
    println!(
        "  · other machines will see {shown:?} — a name travels in {} bytes, \
         and this one is {}",
        esper_proto::MAX_NAME_LEN,
        name.len()
    );
}

/// The first `most` bytes, on a character boundary.
///
/// The same rule the protocol applies, so what is printed here is what the
/// other end will actually receive rather than an approximation of it.
fn shortened(name: &str, most: usize) -> &str {
    if name.len() <= most {
        return name;
    }
    let mut end = most;
    while end > 0 && !name.is_char_boundary(end) {
        end -= 1;
    }
    name.get(..end).unwrap_or("")
}

/// **The file is not the last word, and a change that does nothing says so.**
///
/// The environment and the command line both beat it. Writing a value into the
/// file while one of those is set produces a file that says one thing and a
/// machine that does another — and without this line the only way to find out
/// is to run `config show` and notice the source column.
fn say_if_overridden(key: &str) {
    let name = esper_config::environment_name(key);
    if std::env::var_os(&name).is_some() {
        println!(
            "  · {name} is set in this environment, and beats the file — this change \
             will not take effect until it is unset"
        );
    }
}

/// The file's text, or nothing when there is no file yet.
fn read_or_empty(path: &std::path::Path) -> Result<String, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(format!("reading {}: {e}", path.display())),
    }
}

/// Write the file so only this account can read it.
///
/// **0600, and the mode is set before anything is in it.** The file can carry
/// a broker key, which is a credential; a file created 0644 and narrowed
/// afterwards is world-readable for the length of the write.
///
/// # What this does not do
///
/// It does not lock. A change is a read, an edit and a rename, so two changes
/// made in the same instant would lose one — and that is accepted rather than
/// solved, because both ways of making one are serial: a person at a terminal
/// runs one command at a time, and the window disables the control until the
/// answer comes back. A lock here would be machinery for a race nothing can
/// currently produce, and the file is small enough that the whole of it is
/// rewritten either way.
fn write_private(path: &std::path::Path, text: &str) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    }
    /*
        Written beside it and renamed over it.

        **The failures name the file somebody meant**, not the temporary one.
        A message about `config.toml.new` sends them looking for a file that
        exists for a few microseconds and should never be found — when what
        they need to know is that `config.toml` could not be written, and
        why.
    */
    let beside = path.with_extension("toml.new");
    {
        let mut file = open_private(&beside).map_err(|e| whichever(path, &beside, &e))?;
        use std::io::Write as _;
        file.write_all(text.as_bytes())
            .map_err(|e| format!("writing {}: {e}", path.display()))?;
        file.sync_all()
            .map_err(|e| format!("writing {}: {e}", path.display()))?;
    }
    // **Renamed into place**, so a reader never sees half a file and a write
    // that fails leaves the one that was there.
    std::fs::rename(&beside, path).map_err(|e| {
        let _ = std::fs::remove_file(&beside);
        format!("replacing {}: {e}", path.display())
    })
}

/// Say the failure against the file somebody named.
///
/// `open_private` reports the path it was given, which is the temporary one.
/// This puts the real name back, keeping whatever the operating system said
/// about why.
fn whichever(meant: &std::path::Path, temporary: &std::path::Path, said: &str) -> String {
    said.replace(
        &temporary.display().to_string(),
        &meant.display().to_string(),
    )
}

#[cfg(unix)]
fn open_private(path: &std::path::Path) -> Result<std::fs::File, String> {
    use std::os::unix::fs::OpenOptionsExt as _;
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| format!("writing {}: {e}", path.display()))
}

#[cfg(not(unix))]
fn open_private(path: &std::path::Path) -> Result<std::fs::File, String> {
    std::fs::File::create(path).map_err(|e| format!("writing {}: {e}", path.display()))
}

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
         # TOML, so an editor that knows TOML will colour it correctly.\n\
         #\n\
         # Nothing here has to be edited by hand: `config set <key> <value>`\n\
         # writes one, checked against what that setting can hold, and\n\
         # `config unset <key>` takes it back out. Both leave the rest of this\n\
         # file — including these comments — exactly as they found it.\n",
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

    /// A directory of this test's own, removed when the run ends.
    ///
    /// Named for the test that asked, so two running at once do not share
    /// one — which is how a suite gets a failure that only happens under
    /// `--test-threads`.
    fn scratch() -> std::path::PathBuf {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("esper-config-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a directory");
        dir
    }

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

    /// **A setting reaches the file, and only a valid one does.**
    ///
    /// The kind is what decides what a value may be, and nothing here
    /// re-derives it — so what this checks is that the kind is *consulted*,
    /// in both directions: a value it accepts is written, and one it refuses
    /// never reaches the file at all.
    #[test]
    fn only_a_value_the_setting_can_hold_is_written() {
        let dir = scratch();
        let file = dir.join("config.toml");

        set_in_file(&file, "host.fps", "45").expect("a number inside the bounds");
        let written = std::fs::read_to_string(&file).expect("the file");
        assert!(written.contains("host.fps = 45"), "{written}");

        for (key, bad) in [
            ("host.fps", "0"),           // below the bound
            ("host.fps", "241"),         // above it
            ("host.fps", "thirty"),      // not a number
            ("host.backend", "wayland"), // not one of the choices
            ("host.listen", "not-an-address"),
            ("broker.key", "nope"),
        ] {
            let refused = set_in_file(&file, key, bad).expect_err("refused");
            assert!(refused.contains(key), "{refused}");
        }

        // And the file still says what it said: a refusal that had written
        // first would be a refusal that changed something.
        assert_eq!(
            std::fs::read_to_string(&file).expect("the file"),
            written,
            "a refused value reached the file"
        );
    }

    /// A key this build does not know never becomes a line.
    ///
    /// **The file is read by this program and by a person.** A writer that
    /// took any key would let a typo sit in the file looking like a setting,
    /// and the loader would then refuse the whole file — so the typo is
    /// caught here, where the person who made it is standing.
    #[test]
    fn an_unknown_key_is_refused_before_anything_is_written() {
        let dir = scratch();
        let file = dir.join("config.toml");

        let refused = set_in_file(&file, "host.fbs", "45").expect_err("refused");
        assert!(refused.contains("host.fbs"), "{refused}");
        // Named, so somebody who mistyped can see what they meant.
        assert!(
            refused.contains("host.fps"),
            "the refusal lists nothing near it: {refused}"
        );
        assert!(!file.exists(), "a refused key created a file");

        // A key with nothing near it says where the list is instead, rather
        // than offering a setting that has nothing to do with what was typed.
        let lost = set_in_file(&file, "colour", "blue").expect_err("refused");
        assert!(lost.contains("config"), "{lost}");
        assert!(
            !lost.contains("Did you mean"),
            "a wild guess was offered: {lost}"
        );

        // And the suggestion is the nearest by spelling, from anywhere in the
        // list — not the first that happens to share a prefix.
        for (typo, meant) in [
            ("host.fbs", "host.fps"),
            ("audit.retai", "audit.retain"),
            ("files.enable", "files.enabled"),
            ("discovery.mdsn", "discovery.mdns"),
        ] {
            let said = set_in_file(&file, typo, "1").expect_err("refused");
            assert!(
                said.contains(meant),
                "{typo:?} should have suggested {meant:?}: {said}"
            );
        }
    }

    /// **The file only this account can read.** It can carry a broker key,
    /// which is a credential, so the mode is part of the feature rather than
    /// something to tighten afterwards.
    #[cfg(unix)]
    #[test]
    fn the_file_is_written_private() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = scratch();
        let file = dir.join("config.toml");
        set_in_file(&file, "host.fps", "45").expect("written");
        let mode = std::fs::metadata(&file)
            .expect("the file")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "the config file is {mode:o}");
    }

    /// Unsetting puts the default back, and unsetting what was never there is
    /// not a failure.
    #[test]
    fn unsetting_leaves_the_default_deciding() {
        let dir = scratch();
        let file = dir.join("config.toml");
        set_in_file(&file, "host.fps", "45").expect("written");
        unset_in_file(&file, "host.fps").expect("removed");
        assert_eq!(
            std::fs::read_to_string(&file).expect("the file").trim(),
            "",
            "the assignment is still there"
        );
        unset_in_file(&file, "host.fps").expect("removing it twice is not a failure");
        unset_in_file(&file, "host.fbs").expect_err("an unknown key is still refused");
    }

    /*
    A write that fails names the file somebody meant.

    The file is written beside itself and renamed over, so a reader never sees
    half of one — and every failure on that path reported the *temporary*
    name. Somebody whose directory is read-only was told that
    `config.toml.new` could not be written: a file that exists for a few
    microseconds, that they have never seen, and that they would not find if
    they went looking.
    */
    #[cfg(unix)]
    #[test]
    fn a_write_that_fails_names_the_file_somebody_meant() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = scratch();
        let shut = dir.join("shut");
        std::fs::create_dir_all(&shut).expect("a directory");
        std::fs::set_permissions(&shut, std::fs::Permissions::from_mode(0o500))
            .expect("closing it");

        let file = shut.join("config.toml");
        let refused =
            set_in_file(&file, "host.fps", "45").expect_err("a directory nothing can write in");

        // Put it back before any assertion, so a failure here does not leave a
        // directory this run cannot remove.
        std::fs::set_permissions(&shut, std::fs::Permissions::from_mode(0o700))
            .expect("opening it");

        assert!(
            refused.contains("config.toml"),
            "the failure does not name the file: {refused}"
        );
        assert!(
            !refused.contains(".toml.new"),
            "the failure names the temporary file, which nobody has ever seen: {refused}"
        );
        // And what the system said about why, which is the half that says what
        // to do.
        assert!(
            refused.to_lowercase().contains("permission"),
            "the reason was lost: {refused}"
        );
    }

    /*
    A file holding a credential says so when everybody can read it.

    `broker.key` is a key: whoever has it can be introduced by that broker as
    this machine. `config set` writes the file 0600, but people also write it
    by hand, copy it between machines and put it in places — and a mode of
    0644 is invisible until somebody looks.

    Three cases, because the wrong one of them is a line nobody reads: it says
    something when there is a key and the mode is loose, and nothing when
    either half is not true. A warning on every start about a file holding six
    ordinary settings is a warning people learn to skip.
    */
    #[cfg(unix)]
    #[test]
    fn a_loose_file_says_so_only_when_it_holds_a_key() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = scratch();

        let with_key = |mode: u32| {
            let file = dir.join(format!("key-{mode:o}.toml"));
            std::fs::write(&file, format!("broker.key = \"{}\"\n", "a".repeat(64)))
                .expect("a file");
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(mode))
                .expect("its mode");
            file
        };

        // The decision this makes, rather than the line it prints: what is
        // captured here is which of the three cases is the noisy one, and a
        // test that captured stderr would be testing the terminal.
        let loose = with_key(0o644);
        let config = esper_config::Config::load(&loose, &[]).expect("a file with a key");
        assert!(
            holds_a_readable_key(&loose, &config),
            "a key anybody can read went unmentioned"
        );

        let tight = with_key(0o600);
        let config = esper_config::Config::load(&tight, &[]).expect("a file with a key");
        assert!(
            !holds_a_readable_key(&tight, &config),
            "a key only its owner can read was reported anyway"
        );

        let ordinary = dir.join("plain.toml");
        std::fs::write(&ordinary, "host.fps = 45\n").expect("a file");
        std::fs::set_permissions(&ordinary, std::fs::Permissions::from_mode(0o644))
            .expect("its mode");
        let config = esper_config::Config::load(&ordinary, &[]).expect("a file with no key");
        assert!(
            !holds_a_readable_key(&ordinary, &config),
            "a file with no credential in it was warned about"
        );
    }

    /// **A name longer than the wire carries is said, and cut where the
    /// protocol cuts it.**
    ///
    /// A name that is one thing here and another on every other machine, with
    /// nothing saying why, is the shape of bug somebody spends an afternoon
    /// on. Cut on a character boundary, because half a character is two
    /// replacement marks on the other end's screen.
    #[test]
    fn a_name_longer_than_the_wire_carries_is_said_and_cut_where_it_will_be() {
        let most = esper_proto::MAX_NAME_LEN;
        assert_eq!(shortened("kitchen", most), "kitchen");
        assert_eq!(shortened(&"a".repeat(most), most).len(), most);
        assert_eq!(shortened(&"a".repeat(most + 40), most).len(), most);

        // A multi-byte character straddling the limit is dropped whole.
        let mut name = "a".repeat(most - 1);
        name.push('é');
        let cut = shortened(&name, most);
        assert_eq!(cut.len(), most - 1, "a character was cut in half: {cut:?}");
        assert!(cut.chars().all(|c| c == 'a'), "{cut:?}");

        // And a name made entirely of multi-byte characters still lands on a
        // boundary rather than anywhere near one.
        let wide = "日".repeat(most);
        let cut = shortened(&wide, most);
        assert!(cut.len() <= most, "{} bytes", cut.len());
        assert!(
            std::str::from_utf8(cut.as_bytes()).is_ok(),
            "the cut is not valid text"
        );
        assert_eq!(cut.chars().count(), most / 3);
    }

    /// **A credential's value never reaches the record.**
    ///
    /// `broker.key` decides who can be introduced as this machine. Writing it
    /// into the trail would copy it into a second file with a different
    /// lifetime from the one it was typed into — and the trail is the file a
    /// person is told to read, paste and attach to a bug report.
    #[test]
    fn a_key_is_recorded_as_having_changed_and_never_written_down() {
        let dir = scratch();
        let file = dir.join("config.toml");
        let secret = "b".repeat(64);
        set_in_file(&file, "broker.key", &secret).expect("written");

        let trail = std::fs::read_to_string(dir.join("sessions.log")).expect("a record");
        assert!(
            !trail.contains(&secret),
            "the key itself is in the trail:\n{trail}"
        );
        assert!(
            trail.contains("setting_changed") && trail.contains("broker.key"),
            "the change was not recorded at all:\n{trail}"
        );
        // And the file it was meant for did get it, so this is redaction and
        // not a write that quietly failed.
        let after = std::fs::read_to_string(&file).expect("the file");
        assert!(after.contains(&secret), "{after}");
    }

    /// **The record goes beside the file that changed**, not wherever the
    /// environment points. `set_in_file` takes a path so a caller can name
    /// one, and a trail written somewhere else is a record of a change to a
    /// different machine's settings.
    #[test]
    fn a_change_is_recorded_beside_the_file_it_changed() {
        let dir = scratch();
        let file = dir.join("config.toml");
        set_in_file(&file, "host.fps", "24").expect("written");

        let trail = std::fs::read_to_string(dir.join("sessions.log")).expect("a record");
        assert!(trail.contains("host.fps"), "{trail}");
        assert!(
            trail.contains("\"was\":\"30\""),
            "the previous answer is on the line: {trail}"
        );
        assert!(trail.contains("\"now\":\"24\""), "and the new one: {trail}");

        // Taking it back out is recorded too, and says so rather than reading
        // as a change to nothing.
        unset_in_file(&file, "host.fps").expect("taken out");
        let trail = std::fs::read_to_string(dir.join("sessions.log")).expect("a record");
        assert!(trail.contains("\"now\":null"), "{trail}");
    }

    /// **A line with no peer is a line about this machine**, and says so
    /// rather than carrying somebody else's key.
    #[test]
    fn a_setting_change_names_no_peer() {
        let dir = scratch();
        let file = dir.join("config.toml");
        set_in_file(&file, "discovery.mdns", "false").expect("written");

        let trail = std::fs::read_to_string(dir.join("sessions.log")).expect("a record");
        let line: serde_json::Value = serde_json::from_str(trail.trim()).expect("one JSON line");
        let said = |field: &str| line.get(field).and_then(serde_json::Value::as_str);
        assert_eq!(said("peer"), Some(""), "{trail}");
        assert_eq!(said("peer_key"), Some(""), "{trail}");
        assert_eq!(said("direction"), Some("out"), "{trail}");
    }

    /// **What was there is still there afterwards.** This is the whole reason
    /// the writer edits lines: a file is somebody's, and a change to one
    /// setting that reordered the rest or dropped their comments would be a
    /// change they did not ask for.
    #[test]
    fn a_change_leaves_the_rest_of_somebodys_file_alone() {
        let dir = scratch();
        let file = dir.join("config.toml");
        let theirs = "\
# Why this machine is set up the way it is.
device.name = \"elythion\"

host.fps = 30      # enough for what we do
files.enabled = true
";
        std::fs::write(&file, theirs).expect("their file");
        set_in_file(&file, "host.fps", "60").expect("written");

        let after = std::fs::read_to_string(&file).expect("the file");
        assert!(after.contains("# Why this machine is set up"), "{after}");
        assert!(after.contains("# enough for what we do"), "{after}");
        assert!(after.contains("device.name = \"elythion\""), "{after}");
        assert!(after.contains("files.enabled = true"), "{after}");
        assert!(after.contains("host.fps = 60"), "{after}");
        assert!(!after.contains("host.fps = 30"), "{after}");
    }
}
