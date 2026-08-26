//! D-Bus values, and the wire format they go in.
//!
//! # Why the alignment is the whole job
//!
//! Every D-Bus type has an alignment, and padding is measured from the start of
//! the message rather than from the start of the value. A `u32` after a
//! one-byte string sits at the next multiple of four; a struct starts at the
//! next multiple of eight whatever is in it; an array writes its length in
//! bytes *before* padding its first element, so the length does not count the
//! padding that follows it.
//!
//! Those rules are why a hand-written marshaller is worth being careful about
//! and why this one is checked three ways rather than read: round-tripped in
//! both directions for every type the portals use, compared against a real bus
//! through `dbus-send`, and fuzzed on the decoding side, because these bytes
//! arrive from another process.

use std::fmt::Write as _;

/// Anything that can appear in a portal call or its reply.
///
/// Not all of D-Bus: no `y`, `n`, `q`, `x`, or file descriptors. The set is
/// what `org.freedesktop.portal.*` actually uses, and adding to it is a
/// deliberate act rather than a completeness exercise.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// `b` — marshalled as a `u32` that is 0 or 1.
    Bool(bool),
    /// `i`
    Int32(i32),
    /// `u`
    Uint32(u32),
    /// `t`
    Uint64(u64),
    /// `d`
    Double(f64),
    /// `s`
    Str(String),
    /// `o` — a path, which is a string with rules this crate does not enforce
    /// because the bus does.
    Path(String),
    /// `g` — a signature, length-prefixed with a single byte.
    Signature(String),
    /// `a` — the element signature is carried even when there are no elements,
    /// because an empty array still has to say what it is empty *of*.
    Array { element: String, items: Vec<Value> },
    /// `{..}` — only ever inside an array.
    DictEntry(Box<Value>, Box<Value>),
    /// `(..)`
    Struct(Vec<Value>),
    /// `v`
    Variant(Box<Value>),
}

impl Value {
    /// The type signature of this value.
    pub fn signature(&self) -> String {
        let mut s = String::new();
        self.write_signature(&mut s);
        s
    }

    fn write_signature(&self, out: &mut String) {
        match self {
            Self::Bool(_) => out.push('b'),
            Self::Int32(_) => out.push('i'),
            Self::Uint32(_) => out.push('u'),
            Self::Uint64(_) => out.push('t'),
            Self::Double(_) => out.push('d'),
            Self::Str(_) => out.push('s'),
            Self::Path(_) => out.push('o'),
            Self::Signature(_) => out.push('g'),
            Self::Array { element, .. } => {
                out.push('a');
                out.push_str(element);
            }
            Self::DictEntry(k, v) => {
                out.push('{');
                k.write_signature(out);
                v.write_signature(out);
                out.push('}');
            }
            Self::Struct(fields) => {
                out.push('(');
                for f in fields {
                    f.write_signature(out);
                }
                out.push(')');
            }
            Self::Variant(_) => out.push('v'),
        }
    }

    /// A `a{sv}` from string keys, which is what every portal call's options
    /// argument is and what most of its results are.
    #[must_use]
    pub fn dict(pairs: Vec<(&str, Self)>) -> Self {
        Self::Array {
            element: "{sv}".to_owned(),
            items: pairs
                .into_iter()
                .map(|(k, v)| {
                    Self::DictEntry(
                        Box::new(Self::Str(k.to_owned())),
                        Box::new(Self::Variant(Box::new(v))),
                    )
                })
                .collect(),
        }
    }

    /// An empty `a{sv}`, which is the commonest argument in the whole portal
    /// API and is not the same as no argument at all.
    #[must_use]
    pub fn no_options() -> Self {
        Self::Array {
            element: "{sv}".to_owned(),
            items: Vec::new(),
        }
    }

