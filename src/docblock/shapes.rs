//! Array shape and object shape parsing.
//!
//! This submodule handles parsing PHPStan/Psalm array shape and object
//! shape type strings into their constituent entries, and looking up
//! value types by key.

use crate::types::ArrayShapeEntry;

/// Find the position of the matching `}` for an opening `{` that has
/// already been consumed.  `s` starts right after the `{`.
fn find_matching_brace_close(s: &str) -> usize {
    let mut depth = 1i32;
    let mut angle_depth = 0i32;
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut prev_char = '\0';

    for (i, ch) in s.char_indices() {
        // Skip characters inside quoted strings so that `{`, `}`, etc.
        // inside array shape keys like `"host}?"` are not misinterpreted.
        if in_single_quote {
            if ch == '\'' && prev_char != '\\' {
                in_single_quote = false;
            }
            prev_char = ch;
            continue;
        }
        if in_double_quote {
            if ch == '"' && prev_char != '\\' {
                in_double_quote = false;
            }
            prev_char = ch;
            continue;
        }

        match ch {
            '\'' => in_single_quote = true,
            '"' => in_double_quote = true,
            '{' => depth += 1,
            '}' if angle_depth == 0 => {
                depth -= 1;
                if depth == 0 {
                    return i;
                }
            }
            '<' => angle_depth += 1,
            '>' if angle_depth > 0 => angle_depth -= 1,
            _ => {}
        }
        prev_char = ch;
    }
    // Fallback: end of string (malformed type).
    s.len()
}

