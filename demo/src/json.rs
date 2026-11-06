//! A JSON writer small enough to trust.
//!
//! The demo writes three artefacts and reads none, so it needs a serialiser and not a parser, and
//! it needs no dependency to get one: strings are escaped, numbers are formatted with a fixed
//! number of decimals so two runs diff cleanly, and objects are joined from pre-rendered fields.

use std::fmt::Write as _;

/// Escape a string for a JSON document.
#[must_use]
pub fn string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for character in value.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            control if control < ' ' => {
                let _ = write!(out, "\\u{:04x}", u32::from(control));
            }
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

/// Format a float with `decimals` places, normalising the two values JSON cannot carry.
#[must_use]
pub fn number(value: f32, decimals: usize) -> String {
    let scaled = format!("{value:.decimals$}");
    // `-0` and `NaN` are both legal Rust output and both invalid JSON.
    if scaled == "-0" || scaled.starts_with("-0.0") && scaled.parse::<f64>().unwrap_or(0.0) == 0.0 {
        return format!("{:.decimals$}", 0.0_f32);
    }
    if scaled.contains("NaN") || scaled.contains("inf") {
        return "null".to_owned();
    }
    scaled
}

/// A JSON array of already-rendered items.
#[must_use]
pub fn array(items: &[String]) -> String {
    if items.is_empty() {
        return "[]".to_owned();
    }
    format!("[{}]", items.join(", "))
}

/// A JSON array laid out one item per line, for documents a person will read.
#[must_use]
pub fn array_block(items: &[String], indent: usize) -> String {
    if items.is_empty() {
        return "[]".to_owned();
    }
    let pad = " ".repeat(indent + 2);
    let close = " ".repeat(indent);
    let body = items.join(&format!(",\n{pad}"));
    format!("[\n{pad}{body}\n{close}]")
}

/// A JSON object laid out one field per line.
#[must_use]
pub fn object(fields: &[(&str, String)], indent: usize) -> String {
    if fields.is_empty() {
        return "{}".to_owned();
    }
    let pad = " ".repeat(indent + 2);
    let close = " ".repeat(indent);
    let body = fields
        .iter()
        .map(|(key, value)| format!("{}: {value}", string(key)))
        .collect::<Vec<_>>()
        .join(&format!(",\n{pad}"));
    format!("{{\n{pad}{body}\n{close}}}")
}

/// An object whose values are already inline (used for the innermost records).
#[must_use]
pub fn inline(fields: &[(&str, String)]) -> String {
    let body = fields
        .iter()
        .map(|(key, value)| format!("{}: {value}", string(key)))
        .collect::<Vec<_>>()
        .join(", ");
    format!("{{{body}}}")
}

/// A three-element vector as a JSON array, the shape every glTF-adjacent reader expects.
#[must_use]
pub fn vec3(value: bevy_math::Vec3) -> String {
    format!(
        "[{}, {}, {}]",
        number(value.x, 3),
        number(value.y, 3),
        number(value.z, 3)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strings_are_escaped() {
        assert_eq!(string("a\"b\\c\nd"), "\"a\\\"b\\\\c\\nd\"");
        assert_eq!(string("plain"), "\"plain\"");
    }

    #[test]
    fn numbers_never_render_as_invalid_json() {
        assert_eq!(number(-0.0, 2), "0.00");
        assert_eq!(number(f32::NAN, 2), "null");
        assert_eq!(number(f32::INFINITY, 1), "null");
        assert_eq!(number(1.234_56, 2), "1.23");
    }

    #[test]
    fn an_object_round_trips_its_shape() {
        let rendered = object(&[("a", "1".to_owned()), ("b", string("x"))], 0);
        assert!(rendered.starts_with("{\n  \"a\": 1,"), "{rendered}");
        assert!(rendered.ends_with("\n}"), "{rendered}");
    }

    #[test]
    fn empty_containers_are_not_split() {
        assert_eq!(array(&[]), "[]");
        assert_eq!(object(&[], 0), "{}");
    }
}
