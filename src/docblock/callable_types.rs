//! Callable and generator type parsing.
//!
//! This submodule handles extracting return types and parameter types
//! from `callable(…): …` and `Closure(…): …` PHPDoc type annotations,
//! as well as extracting specific generic parameters from PHP's
//! `Generator<TKey, TValue, TSend, TReturn>` type.

use super::type_strings::{
    clean_type, is_scalar, split_generic_args, split_type_token, split_union_depth0, strip_generics,
};

/// Extract the return type from a callable/Closure type annotation.
///
/// Handles the PHPStan/Psalm callable type syntax:
///   - `Closure(): User`              → `Some("User")`
///   - `callable(int, string): bool`  → `Some("bool")`
///   - `\Closure(Type): Response`     → `Some("Response")`
///   - `Closure(): User|null`         → `Some("User|null")`
///   - `Closure`                      → `None` (no return type info)
///   - `callable`                     → `None`
///
/// Returns `None` if the type is not a callable/Closure type or has no
/// return type annotation.
pub fn extract_callable_return_type(type_str: &str) -> Option<String> {
    let s = type_str.strip_prefix('\\').unwrap_or(type_str);
    let s = s.strip_prefix('?').unwrap_or(s);

    // Must start with `Closure` or `callable` (case-sensitive for Closure,
    // case-insensitive for callable to match PHP semantics).
    let rest = if let Some(r) = s.strip_prefix("Closure") {
        r
    } else if let Some(r) = s.strip_prefix("callable") {
        r
    } else {
        return None;
    };

    // Must have a parameter list starting with `(`.
    let rest = rest.strip_prefix('(')?;

    // Find the matching closing `)`, tracking nested parens.
    let mut depth = 1i32;
    let mut close_pos = None;
    for (i, c) in rest.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    close_pos = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }

    let close = close_pos?;
    let after_paren = &rest[close + 1..];

    // Expect `: ReturnType` after the closing paren.
    let after_colon = after_paren.trim_start().strip_prefix(':')?;
    let ret_type = after_colon.trim_start();

    if ret_type.is_empty() {
        return None;
    }

    // The return type extends to the end of the type token (it may be a
    // union like `User|null`).  Use `split_type_token` to extract it
    // properly, but since this is already the tail of a single type token,
    // we can take everything up to the first whitespace at depth 0.
    let (ret_tok, _) = split_type_token(ret_type);
    if ret_tok.is_empty() {
        return None;
    }

    Some(ret_tok.to_string())
}

/// Extract the parameter types from a `callable(…): …` or `Closure(…): …` type string.
///
/// Returns a `Vec` of the individual parameter type strings.  When the
/// callable has no parameters (e.g. `callable(): void`) an empty `Vec` is
/// returned.  Returns `None` when `type_str` is not a callable/Closure type
/// with a parameter list.
///
/// # Examples
///
/// ```text
/// callable(User, int): void       → Some(["User", "int"])
/// Closure(TValue): mixed          → Some(["TValue"])
/// callable(): void                → Some([])
/// callable(Collection<int, User>): void → Some(["Collection<int, User>"])
/// Closure                         → None
/// string                          → None
/// ```
pub fn extract_callable_param_types(type_str: &str) -> Option<Vec<String>> {
    // Unwrap union types: `(Closure(X): Y)|null` → `Closure(X): Y`,
    // `Closure(X): Y|null` → try `Closure(X): Y|null` first, then
    // fall back to splitting on `|` and trying each part.
    if let Some(result) = extract_callable_param_types_inner(type_str) {
        return Some(result);
    }

    // Try each union member individually — handles `Closure(X)|null`,
    // `null|callable(Y)`, and parenthesized groups like
    // `(Closure(X): Y)|null`.
    for part in split_union_depth0(type_str) {
        let part = part.trim();
        // Strip outer parens from grouped callables: `(Closure(X): Y)` → `Closure(X): Y`
        let inner = part
            .strip_prefix('(')
            .and_then(|p| p.strip_suffix(')'))
            .unwrap_or(part);
        if let Some(result) = extract_callable_param_types_inner(inner) {
            return Some(result);
        }
    }

    None
}

