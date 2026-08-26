//! The subset of D-Bus the desktop portals use, spoken directly.
//!
//! # Why this exists
//!
//! The Wayland input path on GNOME and KDE is the `RemoteDesktop` portal, the
//! portal is D-Bus, and this project had no D-Bus client. Taking
//! `zbus` would have added **30 crates, about 188,000 lines and 845 `unsafe`
//! occurrences** to this workspace — measured against what it already has — in
//! order to call six methods on one interface. The same judgement as
//! `esper-discovery`, which speaks mDNS itself rather than take eight crates
//! to advertise one service.
//!
//! So this is narrow on purpose: one connection, to the **session** bus, over
//! a unix socket, authenticated with `EXTERNAL`, carrying method calls and one
//! signal. No server, no object tree, no properties, no system bus, no TCP.
//! A machine that needs any of those is out of scope and is told so rather
//! than failing obscurely.
//!

mod value;

pub use value::{ReadError, Value, marshal_body, signature_of, unmarshal_body};

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use value::{Reader, Writer};

/// What went wrong talking to the bus.
#[derive(Debug, thiserror::Error)]
pub enum BusError {
    /// No `DBUS_SESSION_BUS_ADDRESS`, or one naming a transport not implemented.
    #[error("no session bus to connect to: {0}")]
    NoBus(String),
    /// The socket would not open, or died.
    #[error("the session bus socket: {0}")]
    Socket(#[from] std::io::Error),
    /// The `EXTERNAL` handshake was refused.
    #[error("the bus refused this connection: {0}")]
    Rejected(String),
    /// A message could not be read.
    #[error("reading from the bus: {0}")]
    Read(#[from] ReadError),
    /// The far end answered with an error rather than a reply.
    #[error("{name}: {message}")]
    Call { name: String, message: String },
    /// Nothing arrived in time.
    #[error("waited {0:?} and the bus said nothing")]
    TimedOut(Duration),
}

/// A message off the wire.
#[derive(Debug, Clone)]
pub struct Message {
    /// 1 method call, 2 return, 3 error, 4 signal.
    pub kind: u8,
    /// This message's own serial.
    pub serial: u32,
    /// The serial this is a reply to, for a return or an error.
    pub reply_to: Option<u32>,
    /// The object path, for a signal.
    pub path: Option<String>,
    /// The interface, for a signal.
    pub interface: Option<String>,
    /// The member name, for a signal or a call.
    pub member: Option<String>,
    /// The error name, for an error.
    pub error_name: Option<String>,
    /// The arguments.
    pub args: Vec<Value>,
}

const METHOD_CALL: u8 = 1;
const ERROR: u8 = 3;
const SIGNAL: u8 = 4;
// There is no constant for a method return (2): a reply that is not an error
// is taken as one, so that a bus speaking a message kind this does not know
// still delivers its arguments rather than being discarded.

/// A connection to the session bus.
#[derive(Debug)]
pub struct Connection {
    socket: UnixStream,
    serial: u32,
    /// The name the bus gave us, like `:1.42`. Portals build object paths out
    /// of it, so it is not merely informational.
    unique_name: String,
}

impl Connection {
    /// Connect to the session bus named by the environment.
    ///
    /// # Errors
    ///
    /// If there is no `DBUS_SESSION_BUS_ADDRESS`, if it names a transport this
    /// does not implement, if the socket will not open, or if the handshake is
    /// refused.
    pub fn session() -> Result<Self, BusError> {
        let address = std::env::var("DBUS_SESSION_BUS_ADDRESS")
            .map_err(|_| BusError::NoBus("DBUS_SESSION_BUS_ADDRESS is not set".to_owned()))?;
        Self::connect(&address)
    }

    /// Connect to a bus at a given address.
    ///
    /// # Errors
    ///
    /// As [`Self::session`].
    pub fn connect(address: &str) -> Result<Self, BusError> {
        let socket = open(address)?;
        let mut connection = Self {
            socket,
            serial: 0,
            unique_name: String::new(),
        };
        connection.handshake()?;
        let reply = connection.call(
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "Hello",
            &[],
        )?;
        connection.unique_name = reply
            .first()
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        Ok(connection)
    }

    /// The name the bus gave this connection.
    #[must_use]
    pub fn unique_name(&self) -> &str {
        &self.unique_name
    }

    /// The `EXTERNAL` handshake.
    ///
    /// A leading nul byte, then the mechanism with our uid in hex, then
    /// `BEGIN`. `EXTERNAL` is the only mechanism a Linux session bus uses: the
    /// kernel already told it who we are over the socket, and the uid we send
    /// only has to agree.
    fn handshake(&mut self) -> Result<(), BusError> {
        let uid = our_uid();
        let mut hex = String::new();
        for byte in uid.to_string().bytes() {
            hex.push_str(&format!("{byte:02x}"));
        }

        self.socket.write_all(&[0])?;
        self.socket
            .write_all(format!("AUTH EXTERNAL {hex}\r\n").as_bytes())?;
        self.socket.flush()?;

        let mut reader = BufReader::new(self.socket.try_clone()?);
        let mut line = String::new();
        reader.read_line(&mut line)?;
        if !line.starts_with("OK") {
            return Err(BusError::Rejected(line.trim_end().to_owned()));
        }

        self.socket.write_all(b"BEGIN\r\n")?;
        self.socket.flush()?;
        Ok(())
    }

    fn next_serial(&mut self) -> u32 {
        // Serial zero is reserved, so a wrap skips it.
        self.serial = self.serial.checked_add(1).unwrap_or(1);
        self.serial
    }

    /// Send a method call and wait for its reply.
    ///
    /// # Errors
    ///
    /// If the socket fails, if the reply cannot be parsed, or if the far end
    /// answers with an error — which is returned as one rather than as a
    /// successful reply nobody looks at.
    pub fn call(
        &mut self,
        destination: &str,
        path: &str,
        interface: &str,
        member: &str,
        args: &[Value],
    ) -> Result<Vec<Value>, BusError> {
        let serial = self.send(METHOD_CALL, destination, path, interface, member, args)?;
        loop {
            let message = self.receive(Duration::from_secs(30))?;
            if message.reply_to != Some(serial) {
                // Somebody else's message — a signal, most often. Portal
                // conversations interleave signals with replies by design, so
                // this is the normal case rather than an anomaly.
                continue;
            }
            return match message.kind {
                ERROR => Err(BusError::Call {
                    name: message.error_name.unwrap_or_else(|| "unknown".to_owned()),
                    message: message
                        .args
                        .first()
                        .and_then(Value::as_str)
                        .unwrap_or("no detail")
                        .to_owned(),
                }),
                _ => Ok(message.args),
            };
        }
    }

    /// Add a match rule, so the bus starts delivering signals we care about.
    ///
    /// Without one the bus delivers nothing: signals are broadcast and a
    /// connection receives only what it has asked for. Every portal call that
    /// needs a person answers on a signal, so forgetting this is a portal that
    /// appears to hang.
    ///
    /// # Errors
    ///
    /// If the bus refuses the rule or the socket fails.
    pub fn add_match(&mut self, rule: &str) -> Result<(), BusError> {
        self.call(
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "AddMatch",
            &[Value::Str(rule.to_owned())],
        )?;
        Ok(())
    }

    /// Wait for a signal on a path, for at most `patience`.
    ///
    /// # Errors
    ///
    /// If nothing arrives in time, or the socket fails.
    pub fn await_signal(
        &mut self,
        path: &str,
        member: &str,
        patience: Duration,
    ) -> Result<Message, BusError> {
        let deadline = Instant::now() + patience;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(BusError::TimedOut(patience));
            }
            let message = self.receive(left)?;
            if message.kind == SIGNAL
                && message.path.as_deref() == Some(path)
                && message.member.as_deref() == Some(member)
            {
                return Ok(message);
            }
        }
    }