    /// Look a key up in an `a{sv}`, seeing through the variant.
    ///
    /// Portal results are dictionaries and every read of one is this shape, so
    /// it lives here rather than five times at the call sites.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Self> {
        let Self::Array { items, .. } = self else {
            return None;
        };
        items.iter().find_map(|entry| match entry {
            Self::DictEntry(k, v) => match (k.as_ref(), v.as_ref()) {
                (Self::Str(name), Self::Variant(inner)) if name == key => Some(inner.as_ref()),
                (Self::Str(name), other) if name == key => Some(other),
                _ => None,
            },
            _ => None,
        })
    }

    /// The string in this value, if it is one.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(s) | Self::Path(s) | Self::Signature(s) => Some(s),
            _ => None,
        }
    }

    /// The unsigned number in this value, if it is one.
    #[must_use]
    pub const fn as_u32(&self) -> Option<u32> {
        match self {
            Self::Uint32(n) => Some(*n),
            _ => None,
        }
    }
}

/// How many bytes a type must start on a multiple of.
///
/// The one rule that is easy to get wrong: this is the alignment of the
/// *first* thing in the type, not of the type as a whole. A signature is
/// length-prefixed by one byte, so `g` aligns to 1 while `s` aligns to 4.
pub(crate) fn alignment(signature: &str) -> usize {
    match signature.as_bytes().first() {
        Some(b'y' | b'g' | b'v') => 1,
        Some(b'n' | b'q') => 2,
        Some(b'b' | b'i' | b'u' | b's' | b'o' | b'a') => 4,
        Some(b'x' | b't' | b'd' | b'(' | b'{') => 8,
        _ => 1,
    }
}

/// Split a signature into its top-level types.
///
/// `"oa{sv}"` is two arguments, not six characters. Nesting has to be counted
/// rather than scanned for, or `a{sv}` ends at the first `}` of a nested one.
pub(crate) fn split_signature(signature: &str) -> Vec<String> {
    let bytes = signature.as_bytes();
    let mut out = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let end = one_type_ends_at(bytes, at);
        out.push(signature[at..end].to_owned());
        at = end;
    }
    out
}

/// Where the single complete type starting at `at` ends.
fn one_type_ends_at(bytes: &[u8], at: usize) -> usize {
    let mut i = at;
    // Any number of `a`s stack up in front of one complete element type.
    while bytes.get(i) == Some(&b'a') {
        i += 1;
    }
    let Some(&opener) = bytes.get(i) else {
        return bytes.len();
    };
    let (open, close) = match opener {
        b'(' => (b'(', b')'),
        b'{' => (b'{', b'}'),
        // Every other type is one character, and the `a`s before it are
        // already counted.
        _ => return i + 1,
    };
    let mut depth = 0usize;
    while let Some(&byte) = bytes.get(i) {
        if byte == open {
            depth += 1;
        } else if byte == close {
            depth -= 1;
            if depth == 0 {
                return i + 1;
            }
        }
        i += 1;
    }
    bytes.len()
}

/// Bytes being built, with the padding rules applied.
pub(crate) struct Writer {
    pub(crate) bytes: Vec<u8>,
}

impl Writer {
    pub(crate) const fn new() -> Self {
        Self { bytes: Vec::new() }
    }

    /// Pad to an alignment. Measured from the start of these bytes, which for
    /// a body is the start of the message, because a body always begins on a
    /// multiple of eight.
    fn pad_to(&mut self, align: usize) {
        while !self.bytes.len().is_multiple_of(align) {
            self.bytes.push(0);
        }
    }

    pub(crate) fn u32(&mut self, n: u32) {
        self.pad_to(4);
        self.bytes.extend_from_slice(&n.to_le_bytes());
    }

    pub(crate) fn byte(&mut self, n: u8) {
        self.bytes.push(n);
    }

    fn string_like(&mut self, s: &str, short_prefix: bool) {
        if short_prefix {
            // A signature's length is one byte and needs no alignment.
            self.bytes.push(u8::try_from(s.len()).unwrap_or(u8::MAX));
        } else {
            self.u32(u32::try_from(s.len()).unwrap_or(u32::MAX));
        }
        self.bytes.extend_from_slice(s.as_bytes());
        self.bytes.push(0);
    }