/// Inner implementation: try to parse a single callable/Closure type
/// string (not a union) and extract its parameter types.
fn extract_callable_param_types_inner(type_str: &str) -> Option<Vec<String>> {
    let s = type_str.strip_prefix('?').unwrap_or(type_str);

    // Must start with `Closure` or `callable`.
    let rest = if let Some(r) = s.strip_prefix("Closure") {
        r
    } else if let Some(r) = s.strip_prefix("callable") {
        r
    } else {
        return None;
    };

    // Must have a parameter list starting with `(`.
    let rest = rest.strip_prefix('(')?;

    // Find the matching closing `)`, tracking nested parens/angles/braces.
    let mut paren_depth = 1i32;
    let mut angle_depth = 0i32;
    let mut brace_depth = 0i32;
    let mut close_pos = None;
    for (i, c) in rest.char_indices() {
        match c {
            '(' => paren_depth += 1,
            ')' => {
                paren_depth -= 1;
                if paren_depth == 0 {
                    close_pos = Some(i);
                    break;
                }
            }
            '<' => angle_depth += 1,
            '>' if angle_depth > 0 => angle_depth -= 1,
            '{' => brace_depth += 1,
            '}' if brace_depth > 0 => brace_depth -= 1,
            _ => {}
        }
    }

    let close = close_pos?;
    let params_str = rest[..close].trim();

    if params_str.is_empty() {
        return Some(vec![]);
    }

    // Split on `,` at depth 0 (respecting `<…>`, `{…}`, `(…)` nesting).
    let mut result = Vec::new();
    let mut current_start = 0usize;
    let mut paren_d = 0i32;
    let mut angle_d = 0i32;
    let mut brace_d = 0i32;

    for (i, c) in params_str.char_indices() {
        match c {
            '(' => paren_d += 1,
            ')' => paren_d -= 1,
            '<' => angle_d += 1,
            '>' if angle_d > 0 => angle_d -= 1,
            '{' => brace_d += 1,
            '}' if brace_d > 0 => brace_d -= 1,
            ',' if paren_d == 0 && angle_d == 0 && brace_d == 0 => {
                let param = params_str[current_start..i].trim();
                if !param.is_empty() {
                    result.push(param.to_string());
                }
                current_start = i + 1;
            }
            _ => {}
        }
    }
    // Last (or only) parameter.
    let last = params_str[current_start..].trim();
    if !last.is_empty() {
        result.push(last.to_string());
    }

    Some(result)
}

/// Extract the TSend type from a `Generator<TKey, TValue, TSend, TReturn>`
/// type annotation.
///
/// PHP's `Generator` class has four generic parameters:
///   - TKey (index 0): the key type yielded by `yield $key => $value`
///   - TValue (index 1): the value type yielded by `yield $value`
///   - TSend (index 2): the type received by `$var = yield $expr`
///   - TReturn (index 3): the type returned by `return $expr`
///
/// This function extracts the **third** parameter (TSend, index 2).
/// When the type is not a `Generator` or has fewer than 3 parameters,
/// returns `None`.
///
/// # Examples
///
/// - `Generator<int, User, Request, void>` → `Some("Request")`
/// - `Generator<int, User, mixed>`         → `Some("mixed")`
/// - `Generator<int, User>`                → `None` (only 2 params)
/// - `Generator<User>`                     → `None` (only 1 param)
/// - `Collection<int, User>`               → `None` (not Generator)
pub fn extract_generator_send_type(raw_type: &str) -> Option<String> {
    let s = raw_type.strip_prefix('\\').unwrap_or(raw_type);
    let s = s.strip_prefix('?').unwrap_or(s);

    let angle_pos = s.find('<')?;
    let base_type = &s[..angle_pos];
    if base_type != "Generator" {
        return None;
    }

    let inner = s.get(angle_pos + 1..)?.strip_suffix('>')?.trim();
    if inner.is_empty() {
        return None;
    }

    let args = split_generic_args(inner);
    // TSend is the 3rd parameter (index 2).
    let send_part = args.get(2)?;
    let cleaned = clean_type(send_part.trim());
    let base_name = strip_generics(&cleaned);

    if base_name.is_empty() || is_scalar(&base_name) {
        return None;
    }
    Some(cleaned)
}

/// Extract the TValue type from a `Generator<TKey, TValue, …>` annotation,
/// returning the raw type string even when it is a scalar.
///
/// Unlike [`super::generics::extract_generic_value_type`] which skips scalar
/// element types, this function returns the raw TValue string regardless.
/// This is needed for reverse yield inference where we may want to propagate
/// the type even if it is scalar (so that the caller can decide whether to
/// resolve it).
///
/// Returns `None` when the type is not a `Generator` generic.
///
/// # Examples
///
/// - `Generator<int, User>`                → `Some("User")`
/// - `Generator<int, string>`              → `Some("string")`
/// - `Generator<User>`                     → `Some("User")` (single param treated as TValue)
/// - `Generator<int, User, mixed, void>`   → `Some("User")`
/// - `Collection<int, User>`               → `None` (not Generator)
pub fn extract_generator_value_type_raw(raw_type: &str) -> Option<String> {
    let s = raw_type.strip_prefix('\\').unwrap_or(raw_type);
    let s = s.strip_prefix('?').unwrap_or(s);

    let angle_pos = s.find('<')?;
    let base_type = &s[..angle_pos];
    if base_type != "Generator" {
        return None;
    }

    let inner = s.get(angle_pos + 1..)?.strip_suffix('>')?.trim();
    if inner.is_empty() {
        return None;
    }

    let args = split_generic_args(inner);
    // TValue is the 2nd parameter (index 1).  When only one param is
    // given, treat it as TValue (consistent with single-param behaviour).
    let value_part = args.get(1).or(args.last()).copied()?;
    Some(clean_type(value_part.trim()))
}
