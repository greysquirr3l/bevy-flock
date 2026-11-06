//! A JSON reader, for glTF.
//!
//! `json` writes; this reads. They are separate modules because they are separate risks: a writer
//! can only produce documents the demo already understands, while a reader is handed whatever a
//! generator emitted and has to be correct about it. Keeping the reader to the subset glTF uses —
//! objects, arrays, strings, numbers, booleans, null — is what makes it small enough to audit, and
//! a glTF document that needs more than this is a document the demo should not be loading.
//!
//! No dependency because the alternative was `serde_json` for one file format, and the crate's
//! dependency budget is a design decision rather than an accident (see `Cargo.toml`).

/// A parsed JSON value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// `null`.
    Null,
    /// `true` or `false`.
    Bool(bool),
    /// A number. glTF uses them for counts, offsets and glTF's own floats.
    Number(f64),
    /// A string, with escapes resolved.
    String(String),
    /// An array.
    Array(Vec<Self>),
    /// An object, in document order.
    Object(Vec<(String, Self)>),
}

impl Value {
    /// The value under `key`, if this is an object that has it.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Self> {
        match self {
            Self::Object(fields) => fields
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value),
            _ => None,
        }
    }

    /// The value under `key`, or a default when the key is absent.
    ///
    /// glTF is built out of optional members with specified defaults — `byteOffset` is usually
    /// absent because it is usually zero — so this is the accessor most code wants.
    #[must_use]
    pub fn get_or(&self, key: &str, fallback: &Self) -> Self {
        self.get(key).cloned().unwrap_or_else(|| fallback.clone())
    }

    /// Element `index` of an array.
    #[must_use]
    pub fn at(&self, index: usize) -> Option<&Self> {
        match self {
            Self::Array(items) => items.get(index),
            _ => None,
        }
    }

    /// The elements of an array.
    #[must_use]
    pub fn items(&self) -> &[Self] {
        match self {
            Self::Array(items) => items,
            _ => &[],
        }
    }

    /// This value as a number.
    #[must_use]
    pub fn number(&self) -> Option<f64> {
        match self {
            Self::Number(value) => Some(*value),
            _ => None,
        }
    }

    /// This value as a `usize`, for counts and indices.
    ///
    /// Negative and fractional values are rejected rather than truncated: an index of `1.5` in a
    /// glTF file is a corrupt file, not an index.
    #[must_use]
    pub fn usize(&self) -> Option<usize> {
        let value = self.number()?;
        if value < 0.0 || value.fract() != 0.0 || value > 1.0e9 {
            return None;
        }
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "the value is non-negative, integral and bounded above, all checked just above"
        )]
        Some(value as usize)
    }

    /// This value as a string.
    #[must_use]
    pub fn text(&self) -> Option<&str> {
        match self {
            Self::String(value) => Some(value),
            _ => None,
        }
    }

    /// This value as a `[f32; 3]`, which is how glTF states a colour or a vector.
    #[must_use]
    pub fn vec3(&self) -> Option<[f32; 3]> {
        let items = self.items();
        if items.len() < 3 {
            return None;
        }
        #[expect(
            clippy::cast_possible_truncation,
            reason = "glTF colours and vectors are 32-bit floats; the parser stores f64 because a \
                      JSON number has no width"
        )]
        Some([
            items.first()?.number()? as f32,
            items.get(1)?.number()? as f32,
            items.get(2)?.number()? as f32,
        ])
    }

    /// This value as a `[f32; 4]`, which is how glTF states a colour with alpha.
    #[must_use]
    pub fn vec4(&self) -> Option<[f32; 4]> {
        let first = self.vec3()?;
        #[expect(
            clippy::cast_possible_truncation,
            reason = "as `vec3`: glTF stores 32-bit floats, the parser f64"
        )]
        let alpha = self.items().get(3).and_then(Self::number)? as f32;
        Some([first[0], first[1], first[2], alpha])
    }
}

/// Parse a document.
///
/// # Errors
///
/// Returns a message with the byte offset where the document stopped making sense. glTF is
/// machine-written, so a parse failure is a corrupt file rather than a typo a human can fix, and the
/// offset is what locates it.
pub fn parse(text: &str) -> Result<Value, String> {
    let bytes = text.as_bytes();
    let mut cursor = 0_usize;
    let value = parse_value(bytes, &mut cursor)?;
    skip_space(bytes, &mut cursor);
    if cursor != bytes.len() {
        return Err(format!("trailing content at byte {cursor}"));
    }
    Ok(value)
}

fn skip_space(bytes: &[u8], cursor: &mut usize) {
    while let Some(byte) = bytes.get(*cursor) {
        if !byte.is_ascii_whitespace() {
            break;
        }
        *cursor += 1;
    }
}