/// Split array shape entries on commas at depth 0, respecting `<…>`,
/// `(…)`, and `{…}` nesting.
fn split_shape_entries(s: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth_angle = 0i32;
    let mut depth_paren = 0i32;
    let mut depth_brace = 0i32;
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut prev_char = '\0';
    let mut start = 0;

    for (i, ch) in s.char_indices() {
        // Skip characters inside quoted strings so that commas inside
        // quoted array shape keys (e.g. `",host"`) don't split entries.
        if in_single_quote {
            if ch == '\'' && prev_char != '\\' {
                in_single_quote = false;
            }
            prev_char = ch;
            continue;
        }
        if in_double_quote {
            if ch == '"' && prev_char != '\\' {
                in_double_quote = false;
            }
            prev_char = ch;
            continue;
        }

        match ch {
            '\'' => in_single_quote = true,
            '"' => in_double_quote = true,
            '<' => depth_angle += 1,
            '>' => depth_angle -= 1,
            '(' => depth_paren += 1,
            ')' => depth_paren -= 1,
            '{' => depth_brace += 1,
            '}' => depth_brace -= 1,
            ',' if depth_angle == 0 && depth_paren == 0 && depth_brace == 0 => {
                parts.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
        prev_char = ch;
    }
    let last = &s[start..];
    if !last.trim().is_empty() {
        parts.push(last);
    }
    parts
}

/// Split a single array shape entry into key and value on the **first**
/// `:` at depth 0, outside of quoted strings.
///
/// Returns `Some((key_part, value_part))` if a `:` separator is found,
/// or `None` for positional entries.
///
/// Must respect `<…>`, `{…}` nesting and quoted strings so that colons
/// inside nested types or quoted keys (e.g. `"host:port"`) are not
/// mistaken for the key–value separator.
fn split_shape_key_value(s: &str) -> Option<(&str, &str)> {
    let mut depth_angle = 0i32;
    let mut depth_paren = 0i32;
    let mut depth_brace = 0i32;
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut prev_char = '\0';

    for (i, ch) in s.char_indices() {
        // Skip characters inside quoted strings so that `:` inside
        // quoted keys like `"host:port"` is not treated as a separator.
        if in_single_quote {
            if ch == '\'' && prev_char != '\\' {
                in_single_quote = false;
            }
            prev_char = ch;
            continue;
        }
        if in_double_quote {
            if ch == '"' && prev_char != '\\' {
                in_double_quote = false;
            }
            prev_char = ch;
            continue;
        }

        match ch {
            '\'' => in_single_quote = true,
            '"' => in_double_quote = true,
            '<' => depth_angle += 1,
            '>' => depth_angle -= 1,
            '(' => depth_paren += 1,
            ')' => depth_paren -= 1,
            '{' => depth_brace += 1,
            '}' => depth_brace -= 1,
            ':' if depth_angle == 0 && depth_paren == 0 && depth_brace == 0 => {
                return Some((&s[..i], &s[i + 1..]));
            }
            _ => {}
        }
        prev_char = ch;
    }
    None
}

/// Strip surrounding single or double quotes from an array shape key.
///
/// PHPStan/Psalm allow array shape keys to be quoted when they contain
/// special characters (spaces, punctuation, etc.):
///   - `'po rt'` → `po rt`
///   - `"host"` → `host`
///   - `foo` → `foo` (unchanged)
fn strip_shape_key_quotes(key: &str) -> String {
    if ((key.starts_with('\'') && key.ends_with('\''))
        || (key.starts_with('"') && key.ends_with('"')))
        && key.len() >= 2
    {
        return key[1..key.len() - 1].to_string();
    }
    key.to_string()
}

/// Parse a PHPStan/Psalm array shape type string into its constituent
/// entries.
///
/// Handles both named and positional (implicit-key) entries, optional
/// keys (with `?` suffix), and nested types.
///
/// # Examples
///
/// - `"array{name: string, age: int}"` → two entries
/// - `"array{name: string, age?: int}"` → "age" is optional
/// - `"array{string, int}"` → positional keys "0", "1"
/// - `"array{user: User, items: list<Item>}"` → nested generics preserved
///
/// Returns `None` if the type is not an array shape.
pub fn parse_array_shape(type_str: &str) -> Option<Vec<ArrayShapeEntry>> {
    let s = type_str.strip_prefix('\\').unwrap_or(type_str);
    let s = s.strip_prefix('?').unwrap_or(s);

    // Must start with `array{` (case-insensitive base).
    let brace_pos = s.find('{')?;
    let base = &s[..brace_pos];
    if !base.eq_ignore_ascii_case("array") {
        return None;
    }

    // Extract the content between `{` and the matching `}`.
    let rest = &s[brace_pos + 1..];
    let close_pos = find_matching_brace_close(rest);
    let inner = rest[..close_pos].trim();

    if inner.is_empty() {
        return Some(vec![]);
    }

    let raw_entries = split_shape_entries(inner);
    let mut entries = Vec::with_capacity(raw_entries.len());
    let mut implicit_index: u32 = 0;

    for raw in raw_entries {
        let raw = raw.trim();
        if raw.is_empty() {
            continue;
        }

        // Try to split on `:` to find `key: type` or `key?: type`.
        // Must respect nesting and quoted strings so that `list<int>`
        // inside a value type doesn't get split, and colons inside
        // quoted keys like `"host:port"` are handled correctly.
        if let Some((key_part, value_part)) = split_shape_key_value(raw) {
            let key_trimmed = key_part.trim();
            let value_trimmed = value_part.trim();

            let (key, optional) = if let Some(k) = key_trimmed.strip_suffix('?') {
                (k.to_string(), true)
            } else {
                (key_trimmed.to_string(), false)
            };

            // Strip surrounding quotes from keys — PHPStan allows
            // `'foo'`, `"bar"`, and unquoted `baz` as key names.
            let key = strip_shape_key_quotes(&key);

            entries.push(ArrayShapeEntry {
                key,
                value_type: value_trimmed.to_string(),
                optional,
            });
        } else {
            // No `:` found — positional entry with implicit numeric key.
            entries.push(ArrayShapeEntry {
                key: implicit_index.to_string(),
                value_type: raw.to_string(),
                optional: false,
            });
            implicit_index += 1;
        }
    }

    Some(entries)
}

/// Look up the value type for a specific key in an array shape type string.
///
/// Given a type like `"array{name: string, user: User}"` and key `"user"`,
/// returns `Some("User")`.
///
/// Returns `None` if the type is not an array shape or the key is not found.
pub fn extract_array_shape_value_type(type_str: &str, key: &str) -> Option<String> {
    let entries = parse_array_shape(type_str)?;
    entries
        .into_iter()
        .find(|e| e.key == key)
        .map(|e| e.value_type)
}

/// Parse a PHPStan object shape type string into its constituent entries.
///
/// Object shapes describe an anonymous object with typed properties:
///
/// # Examples
///
/// - `"object{foo: int, bar: string}"` → two entries
/// - `"object{foo: int, bar?: string}"` → "bar" is optional
/// - `"object{'foo': int, \"bar\": string}"` → quoted property names
/// - `"object{foo: int, bar: string}&\stdClass"` → intersection ignored here
///
/// The returned entries reuse [`ArrayShapeEntry`] since the structure is
/// identical (key name, value type, optional flag).
///
/// Returns `None` if the type is not an object shape.
pub fn parse_object_shape(type_str: &str) -> Option<Vec<ArrayShapeEntry>> {
    let s = type_str.strip_prefix('?').unwrap_or(type_str);

    // Must start with `object{` (case-insensitive base).
    let brace_pos = s.find('{')?;
    let base = &s[..brace_pos];
    if !base.eq_ignore_ascii_case("object") {
        return None;
    }

    // Extract the content between `{` and the matching `}`.
    let rest = &s[brace_pos + 1..];
    let close_pos = find_matching_brace_close(rest);
    let inner = rest[..close_pos].trim();

    if inner.is_empty() {
        return Some(vec![]);
    }

    // Reuse the same splitting and key-value parsing as array shapes —
    // the syntax is identical (`key: Type`, `key?: Type`, quoted keys).
    let raw_entries = split_shape_entries(inner);
    let mut entries = Vec::with_capacity(raw_entries.len());

    for raw in raw_entries {
        let raw = raw.trim();
        if raw.is_empty() {
            continue;
        }

        if let Some((key_part, value_part)) = split_shape_key_value(raw) {
            let key_trimmed = key_part.trim();
            let value_trimmed = value_part.trim();

            let (key, optional) = if let Some(k) = key_trimmed.strip_suffix('?') {
                (k.to_string(), true)
            } else {
                (key_trimmed.to_string(), false)
            };

            let key = strip_shape_key_quotes(&key);

            entries.push(ArrayShapeEntry {
                key,
                value_type: value_trimmed.to_string(),
                optional,
            });
        }
        // Object shapes don't have positional entries — skip anything
        // without an explicit key.
    }

    Some(entries)
}

/// Return `true` if `type_str` is an object shape type (e.g. `object{name: string}`).
pub fn is_object_shape(type_str: &str) -> bool {
    let s = type_str.strip_prefix('?').unwrap_or(type_str);
    // Check for `object{` case-insensitively, but only when `{` immediately
    // follows the word `object` (no intervening whitespace).
    if let Some(brace_pos) = s.find('{') {
        let base = &s[..brace_pos];
        base.eq_ignore_ascii_case("object")
    } else {
        false
    }
}

/// Look up the value type for a specific property in an object shape.
///
/// Given a type like `"object{name: string, user: User}"` and key `"user"`,
/// returns `Some("User")`.
///
/// Returns `None` if the type is not an object shape or the property
/// is not found.
pub fn extract_object_shape_property_type(type_str: &str, prop: &str) -> Option<String> {
    let entries = parse_object_shape(type_str)?;
    entries
        .into_iter()
        .find(|e| e.key == prop)
        .map(|e| e.value_type)
}
