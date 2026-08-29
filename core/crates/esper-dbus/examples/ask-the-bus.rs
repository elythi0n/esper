//! Ask a real bus something, so an independent tool can be asked the same.
//!
//! Round-trip tests prove this crate agrees with itself, which a writer and a
//! reader that are wrong in the same way also do. `scripts/check-dbus.sh` runs
//! this inside a container that has a real session bus and compares what comes
//! back against `dbus-send`, which is the reference implementation's own tool —
//! the same shape as checking the mDNS encoder against **dnspython** rather
//! than against itself.
//!
//! Usage, inside a session:
//!
//!     ask-the-bus names            # every name on the bus, one per line
//!     ask-the-bus id               # the bus's own GUID
//!     ask-the-bus portal <iface>   # a portal interface's version, or "absent"
// Unix only, like the crate it exercises. An example still has to have a
// `main` on every target it is type-checked for, so the gate goes round the
// body rather than round the file.
#[cfg(unix)]
mod unix_only {
    use esper_dbus::{Connection, Value};

    pub fn run() -> std::process::ExitCode {
        let what: Vec<String> = std::env::args().skip(1).collect();
        let mut bus = match Connection::session() {
            Ok(bus) => bus,
            Err(e) => {
                eprintln!("no session bus: {e}");
                return std::process::ExitCode::FAILURE;
            }
        };

        let result = match what.first().map(String::as_str) {
            Some("names") => list_names(&mut bus),
            Some("id") => bus_id(&mut bus),
            Some("portal") => portal_version(&mut bus, what.get(1).map_or("", String::as_str)),
            _ => {
                eprintln!("usage: ask-the-bus names|id|portal <interface>");
                return std::process::ExitCode::FAILURE;
            }
        };

        match result {
            Ok(text) => {
                println!("{text}");
                std::process::ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("{e}");
                std::process::ExitCode::FAILURE
            }
        }
    }

    fn list_names(bus: &mut Connection) -> Result<String, esper_dbus::BusError> {
        let reply = bus.call(
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "ListNames",
            &[],
        )?;
        let mut names: Vec<&str> = match reply.first() {
            Some(Value::Array { items, .. }) => items.iter().filter_map(Value::as_str).collect(),
            _ => Vec::new(),
        };
        names.sort_unstable();
        Ok(names.join("\n"))
    }

    fn bus_id(bus: &mut Connection) -> Result<String, esper_dbus::BusError> {
        let reply = bus.call(
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "GetId",
            &[],
        )?;
        Ok(reply
            .first()
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned())
    }

    /// A portal interface's version, through `org.freedesktop.DBus.Properties.Get`
    /// — which returns a **variant**, so this exercises the one type whose reading
    /// depends on bytes the sender chose rather than on what we asked for.
    fn portal_version(
        bus: &mut Connection,
        interface: &str,
    ) -> Result<String, esper_dbus::BusError> {
        let reply = bus.call(
            "org.freedesktop.portal.Desktop",
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.DBus.Properties",
            "Get",
            &[
                Value::Str(format!("org.freedesktop.portal.{interface}")),
                Value::Str("version".to_owned()),
            ],
        );
        match reply {
            Ok(values) => Ok(values
                .first()
                .and_then(|v| match v {
                    Value::Variant(inner) => inner.as_u32(),
                    other => other.as_u32(),
                })
                .map_or_else(|| "absent".to_owned(), |n| n.to_string())),
            // A portal that is not there answers with an error, and "absent" is
            // the honest reading of that rather than a failure of this program.
            Err(esper_dbus::BusError::Call { .. }) => Ok("absent".to_owned()),
            Err(e) => Err(e),
        }
    }
}

#[cfg(unix)]
fn main() -> std::process::ExitCode {
    unix_only::run()
}

#[cfg(not(unix))]
fn main() {}