fn parse_value(bytes: &[u8], cursor: &mut usize) -> Result<Value, String> {
    skip_space(bytes, cursor);
    let byte = *bytes
        .get(*cursor)
        .ok_or_else(|| format!("unexpected end of document at byte {cursor}"))?;
    match byte {
        b'{' => parse_object(bytes, cursor),
        b'[' => parse_array(bytes, cursor),
        b'"' => Ok(Value::String(parse_string(bytes, cursor)?)),
        b't' => expect_literal(bytes, cursor, b"true", Value::Bool(true)),
        b'f' => expect_literal(bytes, cursor, b"false", Value::Bool(false)),
        b'n' => expect_literal(bytes, cursor, b"null", Value::Null),
        b'-' | b'0'..=b'9' => parse_number(bytes, cursor),
        other => Err(format!("unexpected byte {other:#04x} at byte {cursor}")),
    }
}

fn expect_literal(
    bytes: &[u8],
    cursor: &mut usize,
    literal: &[u8],
    value: Value,
) -> Result<Value, String> {
    let end = cursor.saturating_add(literal.len());
    if bytes.get(*cursor..end) == Some(literal) {
        *cursor = end;
        return Ok(value);
    }
    Err(format!("expected a literal at byte {cursor}"))
}

fn parse_object(bytes: &[u8], cursor: &mut usize) -> Result<Value, String> {
    *cursor += 1; // '{'
    let mut fields = Vec::new();
    skip_space(bytes, cursor);
    if bytes.get(*cursor) == Some(&b'}') {
        *cursor += 1;
        return Ok(Value::Object(fields));
    }
    loop {
        skip_space(bytes, cursor);
        if bytes.get(*cursor) != Some(&b'"') {
            return Err(format!("expected a key at byte {cursor}"));
        }
        let key = parse_string(bytes, cursor)?;
        skip_space(bytes, cursor);
        if bytes.get(*cursor) != Some(&b':') {
            return Err(format!("expected ':' after `{key}` at byte {cursor}"));
        }
        *cursor += 1;
        let value = parse_value(bytes, cursor)?;
        fields.push((key, value));
        skip_space(bytes, cursor);
        match bytes.get(*cursor) {
            Some(b',') => *cursor += 1,
            Some(b'}') => {
                *cursor += 1;
                return Ok(Value::Object(fields));
            }
            _ => return Err(format!("expected ',' or '}}' at byte {cursor}")),
        }
    }
}

fn parse_array(bytes: &[u8], cursor: &mut usize) -> Result<Value, String> {
    *cursor += 1; // '['
    let mut items = Vec::new();
    skip_space(bytes, cursor);
    if bytes.get(*cursor) == Some(&b']') {
        *cursor += 1;
        return Ok(Value::Array(items));
    }
    loop {
        items.push(parse_value(bytes, cursor)?);
        skip_space(bytes, cursor);
        match bytes.get(*cursor) {
            Some(b',') => *cursor += 1,
            Some(b']') => {
                *cursor += 1;
                return Ok(Value::Array(items));
            }
            _ => return Err(format!("expected ',' or ']' at byte {cursor}")),
        }
    }
}

fn parse_string(bytes: &[u8], cursor: &mut usize) -> Result<String, String> {
    *cursor += 1; // '"'
    let mut out = String::new();
    loop {
        let byte = *bytes
            .get(*cursor)
            .ok_or_else(|| format!("unterminated string at byte {cursor}"))?;
        *cursor += 1;
        match byte {
            b'"' => return Ok(out),
            b'\\' => {
                let escape = *bytes
                    .get(*cursor)
                    .ok_or_else(|| format!("unterminated escape at byte {cursor}"))?;
                *cursor += 1;
                match escape {
                    b'"' => out.push('"'),
                    b'\\' => out.push('\\'),
                    b'/' => out.push('/'),
                    b'b' => out.push('\u{8}'),
                    b'f' => out.push('\u{c}'),
                    b'n' => out.push('\n'),
                    b'r' => out.push('\r'),
                    b't' => out.push('\t'),
                    b'u' => {
                        let code = parse_hex4(bytes, cursor)?;
                        // A surrogate pair is two escapes; a lone one is invalid JSON but harmless
                        // to render as the replacement character.
                        if (0xD800..0xDC00).contains(&code) {
                            if bytes.get(*cursor..cursor.saturating_add(2)) == Some(b"\\u") {
                                *cursor += 2;
                                let low = parse_hex4(bytes, cursor)?;
                                if (0xDC00..0xE000).contains(&low) {
                                    let combined = 0x1_0000
                                        + ((u32::from(code) - 0xD800) << 10)
                                        + (u32::from(low) - 0xDC00);
                                    out.push(char::from_u32(combined).unwrap_or('\u{fffd}'));
                                } else {
                                    out.push('\u{fffd}');
                                }
                            } else {
                                out.push('\u{fffd}');
                            }
                        } else {
                            out.push(char::from_u32(u32::from(code)).unwrap_or('\u{fffd}'));
                        }
                    }
                    other => {
                        return Err(format!(
                            "unknown escape \\{} at byte {cursor}",
                            other as char
                        ));
                    }
                }
            }
            other if other < 0x20 => {
                return Err(format!("raw control byte in a string at byte {cursor}"));
            }
            _ => {
                // Collect a whole UTF-8 sequence: the input is a `&str`, so it is valid UTF-8, but
                // this parser walks bytes.
                let start = cursor.saturating_sub(1);
                let mut end = *cursor;
                while let Some(next) = bytes.get(end) {
                    if next & 0xC0 == 0x80 {
                        end += 1;
                    } else {
                        break;
                    }
                }
                let slice = bytes
                    .get(start..end)
                    .ok_or_else(|| format!("bad UTF-8 sequence at byte {start}"))?;
                let text = std::str::from_utf8(slice)
                    .map_err(|error| format!("bad UTF-8 at byte {start}: {error}"))?;
                out.push_str(text);
                *cursor = end;
            }
        }
    }
}