    fn send(
        &mut self,
        kind: u8,
        destination: &str,
        path: &str,
        interface: &str,
        member: &str,
        args: &[Value],
    ) -> Result<u32, BusError> {
        let serial = self.next_serial();
        let body = marshal_body(args);
        let signature = signature_of(args);

        let mut fields = vec![
            Value::Struct(vec![
                Value::Uint32(1),
                Value::Variant(Box::new(Value::Path(path.to_owned()))),
            ]),
            Value::Struct(vec![
                Value::Uint32(2),
                Value::Variant(Box::new(Value::Str(interface.to_owned()))),
            ]),
            Value::Struct(vec![
                Value::Uint32(3),
                Value::Variant(Box::new(Value::Str(member.to_owned()))),
            ]),
            Value::Struct(vec![
                Value::Uint32(6),
                Value::Variant(Box::new(Value::Str(destination.to_owned()))),
            ]),
        ];
        if !signature.is_empty() {
            fields.push(Value::Struct(vec![
                Value::Uint32(8),
                Value::Variant(Box::new(Value::Signature(signature))),
            ]));
        }

        // The header field codes are bytes on the wire, but writing them as
        // `u32` and truncating would be wrong in a way nothing would notice
        // until a code above 255 existed. They are written as bytes below.
        let mut writer = Writer::new();
        writer.byte(b'l');
        writer.byte(kind);
        writer.byte(0);
        writer.byte(1);
        writer.u32(u32::try_from(body.len()).unwrap_or(u32::MAX));
        writer.u32(serial);

        // The header fields are `a(yv)`, and the `y` is a real byte, so the
        // struct above cannot be reused as written. Marshalled by hand here.
        let mut fields_writer = Writer::new();
        for field in &fields {
            let Value::Struct(parts) = field else {
                continue;
            };
            let (code, variant) = match parts.as_slice() {
                [Value::Uint32(code), v] => (*code, v),
                _ => continue,
            };
            // Each struct in the array starts on an eight-byte boundary,
            // measured from the start of the array's elements.
            while !fields_writer.bytes.len().is_multiple_of(8) {
                fields_writer.byte(0);
            }
            fields_writer.byte(u8::try_from(code).unwrap_or(0));
            fields_writer.value(variant);
        }
        writer.u32(u32::try_from(fields_writer.bytes.len()).unwrap_or(u32::MAX));
        // The array's elements begin at the next multiple of eight, measured
        // from the start of the *message*, which is where the field writer's
        // own alignment has to be rebased.
        let base = writer.bytes.len();
        let padding = (8 - base % 8) % 8;
        for _ in 0..padding {
            writer.byte(0);
        }
        writer.bytes.extend_from_slice(&fields_writer.bytes);

        // The body begins on an eight-byte boundary after the header.
        while !writer.bytes.len().is_multiple_of(8) {
            writer.byte(0);
        }
        writer.bytes.extend_from_slice(&body);

        self.socket.write_all(&writer.bytes)?;
        self.socket.flush()?;
        Ok(serial)
    }

