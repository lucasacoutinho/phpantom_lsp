//! Type string manipulation and classification utilities.
//!
//! This submodule provides the foundational helpers for normalising raw
//! type strings extracted from docblocks: splitting type tokens, stripping
//! leading backslashes, generic parameters, nullable wrappers, and
//! classifying scalars.
//!
//! These functions are used throughout the other `docblock` submodules
//! (generics, shapes, callable types) and the wider codebase.

/// Scalar / built-in type names that can never be an object and therefore
/// must not be overridden by a class-name docblock annotation.
pub(crate) const SCALAR_TYPES: &[&str] = &[
    "int", "integer", "float", "double", "string", "bool", "boolean", "void", "never", "null",
    "false", "true", "array", "callable", "iterable", "resource",
];

/// All built-in type keywords offered in PHPDoc type completion contexts.
///
/// This is a superset of [`SCALAR_TYPES`] that also includes PHPDoc-only
/// pseudo-types (`mixed`, `class-string`, `non-empty-string`, etc.) and
/// the special `self` / `static` keywords.  Kept here as a single source
/// of truth so the list is maintained in one place rather than duplicated
/// in the completion handler.
pub(crate) const PHPDOC_TYPE_KEYWORDS: &[&str] = &[
    // ── SCALAR_TYPES entries ────────────────────────────────────────
    "int",
    "integer",
    "float",
    "double",
    "string",
    "bool",
    "boolean",
    "void",
    "never",
    "null",
    "false",
    "true",
    "array",
    "callable",
    "iterable",
    "resource",
    // ── Additional PHP built-in types ───────────────────────────────
    "object",
    "mixed",
    "self",
    "static",
    // ── PHPStan / PHPDoc extended types ─────────────────────────────
    // Integer refinements
    "positive-int",
    "negative-int",
    "non-negative-int",
    "non-positive-int",
    "non-zero-int",
    "int-mask",
    "int-mask-of",
    // String refinements
    "non-empty-string",
    "non-falsy-string",
    "truthy-string",
    "literal-string",
    "non-empty-literal-string",
    "numeric-string",
    "callable-string",
    "lowercase-string",
    "uppercase-string",
    "non-empty-lowercase-string",
    "non-empty-uppercase-string",
    // Array / list refinements
    "list",
    "non-empty-list",
    "non-empty-array",
    "associative-array",
    // Class-string variants
    "class-string",
    "interface-string",
    "trait-string",
    "enum-string",
    // Scalar / mixed variants
    "scalar",
    "numeric",
    "empty-scalar",
    "non-empty-scalar",
    "non-empty-mixed",
    "number",
    "empty",
    // Object / callable variants
    "callable-object",
    "callable-array",
    // Resource variants
    "closed-resource",
    "open-resource",
    // Key / value utility types
    "array-key",
    "key-of",
    "value-of",
    // Never aliases
    "no-return",
    "noreturn",
    "never-return",
    "never-returns",
];