fn parse_hex4(bytes: &[u8], cursor: &mut usize) -> Result<u16, String> {
    let slice = bytes
        .get(*cursor..cursor.saturating_add(4))
        .ok_or_else(|| format!("truncated \\u escape at byte {cursor}"))?;
    let text = std::str::from_utf8(slice).map_err(|_| format!("bad hex at byte {cursor}"))?;
    let value = u16::from_str_radix(text, 16).map_err(|_| format!("bad hex `{text}`"))?;
    *cursor += 4;
    Ok(value)
}

fn parse_number(bytes: &[u8], cursor: &mut usize) -> Result<Value, String> {
    let start = *cursor;
    while let Some(byte) = bytes.get(*cursor) {
        if byte.is_ascii_digit() || matches!(byte, b'-' | b'+' | b'.' | b'e' | b'E') {
            *cursor += 1;
        } else {
            break;
        }
    }
    let slice = bytes
        .get(start..*cursor)
        .ok_or_else(|| format!("bad number at byte {start}"))?;
    let text = std::str::from_utf8(slice).map_err(|_| format!("bad number at byte {start}"))?;
    text.parse::<f64>()
        .map(Value::Number)
        .map_err(|_| format!("`{text}` is not a number (byte {start})"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_flock::testkit::{OrDie, OrDieErr};

    #[test]
    fn objects_arrays_and_numbers_round_trip() {
        let value = parse(r#"{"a": [1, 2.5, -3e2], "b": {"c": true}, "d": null}"#).or_die();
        assert_eq!(
            value.get("a").and_then(|a| a.at(1)).and_then(Value::number),
            Some(2.5)
        );
        assert_eq!(
            value
                .get("b")
                .and_then(|b| b.get("c"))
                .and_then(|c| match c {
                    Value::Bool(flag) => Some(*flag),
                    _ => None,
                }),
            Some(true)
        );
        assert_eq!(value.get("d"), Some(&Value::Null));
    }

    #[test]
    fn escapes_are_resolved() {
        let value = parse(r#"{"name": "a\"b\\c\nd\u00e9"}"#).or_die();
        assert_eq!(value.get("name").and_then(Value::text), Some("a\"b\\c\ndé"));
    }

    #[test]
    fn a_surrogate_pair_becomes_one_character() {
        let value = parse(r#"{"name": "\ud83d\ude00"}"#).or_die();
        assert_eq!(value.get("name").and_then(Value::text), Some("\u{1f600}"));
    }

    #[test]
    fn missing_keys_use_the_default() {
        let value = parse(r#"{"byteOffset": 12}"#).or_die();
        assert_eq!(
            value.get_or("byteStride", &Value::Number(4.0)),
            Value::Number(4.0)
        );
        assert_eq!(
            value.get_or("byteOffset", &Value::Null),
            Value::Number(12.0)
        );
    }

    #[test]
    fn counts_are_integral() {
        assert_eq!(parse("42").or_die().usize(), Some(42));
        assert_eq!(parse("4.5").or_die().usize(), None);
        assert_eq!(parse("-1").or_die().usize(), None);
    }

    #[test]
    fn broken_documents_report_where_they_broke() {
        let error = parse(r#"{"a": }"#).or_die_err();
        assert!(error.contains("byte"), "{error}");
        assert!(parse(r#"{"a": 1}"#).is_ok());
        assert!(parse(r#"{"a": 1"#).is_err());
        assert!(parse("[1, 2]extra").is_err());
    }
}