    fn receive(&mut self, patience: Duration) -> Result<Message, BusError> {
        self.socket.set_read_timeout(Some(patience))?;

        let mut fixed = [0u8; 16];
        // A read timeout arrives as `WouldBlock` or `TimedOut` depending on
        // how the socket was set, and as neither of those words: the caller
        // sees "Resource temporarily unavailable (os error 11)" and has to
        // know errno to read it. Waiting is a thing this crate does on
        // purpose — a portal prompt is a person — so it says so.
        if let Err(e) = self.socket.read_exact(&mut fixed) {
            return Err(match e.kind() {
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => {
                    BusError::TimedOut(patience)
                }
                _ => BusError::Socket(e),
            });
        }

        let body_length = u32::from_le_bytes([fixed[4], fixed[5], fixed[6], fixed[7]]) as usize;
        let serial = u32::from_le_bytes([fixed[8], fixed[9], fixed[10], fixed[11]]);
        let fields_length =
            u32::from_le_bytes([fixed[12], fixed[13], fixed[14], fixed[15]]) as usize;

        // The fields are followed by padding to eight, then the body.
        let padded_fields = fields_length.div_ceil(8) * 8;
        let mut rest = vec![0u8; padded_fields + body_length];
        self.socket.read_exact(&mut rest)?;

        let mut message = Message {
            kind: fixed[1],
            serial,
            reply_to: None,
            path: None,
            interface: None,
            member: None,
            error_name: None,
            args: Vec::new(),
        };

        // Header fields: an array of (byte, variant), read by hand for the
        // same reason it is written by hand.
        let header_bytes = rest.get(..fields_length.min(rest.len())).unwrap_or(&[]);
        let mut reader = Reader::new(header_bytes);
        let mut signature = String::new();
        while reader.position() < fields_length {
            while !reader.position().is_multiple_of(8) {
                if reader.byte().is_err() {
                    break;
                }
            }
            if reader.position() >= fields_length {
                break;
            }
            let Ok(code) = reader.byte() else { break };
            let Ok(v) = reader.value("v") else { break };
            let Value::Variant(inner) = v else { continue };
            match (code, *inner) {
                (1, Value::Path(p)) => message.path = Some(p),
                (2, Value::Str(s)) => message.interface = Some(s),
                (3, Value::Str(s)) => message.member = Some(s),
                (4, Value::Str(s)) => message.error_name = Some(s),
                (5, Value::Uint32(n)) => message.reply_to = Some(n),
                (8, Value::Signature(s)) => signature = s,
                _ => {}
            }
        }

        if !signature.is_empty() {
            // The body follows the padded header fields. A message whose
            // declared lengths do not reach that far has no body to read
            // rather than a body of nothing, and saying so is better than
            // indexing past the end of what arrived.
            let body = rest.get(padded_fields..).unwrap_or(&[]);
            message.args = unmarshal_body(&signature, body)?;
        }
        Ok(message)
    }
}