/// Split off the first type token from `s`, respecting `<…>` and `{…}`
/// nesting (the latter is needed for PHPStan array shape syntax like
/// `array{name: string, age: int}`).
///
/// Returns `(type_token, remainder)` where `type_token` is the full type
/// (e.g. `Collection<int, User>` or `array{name: string}`) and
/// `remainder` is whatever follows.
pub(crate) fn split_type_token(s: &str) -> (&str, &str) {
    let mut angle_depth = 0i32;
    let mut brace_depth = 0i32;
    let mut paren_depth = 0i32;
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut prev_char = '\0';

    for (i, c) in s.char_indices() {
        // Handle string literals inside array shape keys — skip everything
        // inside quotes so that `{`, `}`, `,`, `:` etc. are not
        // misinterpreted as structural delimiters.
        if in_single_quote {
            if c == '\'' && prev_char != '\\' {
                in_single_quote = false;
            }
            prev_char = c;
            continue;
        }
        if in_double_quote {
            if c == '"' && prev_char != '\\' {
                in_double_quote = false;
            }
            prev_char = c;
            continue;
        }

        match c {
            '\'' if brace_depth > 0 => in_single_quote = true,
            '"' if brace_depth > 0 => in_double_quote = true,
            '<' => angle_depth += 1,
            '>' if angle_depth > 0 => {
                angle_depth -= 1;
                // If we just closed the outermost `<`, the type ends here
                // (but only when we're not also inside braces or parens).
                // Continue consuming any union/intersection suffix so
                // that `Collection<int, User>|null` stays one token.
                if angle_depth == 0 && brace_depth == 0 && paren_depth == 0 {
                    let end = i + c.len_utf8();
                    let end = consume_union_intersection_suffix(s, end);
                    return (&s[..end], &s[end..]);
                }
            }
            '{' => brace_depth += 1,
            '}' => {
                brace_depth -= 1;
                // If we just closed the outermost `{`, the type ends here
                // (but only when we're not also inside angle brackets or parens).
                // Continue consuming any union/intersection suffix so
                // that `array{id: int}|null` stays one token.
                if brace_depth == 0 && angle_depth == 0 && paren_depth == 0 {
                    let end = i + c.len_utf8();
                    let end = consume_union_intersection_suffix(s, end);
                    return (&s[..end], &s[end..]);
                }
            }
            '(' => paren_depth += 1,
            ')' => {
                paren_depth -= 1;
                // After closing the outermost `(…)`, check whether a
                // callable return-type follows (`: ReturnType`).  If so,
                // consume the `: ` and the return-type token as part of
                // this token.
                if paren_depth == 0 && angle_depth == 0 && brace_depth == 0 {
                    let after_paren = i + c.len_utf8();
                    let rest = &s[after_paren..];
                    let rest_trimmed = rest.trim_start();
                    if let Some(after_colon) = rest_trimmed.strip_prefix(':') {
                        let after_colon = after_colon.trim_start();
                        if !after_colon.is_empty() {
                            // Consume the return-type token.
                            let (ret_tok, _remainder) = split_type_token(after_colon);
                            // Compute the end offset: start of `after_colon`
                            // relative to `s` + length of ret_tok.
                            let colon_start_in_s =
                                s.len() - rest.len() + (rest.len() - rest_trimmed.len()) + 1;
                            let ret_start_in_s = colon_start_in_s
                                + (after_colon.as_ptr() as usize
                                    - s[colon_start_in_s..].as_ptr() as usize);
                            let mut end = ret_start_in_s + ret_tok.len();

                            // After a callable return type, continue
                            // consuming union/intersection suffixes so
                            // that `(Closure(Builder): mixed)|null`
                            // is kept as one token.
                            end = consume_union_intersection_suffix(s, end);

                            return (&s[..end], &s[end..]);
                        }
                    }
                    // After a bare parenthesized group (no callable
                    // return type), continue consuming any
                    // union/intersection suffix.  This handles DNF
                    // types like `(A&B)|C` and grouped callables
                    // like `(Closure(X): Y)|null`.
                    let end = consume_union_intersection_suffix(s, after_paren);
                    return (&s[..end], &s[end..]);
                }
            }
            c if c.is_whitespace() && angle_depth == 0 && brace_depth == 0 && paren_depth == 0 => {
                return (&s[..i], &s[i..]);
            }
            _ => {}
        }
        prev_char = c;
    }
    (s, "")
}

/// After a parenthesized type group or callable return type, consume
/// any `|Type` or `&Type` continuation so the full union/intersection
/// is kept as a single token.
///
/// `pos` is the byte offset just past the already-consumed portion of
/// `s`.  Returns the updated end offset after consuming zero or more
/// `|`/`&`-separated type parts.
fn consume_union_intersection_suffix(s: &str, pos: usize) -> usize {
    let mut end = pos;
    loop {
        let rest = &s[end..];
        // Allow optional whitespace before the operator, but only if
        // the operator is `|` or `&` (not a plain space which would
        // signal the start of the next token like a parameter name).
        let rest_trimmed = rest.trim_start();
        let first = rest_trimmed.chars().next();
        if first == Some('|') || first == Some('&') {
            // `&$var` is a by-reference parameter, not an intersection.
            if first == Some('&') && rest_trimmed.as_bytes().get(1) == Some(&b'$') {
                break;
            }
            // Skip the operator character.
            let after_op = &rest_trimmed[1..];
            let after_op = after_op.trim_start();
            if after_op.is_empty() {
                break;
            }
            // Consume the next type token.
            let (tok, _) = split_type_token(after_op);
            if tok.is_empty() {
                break;
            }
            // Compute the absolute end position from the consumed
            // token.  `after_op` is a sub-slice of `s`, so pointer
            // arithmetic gives us the byte offset.
            let tok_start_in_s = after_op.as_ptr() as usize - s.as_ptr() as usize;
            end = tok_start_in_s + tok.len();
        } else {
            break;
        }
    }
    end
}