    pub(crate) fn value(&mut self, value: &Value) {
        match value {
            Value::Bool(b) => self.u32(u32::from(*b)),
            Value::Int32(n) => {
                self.pad_to(4);
                self.bytes.extend_from_slice(&n.to_le_bytes());
            }
            Value::Uint32(n) => self.u32(*n),
            Value::Uint64(n) => {
                self.pad_to(8);
                self.bytes.extend_from_slice(&n.to_le_bytes());
            }
            Value::Double(d) => {
                self.pad_to(8);
                self.bytes.extend_from_slice(&d.to_le_bytes());
            }
            Value::Str(s) | Value::Path(s) => self.string_like(s, false),
            Value::Signature(s) => self.string_like(s, true),
            Value::Array { element, items } => {
                // The length counts the elements and *not* the padding between
                // the length and the first one, so it cannot be written until
                // the elements have been. Reserve four bytes, fill them after.
                self.pad_to(4);
                let length_at = self.bytes.len();
                self.bytes.extend_from_slice(&0u32.to_le_bytes());
                self.pad_to(alignment(element));
                let body_starts = self.bytes.len();
                for item in items {
                    self.value(item);
                }
                let length =
                    u32::try_from(self.bytes.len().saturating_sub(body_starts)).unwrap_or(u32::MAX);
                if let Some(slot) = self.bytes.get_mut(length_at..length_at.saturating_add(4)) {
                    slot.copy_from_slice(&length.to_le_bytes());
                }
            }
            Value::DictEntry(k, v) => {
                self.pad_to(8);
                self.value(k);
                self.value(v);
            }
            Value::Struct(fields) => {
                self.pad_to(8);
                for f in fields {
                    self.value(f);
                }
            }
            Value::Variant(inner) => {
                self.string_like(&inner.signature(), true);
                self.value(inner);
            }
        }
    }
}

/// What went wrong reading bytes from the bus.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ReadError {
    /// The message ended in the middle of a value.
    #[error("the message ended after {at} bytes, in the middle of a {wanted}")]
    Truncated { at: usize, wanted: String },
    /// A type this crate does not implement.
    #[error("signature {0:?} is not one of the types the portals use")]
    Unsupported(String),
    /// A string that was not UTF-8, or a length that was absurd.
    #[error("a {0} on the wire was malformed")]
    Malformed(String),
}