/// Open the socket an address names.
///
/// Two transports, because a Linux session bus uses one of them: a filesystem
/// path, or an abstract socket, which is a Linux namespace with no file behind
/// it. Anything else is refused by name rather than half-attempted.
fn open(address: &str) -> Result<UnixStream, BusError> {
    // An address can carry several alternatives separated by semicolons, and
    // several keys separated by commas. Only the unix ones are understood.
    for candidate in address.split(';') {
        let Some(rest) = candidate.strip_prefix("unix:") else {
            continue;
        };
        for pair in rest.split(',') {
            if let Some(path) = pair.strip_prefix("path=") {
                return Ok(UnixStream::connect(path)?);
            }
            if let Some(name) = pair.strip_prefix("abstract=") {
                use std::os::linux::net::SocketAddrExt as _;
                let socket_address = std::os::unix::net::SocketAddr::from_abstract_name(name)?;
                return Ok(UnixStream::connect_addr(&socket_address)?);
            }
        }
    }
    Err(BusError::NoBus(format!(
        "{address:?} names no unix socket; only unix:path= and unix:abstract= are implemented"
    )))
}

/// This process's user id, without a libc dependency.
///
/// `/proc/self` is owned by the user the process runs as, so its owner is the
/// answer. On a machine with no `/proc` the handshake sends zero and the bus
/// refuses it, which is a clearer failure than guessing.
fn our_uid() -> u32 {
    use std::os::unix::fs::MetadataExt as _;
    std::fs::metadata("/proc/self")
        .map(|m| m.uid())
        .unwrap_or(0)
}