/// Split a type string on `|` at nesting depth 0, respecting `<…>`,
/// `(…)`, and `{…}` nesting.
///
/// Returns a `Vec` with at least one element.  If there is no `|` at
/// depth 0, the returned vector contains the entire input as a single
/// element.
///
/// # Examples
///
/// - `"Foo|null"` → `["Foo", "null"]`
/// - `"Collection<int|string, User>|null"` → `["Collection<int|string, User>", "null"]`
/// - `"array{name: string|int}|null"` → `["array{name: string|int}", "null"]`
/// - `"Foo"` → `["Foo"]`
pub(crate) fn split_union_depth0(s: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth_angle = 0i32;
    let mut depth_paren = 0i32;
    let mut depth_brace = 0i32;
    let mut start = 0;

    for (i, c) in s.char_indices() {
        match c {
            '<' => depth_angle += 1,
            '>' => depth_angle -= 1,
            '(' => depth_paren += 1,
            ')' => depth_paren -= 1,
            '{' => depth_brace += 1,
            '}' => depth_brace -= 1,
            '|' if depth_angle == 0 && depth_paren == 0 && depth_brace == 0 => {
                parts.push(&s[start..i]);
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(&s[start..]);
    parts
}

/// Split a type string on `&` (intersection) at depth 0, respecting
/// `<…>`, `(…)`, and `{…}` nesting.
///
/// This is necessary so that intersection operators inside generic
/// parameters or object/array shapes (e.g. `object{foo: A&B}`) are not
/// mistaken for top-level intersection splits.
///
/// # Examples
///
/// - `"User&JsonSerializable"` → `["User", "JsonSerializable"]`
/// - `"object{foo: int}&\stdClass"` → `["object{foo: int}", "\stdClass"]`
/// - `"object{foo: A&B}"` → `["object{foo: A&B}"]` (no split — `&` is nested)
pub fn split_intersection_depth0(s: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth_angle = 0i32;
    let mut depth_paren = 0i32;
    let mut depth_brace = 0i32;
    let mut start = 0;

    for (i, c) in s.char_indices() {
        match c {
            '<' => depth_angle += 1,
            '>' => depth_angle -= 1,
            '(' => depth_paren += 1,
            ')' => depth_paren -= 1,
            '{' => depth_brace += 1,
            '}' => depth_brace -= 1,
            '&' if depth_angle == 0 && depth_paren == 0 && depth_brace == 0 => {
                parts.push(&s[start..i]);
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(&s[start..]);
    parts
}

/// Clean a raw type string from a docblock, **preserving** generic
/// parameters so that downstream resolution can apply generic
/// substitution.
///
/// Specifically this function:
///   - Strips leading `\` (PHP fully-qualified prefix)
///   - Strips trailing punctuation (`.`, `,`) that could leak from
///     docblock descriptions
///   - Handles `TypeName|null` → `TypeName` (using depth-0 splitting so
///     that `Collection<int|string, User>|null` is handled correctly)
///
/// Generic parameters like `<int, User>` are **not** stripped.  Use
/// [`base_class_name`] when you need just the unparameterised class name.
pub fn clean_type(raw: &str) -> String {
    // Preserve the leading `\` — it marks the type as a fully-qualified
    // name (FQN).  Stripping it would make the name look relative,
    // causing `resolve_type_string` to incorrectly prepend the current
    // file's namespace (e.g. `\Illuminate\Builder` would become
    // `App\Models\Illuminate\Builder`).  Downstream consumers
    // (`type_hint_to_classes`, `resolve_name`, `resolve_class_name`)
    // all handle `\`-prefixed names correctly.
    let s = raw;

    // Strip trailing punctuation that could leak from docblocks
    // (e.g. trailing `.` or `,` in descriptions).
    // Be careful not to strip `,` or `.` that is inside `<…>`.
    let s = s.trim_end_matches(['.', ',']);

    // Handle `TypeName|null` → extract the non-null part, using depth-0
    // splitting so that `|` inside `<…>` is not mistaken for a union
    // separator.
    let parts = split_union_depth0(s);
    if parts.len() > 1 {
        let non_null: Vec<&str> = parts
            .into_iter()
            .map(|p| p.trim())
            .filter(|p| !p.eq_ignore_ascii_case("null"))
            .collect();

        if non_null.len() == 1 {
            return non_null[0].to_string();
        }
        // Multiple non-null parts → keep as union
        if non_null.len() > 1 {
            return non_null.join("|");
        }
    }

    s.to_string()
}

/// Extract the base (unparameterised) class name from a type string,
/// stripping any generic parameters.
///
/// This is the function to use when you need a plain class name for
/// lookups (e.g. mixin resolution, type assertion matching) and do
/// **not** want to carry generic arguments forward.
///
/// # Examples
///
/// - `"Collection<int, User>"` → `"Collection"`
/// - `"\\App\\Models\\User"` → `"\\App\\Models\\User"`
/// - `"?Foo"` → `"Foo"`
/// - `"Foo|null"` → `"Foo"`
pub fn base_class_name(raw: &str) -> String {
    let cleaned = clean_type(raw);
    let stripped = strip_nullable(&cleaned);
    strip_generics(stripped)
}

/// Strip generic parameters and array shape braces from a (already
/// cleaned) type string.
///
/// `"Collection<int, User>"` → `"Collection"`
/// `"array{name: string}"` → `"array"`
/// `"Foo"` → `"Foo"`
pub(crate) fn strip_generics(s: &str) -> String {
    // Find the earliest `<` or `{` — both delimit parameterisation.
    let angle = s.find('<');
    let brace = s.find('{');
    let idx = match (angle, brace) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    };
    if let Some(i) = idx {
        s[..i].to_string()
    } else {
        s.to_string()
    }
}

/// Split generic arguments on commas at depth 0, respecting `<…>`,
/// `(…)`, and `{…}` nesting.
///
/// Returns trimmed, non-empty segments. This is the single shared
/// implementation used by `parse_generic_args`, `extract_generics_tag`,
/// `apply_substitution`, and the generic-key/value extraction helpers.
pub(crate) fn split_generic_args(s: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth_angle = 0i32;
    let mut depth_paren = 0i32;
    let mut depth_brace = 0i32;
    let mut start = 0;

    for (i, ch) in s.char_indices() {
        match ch {
            '<' => depth_angle += 1,
            '>' => depth_angle -= 1,
            '(' => depth_paren += 1,
            ')' => depth_paren -= 1,
            '{' => depth_brace += 1,
            '}' => depth_brace -= 1,
            ',' if depth_angle == 0 && depth_paren == 0 && depth_brace == 0 => {
                parts.push(s[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    let last = s[start..].trim();
    if !last.is_empty() {
        parts.push(last);
    }
    parts
}

/// Strip the nullable `?` prefix from a type string.
pub(crate) fn strip_nullable(type_str: &str) -> &str {
    type_str.strip_prefix('?').unwrap_or(type_str)
}

/// Normalize nullable syntax to a canonical `X|null` form.
///
/// `?Foo` becomes `Foo|null`, `null|Foo` becomes `Foo|null`, and
/// already-canonical forms are returned unchanged.  This lets callers
/// compare two type strings for semantic equivalence regardless of
/// which nullable notation was used.
pub(crate) fn normalize_nullable(type_str: &str) -> String {
    // Expand `?X` → `X|null`
    let expanded = if let Some(inner) = type_str.strip_prefix('?') {
        format!("{inner}|null")
    } else {
        type_str.to_string()
    };

    // Sort the union parts so that `null|string` and `string|null`
    // compare equal.
    let mut parts: Vec<&str> = expanded.split('|').map(|s| s.trim()).collect();
    parts.sort_unstable();
    parts.join("|")
}

/// Check whether a raw type string is nullable.
///
/// A type is nullable when it:
/// - starts with `?` (e.g. `?Foo`)
/// - contains `null` as a union member (e.g. `Foo|null`, `null|Foo`)
/// - is literally `"null"` or `"mixed"`
///
/// Used by the null-coalesce (`??`) refinement to decide whether the
/// LHS can actually be null (and therefore whether the RHS matters).
pub(crate) fn raw_type_is_nullable(type_str: &str) -> bool {
    if type_str == "null" || type_str == "mixed" {
        return true;
    }
    if type_str.starts_with('?') {
        return true;
    }
    type_str.split('|').any(|part| part.trim() == "null")
}

/// Remove `null` from a union type string.
///
/// - `?Foo` → `"Foo"`
/// - `Foo|null` → `"Foo"`
/// - `Foo|Bar|null` → `"Foo|Bar"`
/// - `null` → `None`
/// - `mixed` → `None` (stripping null from mixed is meaningless)
///
/// Returns `None` when the type collapses to nothing.
pub(crate) fn strip_null_from_union(type_str: &str) -> Option<String> {
    if type_str == "null" || type_str == "mixed" {
        return None;
    }
    if let Some(inner) = type_str.strip_prefix('?') {
        return Some(inner.to_string());
    }
    let parts: Vec<&str> = type_str
        .split('|')
        .map(|s| s.trim())
        .filter(|s| *s != "null")
        .collect();
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("|"))
    }
}

/// Check whether a type name is a built-in scalar (i.e. can never be an object).
pub(crate) fn is_scalar(type_name: &str) -> bool {
    // Strip generic parameters and array shape braces before checking so
    // that `array<int, User>` and `array{name: string}` are still
    // recognised as scalar base types.
    let base = if let Some(idx_angle) = type_name.find('<') {
        let idx_brace = type_name.find('{').unwrap_or(usize::MAX);
        &type_name[..idx_angle.min(idx_brace)]
    } else if let Some(idx) = type_name.find('{') {
        &type_name[..idx]
    } else {
        type_name
    };
    let lower = base.to_ascii_lowercase();
    SCALAR_TYPES.contains(&lower.as_str())
}

/// Replace `self`, `static`, and `$this` tokens in a type string with
/// a concrete class name, using word-boundary detection.
///
/// This handles all type string shapes: simple types (`self`), union
/// types (`self|null`), nullable types (`?static`), and generic types
/// (`Collection<$this>`).
pub fn replace_self_in_type(type_str: &str, class_name: &str) -> String {
    // Fast path: no substitution needed.
    if !type_str.contains("self") && !type_str.contains("static") && !type_str.contains("$this") {
        return type_str.to_string();
    }

    let bytes = type_str.as_bytes();
    let len = bytes.len();
    let mut out = String::with_capacity(len);
    let mut i = 0;

    while i < len {
        // Try to match each keyword at the current position.
        if let Some(replaced) = try_replace_keyword(bytes, i, len, b"$this") {
            out.push_str(class_name);
            i = replaced;
            continue;
        }
        if let Some(replaced) = try_replace_keyword(bytes, i, len, b"static") {
            out.push_str(class_name);
            i = replaced;
            continue;
        }
        if let Some(replaced) = try_replace_keyword(bytes, i, len, b"self") {
            out.push_str(class_name);
            i = replaced;
            continue;
        }
        out.push(bytes[i] as char);
        i += 1;
    }

    out
}

/// Check whether `keyword` appears at position `i` in `bytes` with valid
/// word boundaries on both sides.  Returns `Some(end_pos)` if the keyword
/// matches, where `end_pos` is the byte offset just past the keyword.
fn try_replace_keyword(bytes: &[u8], i: usize, len: usize, keyword: &[u8]) -> Option<usize> {
    let kw_len = keyword.len();
    if i + kw_len > len {
        return None;
    }
    if &bytes[i..i + kw_len] != keyword {
        return None;
    }
    // `$this` starts with `$`, which is never part of a preceding
    // identifier, so we only need a before-boundary check for `self`
    // and `static`.
    if keyword != b"$this" {
        let before_ok = i == 0 || !is_ident_char(bytes[i - 1]);
        if !before_ok {
            return None;
        }
    }
    let after = i + kw_len;
    let after_ok = after >= len || !is_ident_char(bytes[after]);
    if !after_ok {
        return None;
    }
    Some(after)
}

/// Returns `true` if `b` is an ASCII alphanumeric character or `_`,
/// i.e. a character that can appear in a PHP identifier.
fn is_ident_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}