/// Bytes being read, with the same padding rules.
pub(crate) struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    pub(crate) const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    pub(crate) const fn position(&self) -> usize {
        self.at
    }

    fn pad_to(&mut self, align: usize) {
        while !self.at.is_multiple_of(align) && self.at < self.bytes.len() {
            self.at += 1;
        }
    }

    fn take(&mut self, n: usize, wanted: &str) -> Result<&'a [u8], ReadError> {
        // Checked addition, because a length read off the wire is an
        // attacker's number and `at + n` is where it would overflow.
        let end = self.at.checked_add(n).ok_or_else(|| ReadError::Truncated {
            at: self.at,
            wanted: wanted.to_owned(),
        })?;
        if end > self.bytes.len() {
            return Err(ReadError::Truncated {
                at: self.at,
                wanted: wanted.to_owned(),
            });
        }
        let out = self
            .bytes
            .get(self.at..end)
            .ok_or_else(|| ReadError::Truncated {
                at: self.at,
                wanted: wanted.to_owned(),
            })?;
        self.at = end;
        Ok(out)
    }

    pub(crate) fn u32(&mut self) -> Result<u32, ReadError> {
        self.pad_to(4);
        Ok(u32::from_le_bytes(self.exactly::<4>("u32")?))
    }

    /// Exactly `N` bytes, as an array, so the callers need no indexing.
    fn exactly<const N: usize>(&mut self, wanted: &str) -> Result<[u8; N], ReadError> {
        self.take(N, wanted)?
            .try_into()
            .map_err(|_| ReadError::Truncated {
                at: self.at,
                wanted: wanted.to_owned(),
            })
    }

    pub(crate) fn byte(&mut self) -> Result<u8, ReadError> {
        let [byte] = self.exactly::<1>("byte")?;
        Ok(byte)
    }

    fn string_like(&mut self, short_prefix: bool) -> Result<String, ReadError> {
        let length = if short_prefix {
            usize::from(self.byte()?)
        } else {
            self.u32()? as usize
        };
        let raw = self.take(length, "string")?;
        let text = std::str::from_utf8(raw)
            .map_err(|_| ReadError::Malformed("string that is not UTF-8".to_owned()))?
            .to_owned();
        // The trailing nul is on the wire and is not part of the value.
        let _ = self.take(1, "string terminator")?;
        Ok(text)
    }

    /// Read one value of the given signature.
    pub(crate) fn value(&mut self, signature: &str) -> Result<Value, ReadError> {
        let first = signature
            .as_bytes()
            .first()
            .copied()
            .ok_or_else(|| ReadError::Unsupported(String::new()))?;
        match first {
            b'b' => Ok(Value::Bool(self.u32()? != 0)),
            b'i' => {
                self.pad_to(4);
                Ok(Value::Int32(i32::from_le_bytes(self.exactly::<4>("i32")?)))
            }
            b'u' => Ok(Value::Uint32(self.u32()?)),
            b't' => {
                self.pad_to(8);
                Ok(Value::Uint64(u64::from_le_bytes(self.exactly::<8>("u64")?)))
            }
            b'd' => {
                self.pad_to(8);
                Ok(Value::Double(f64::from_le_bytes(
                    self.exactly::<8>("double")?,
                )))
            }
            b's' => Ok(Value::Str(self.string_like(false)?)),
            b'o' => Ok(Value::Path(self.string_like(false)?)),
            b'g' => Ok(Value::Signature(self.string_like(true)?)),
            b'v' => {
                let inner_signature = self.string_like(true)?;
                Ok(Value::Variant(Box::new(self.value(&inner_signature)?)))
            }
            b'a' => {
                let element = signature
                    .get(1..)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| ReadError::Unsupported(signature.to_owned()))?;
                let element = split_signature(element)
                    .into_iter()
                    .next()
                    .ok_or_else(|| ReadError::Unsupported(signature.to_owned()))?;
                let length = self.u32()? as usize;
                self.pad_to(alignment(&element));
                let ends_at = self
                    .at
                    .checked_add(length)
                    .ok_or_else(|| ReadError::Truncated {
                        at: self.at,
                        wanted: "array".to_owned(),
                    })?;
                if ends_at > self.bytes.len() {
                    return Err(ReadError::Truncated {
                        at: self.at,
                        wanted: "array".to_owned(),
                    });
                }
                let mut items = Vec::new();
                while self.at < ends_at {
                    let before = self.at;
                    items.push(self.value(&element)?);
                    // A malformed element that consumes nothing would spin
                    // here for ever on a length the sender chose.
                    if self.at == before {
                        return Err(ReadError::Malformed(
                            "array element of no length".to_owned(),
                        ));
                    }
                }
                Ok(Value::Array { element, items })
            }
            b'(' => {
                let inner = signature
                    .get(1..signature.len().saturating_sub(1))
                    .ok_or_else(|| ReadError::Unsupported(signature.to_owned()))?;
                self.pad_to(8);
                let mut fields = Vec::new();
                for part in split_signature(inner) {
                    fields.push(self.value(&part)?);
                }
                Ok(Value::Struct(fields))
            }
            b'{' => {
                let inner = signature
                    .get(1..signature.len().saturating_sub(1))
                    .ok_or_else(|| ReadError::Unsupported(signature.to_owned()))?;
                let parts = split_signature(inner);
                let (k, v) = match parts.as_slice() {
                    [k, v] => (k.clone(), v.clone()),
                    _ => return Err(ReadError::Unsupported(signature.to_owned())),
                };
                self.pad_to(8);
                let key = self.value(&k)?;
                let value = self.value(&v)?;
                Ok(Value::DictEntry(Box::new(key), Box::new(value)))
            }
            _ => Err(ReadError::Unsupported(signature.to_owned())),
        }
    }
}

/// Marshal a whole argument list, as a message body.
#[must_use]
pub fn marshal_body(args: &[Value]) -> Vec<u8> {
    let mut writer = Writer::new();
    for arg in args {
        writer.value(arg);
    }
    writer.bytes
}

/// Unmarshal a whole argument list from a message body.
pub fn unmarshal_body(signature: &str, bytes: &[u8]) -> Result<Vec<Value>, ReadError> {
    let mut reader = Reader::new(bytes);
    let mut out = Vec::new();
    for part in split_signature(signature) {
        out.push(reader.value(&part)?);
    }
    Ok(out)
}

/// The signature of an argument list.
#[must_use]
pub fn signature_of(args: &[Value]) -> String {
    let mut out = String::new();
    for arg in args {
        let _ = write!(out, "{}", arg.signature());
    }
    out
}
