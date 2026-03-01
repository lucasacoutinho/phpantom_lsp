/// Source-text scanning helpers that are **not** part of the deprecated
/// `extract_raw_type_from_assignment_text` pipeline.
///
/// These functions perform lightweight, targeted scans of raw PHP source
/// text for patterns that the AST-based walker cannot (or need not)
/// handle:
///
/// - **`extract_new_expression_class`** — parse `new ClassName(…)` from
///   a text fragment.
/// - **`extract_function_return_from_source`** — find a function's
///   `@return` type by scanning backward for its docblock.
/// - **`extract_closure_return_type_from_assignment`** — find a
///   closure/arrow-function's native return type hint from its
///   assignment.
/// - **`extract_first_class_callable_return_type`** — resolve the
///   return type of a first-class callable assignment like
///   `$fn = strlen(...)` or `$fn = $obj->method(...)`.
/// - **`try_chained_array_access_with_candidates`** /
///   **`walk_array_segments_and_resolve`** — walk bracket segments on
///   candidate raw type strings to resolve array access chains.
///
/// Migrated from `text_resolution.rs` as part of the §3 refactoring to
/// retire the bulk text scanner while preserving these unique helpers.
use crate::Backend;
use crate::docblock;
use crate::types::{BracketSegment, ClassInfo};
use crate::util::{find_semicolon_balanced, short_name};

use super::resolver::FunctionLoaderFn;

/// Replace `self`, `static`, and `$this` tokens in a type string with
/// the concrete class name.
///
/// This is needed when a method's return type is extracted in a context
/// where the owning class is known but the caller will resolve the type
/// string without that class context (e.g. first-class callable return
/// types passed through with `owning_class_name = ""`).
fn replace_self_references(type_str: &str, class_name: &str) -> String {
    // Fast path: no substitution needed.
    if !type_str.contains("self") && !type_str.contains("static") && !type_str.contains("$this") {
        return type_str.to_string();
    }

    // Split on `|` (union) boundaries, replace each token that is
    // exactly `self`, `static`, or `$this` (with optional leading `?`).
    type_str
        .split('|')
        .map(|part| {
            let trimmed = part.trim();
            let (prefix, base) = if let Some(rest) = trimmed.strip_prefix('?') {
                ("?", rest)
            } else {
                ("", trimmed)
            };
            if base == "self" || base == "static" || base == "$this" {
                format!("{}{}", prefix, class_name)
            } else {
                trimmed.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("|")
}

// ─── impl Backend — source-text helpers ─────────────────────────────────────

impl Backend {
    /// Parse a `new ClassName(…)` expression from a text fragment and
    /// return the class name.
    ///
    /// Handles optional outer parentheses and leading backslashes:
    /// - `new Customer()` → `Some("Customer")`
    /// - `(new \App\Builder())` → `Some("App\\Builder")`
    /// - `$this->foo()`     → `None`
    pub(super) fn extract_new_expression_class(s: &str) -> Option<String> {
        // Strip balanced outer parentheses.
        let inner = if s.starts_with('(') && s.ends_with(')') {
            &s[1..s.len() - 1]
        } else {
            s
        };
        let rest = inner.trim().strip_prefix("new ")?;
        let rest = rest.trim_start();
        // The class name runs until `(`, whitespace, or end-of-string.
        let end = rest
            .find(|c: char| c == '(' || c.is_whitespace())
            .unwrap_or(rest.len());
        let class_name = rest[..end].trim_start_matches('\\');
        if class_name.is_empty()
            || !class_name
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == '\\')
        {
            return None;
        }
        Some(class_name.to_string())
    }

    /// Search backward in `content` for a function definition matching
    /// `func_name` and extract its `@return` type from the docblock.
    pub(super) fn extract_function_return_from_source(
        func_name: &str,
        content: &str,
    ) -> Option<String> {
        // Look for `function funcName(` in the source.
        let pattern = format!("function {}(", func_name);
        let func_pos = content.find(&pattern)?;

        // Search backward from the function definition for a docblock.
        let before = content.get(..func_pos)?;
        let trimmed = before.trim_end();
        if !trimmed.ends_with("*/") {
            return None;
        }
        let open_pos = trimmed.rfind("/**")?;
        let docblock = &trimmed[open_pos..];

        docblock::extract_return_type(docblock)
    }

    /// Scan backward through `content` for a closure or arrow-function
    /// literal assigned to `var_name` and extract the native return type
    /// hint from the source text.
    ///
    /// Handles:
    /// - `$fn = function(…): ReturnType { … }`
    /// - `$fn = function(…) use (…): ReturnType { … }`
    /// - `$fn = fn(…): ReturnType => …`
    ///
    /// Returns `None` if no closure/arrow-function assignment is found
    /// or if there is no return type hint.
    pub(super) fn extract_closure_return_type_from_assignment(
        var_name: &str,
        content: &str,
        cursor_offset: u32,
    ) -> Option<String> {
        let search_area = content.get(..cursor_offset as usize)?;

        // Look for `$fn = function` or `$fn = fn` assignment.
        let assign_prefix = format!("{} = ", var_name);
        let assign_pos = search_area.rfind(&assign_prefix)?;
        let rhs_start = assign_pos + assign_prefix.len();
        let rhs = search_area.get(rhs_start..)?.trim_start();

        // Match `function(…): ReturnType` or `fn(…): ReturnType => …`
        let is_closure = rhs.starts_with("function") && rhs[8..].trim_start().starts_with('(');
        let is_arrow = rhs.starts_with("fn") && rhs[2..].trim_start().starts_with('(');

        if !is_closure && !is_arrow {
            return None;
        }

        // Find the opening `(` of the parameter list.
        let paren_open = rhs.find('(')?;
        // Find the matching `)` by tracking depth.
        let mut depth = 0i32;
        let mut paren_close = None;
        for (i, c) in rhs[paren_open..].char_indices() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        paren_close = Some(paren_open + i);
                        break;
                    }
                }
                _ => {}
            }
        }
        let paren_close = paren_close?;

        // After `)`, look for `: ReturnType`.
        let after_paren = rhs.get(paren_close + 1..)?.trim_start();
        // For closures there may be a `use (…)` clause before the return type.
        let after_use = if after_paren.starts_with("use") {
            let use_paren = after_paren.find('(')?;
            let mut udepth = 0i32;
            let mut use_close = None;
            for (i, c) in after_paren[use_paren..].char_indices() {
                match c {
                    '(' => udepth += 1,
                    ')' => {
                        udepth -= 1;
                        if udepth == 0 {
                            use_close = Some(use_paren + i);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            after_paren.get(use_close? + 1..)?.trim_start()
        } else {
            after_paren
        };

        // Expect `: ReturnType`
        let after_colon = after_use.strip_prefix(':')?.trim_start();
        if after_colon.is_empty() {
            return None;
        }

        // Extract the return type token — stop at `{`, `=>`, or whitespace.
        let end = after_colon
            .find(|c: char| c == '{' || c == '=' || c.is_whitespace())
            .unwrap_or(after_colon.len());
        let ret_type = after_colon[..end].trim();
        if ret_type.is_empty() {
            return None;
        }

        Some(ret_type.to_string())
    }

    /// Resolve the return type of a first-class callable assigned to
    /// `var_name`.
    ///
    /// Scans backward for `$var_name = callable_expr(...)` and resolves
    /// the underlying function or method's return type.  Handles:
    ///
    /// - `$fn = strlen(...)` (standalone function)
    /// - `$fn = $this->method(...)` (instance method)
    /// - `$fn = $obj->method(...)` (instance method on resolved variable)
    /// - `$fn = ClassName::method(...)` (static method)
    /// - `$fn = self::method(...)` / `static::method(...)`
    ///
    /// Returns `None` if no first-class callable assignment is found or
    /// the return type cannot be determined.
    pub(super) fn extract_first_class_callable_return_type(
        var_name: &str,
        content: &str,
        cursor_offset: u32,
        current_class: Option<&ClassInfo>,
        all_classes: &[ClassInfo],
        class_loader: &dyn Fn(&str) -> Option<ClassInfo>,
        function_loader: FunctionLoaderFn<'_>,
    ) -> Option<String> {
        let search_area = content.get(..cursor_offset as usize)?;

        // Look for `$fn = ` assignment.
        let assign_prefix = format!("{} = ", var_name);
        let assign_pos = search_area.rfind(&assign_prefix)?;
        let rhs_start = assign_pos + assign_prefix.len();

        // Extract the RHS up to the next `;`
        let remaining = &content[rhs_start..];
        let semi_pos = find_semicolon_balanced(remaining)?;
        let rhs_text = remaining[..semi_pos].trim();

        // Must end with `(...)` — the first-class callable marker.
        let callable_text = rhs_text.strip_suffix("(...)")?.trim_end();
        if callable_text.is_empty() {
            return None;
        }

        // ── Instance method: `$this->method` or `$obj->method` ──────
        if let Some(pos) = callable_text.rfind("->") {
            let lhs = callable_text[..pos].trim_end_matches('?');
            let method_name = &callable_text[pos + 2..];

            let owner = if lhs == "$this" || lhs == "self" || lhs == "static" {
                current_class.cloned()
            } else if lhs.starts_with('$') {
                // Bare variable LHS like `$factory->create(...)`.
                // Resolve the variable's type via the AST walker.
                Self::resolve_variable_assignment_raw_type(
                    lhs,
                    content,
                    cursor_offset,
                    current_class,
                    all_classes,
                    class_loader,
                    function_loader,
                )
                .and_then(|raw| {
                    let clean = crate::docblock::types::clean_type(&raw);
                    let lookup = short_name(&clean);
                    all_classes
                        .iter()
                        .find(|c| c.name == lookup)
                        .cloned()
                        .or_else(|| class_loader(&clean))
                })
            } else {
                // Non-variable LHS (e.g. chained call) — delegate to
                // the general-purpose text resolver.
                Self::resolve_lhs_to_class(lhs, current_class, all_classes, class_loader)
            };

            if let Some(cls) = owner {
                return Self::resolve_method_return_type(&cls, method_name, class_loader)
                    .map(|ret| replace_self_references(&ret, &cls.name));
            }
            return None;
        }

        // ── Static method: `ClassName::method` / `self::method` ─────
        if let Some(pos) = callable_text.rfind("::") {
            let class_part = &callable_text[..pos];
            let method_name = &callable_text[pos + 2..];

            let owner = if class_part == "self" || class_part == "static" {
                current_class.cloned()
            } else if class_part == "parent" {
                current_class
                    .and_then(|cc| cc.parent_class.as_ref())
                    .and_then(|p| class_loader(p))
            } else {
                let lookup = short_name(class_part);
                all_classes
                    .iter()
                    .find(|c| c.name == lookup)
                    .cloned()
                    .or_else(|| class_loader(class_part))
            };

            if let Some(cls) = owner {
                return Self::resolve_method_return_type(&cls, method_name, class_loader)
                    .map(|ret| replace_self_references(&ret, &cls.name));
            }
            return None;
        }

        // ── Plain function: `strlen`, `array_map`, etc. ─────────────
        if callable_text
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '\\')
            && !callable_text.starts_with('$')
        {
            let func_info = function_loader?(callable_text)?;
            return func_info.return_type;
        }

        None
    }

    /// Resolve a chained array access, trying each candidate raw type
    /// in order until one succeeds through the full segment walk.
    ///
    /// Each candidate raw type string is fed through
    /// `walk_array_segments_and_resolve`.  The first that resolves
    /// through the segment walk and, if it produces a non-empty
    /// `ClassInfo` set, returned immediately.  Returns `None` when no
    /// candidate succeeds.
    pub(super) fn try_chained_array_access_with_candidates<'a>(
        candidates: impl Iterator<Item = String> + 'a,
        segments: &[BracketSegment],
        current_class: Option<&ClassInfo>,
        all_classes: &[ClassInfo],
        class_loader: &dyn Fn(&str) -> Option<ClassInfo>,
    ) -> Option<Vec<ClassInfo>> {
        let current_class_name = current_class.map(|c| c.name.as_str()).unwrap_or("");

        for raw_type in candidates {
            if let Some(result) = Self::walk_array_segments_and_resolve(
                &raw_type,
                segments,
                current_class_name,
                all_classes,
                class_loader,
            ) {
                return Some(result);
            }
        }

        None
    }

    /// Walk bracket segments on a raw type string, then resolve the
    /// resulting type to `ClassInfo`.
    ///
    /// Returns `Some(classes)` when the full segment chain resolves
    /// successfully, or `None` when a segment cannot be applied (e.g.
    /// the array shape does not contain the requested key).
    fn walk_array_segments_and_resolve(
        raw_type: &str,
        segments: &[BracketSegment],
        current_class_name: &str,
        all_classes: &[ClassInfo],
        class_loader: &dyn Fn(&str) -> Option<ClassInfo>,
    ) -> Option<Vec<ClassInfo>> {
        let mut current_type = raw_type.to_string();

        for seg in segments {
            match seg {
                BracketSegment::StringKey(key) => {
                    current_type = docblock::extract_array_shape_value_type(&current_type, key)?;
                }
                BracketSegment::ElementAccess => {
                    current_type = docblock::types::extract_generic_value_type(&current_type)?;
                }
            }
        }

        let cleaned = docblock::clean_type(&current_type);
        let base_name = docblock::types::strip_generics(&cleaned);
        if base_name.is_empty() || docblock::types::is_scalar(&base_name) {
            return None;
        }

        let classes =
            Self::type_hint_to_classes(&cleaned, current_class_name, all_classes, class_loader);
        if classes.is_empty() {
            return None;
        }
        Some(classes)
    }

    // ── Internal helpers for `extract_first_class_callable_return_type` ──

    /// Resolve the return type of a chained call expression from text.
    ///
    /// Splits at the rightmost `->`, resolves the LHS to a `ClassInfo`,
    /// then looks up the method's return type.  Used by
    /// `extract_first_class_callable_return_type` for chained-call LHS
    /// patterns.
    fn resolve_raw_type_from_call_chain(
        callee: &str,
        _args_text: &str,
        current_class: Option<&ClassInfo>,
        all_classes: &[ClassInfo],
        class_loader: &dyn Fn(&str) -> Option<ClassInfo>,
    ) -> Option<String> {
        // Split at the rightmost `->` to get the final method name and
        // the LHS expression that produces the owning object.
        let pos = callee.rfind("->")?;
        // Strip trailing `?` from LHS when the operator was `?->`
        let lhs = callee[..pos]
            .strip_suffix('?')
            .unwrap_or(&callee[..pos])
            .trim();
        let method_name = callee[pos + 2..].trim();

        // Resolve LHS to a class.
        let owner = Self::resolve_lhs_to_class(lhs, current_class, all_classes, class_loader)?;
        Self::resolve_method_return_type(&owner, method_name, class_loader)
    }

    /// Resolve the left-hand side of a chained expression to a `ClassInfo`.
    ///
    /// Handles `$this` / `self` / `static`, `$this->prop`, `new Foo()`,
    /// `(new Foo())`, and recursive chains.  Used by
    /// `resolve_raw_type_from_call_chain` for the text-only path.
    fn resolve_lhs_to_class(
        lhs: &str,
        current_class: Option<&ClassInfo>,
        all_classes: &[ClassInfo],
        class_loader: &dyn Fn(&str) -> Option<ClassInfo>,
    ) -> Option<ClassInfo> {
        // Trim whitespace so that multi-line call chains (where
        // `rfind("->")` leaves trailing newlines/spaces on the LHS)
        // are handled correctly by all downstream checks.
        let lhs = lhs.trim();

        // `$this` / `self` / `static`
        if lhs == "$this" || lhs == "self" || lhs == "static" {
            return current_class.cloned();
        }

        // `(new ClassName(...))` or `new ClassName(...)`
        if let Some(class_name) = Self::extract_new_expression_class(lhs) {
            let lookup = short_name(&class_name);
            return all_classes
                .iter()
                .find(|c| c.name == lookup)
                .cloned()
                .or_else(|| class_loader(&class_name));
        }

        // LHS ends with `)` — it's a call expression.  Recurse.
        if lhs.ends_with(')') {
            let inner = lhs.strip_suffix(')')?;
            // Find matching open paren.
            let mut depth = 0u32;
            let mut open = None;
            for (i, b) in inner.bytes().enumerate().rev() {
                match b {
                    b')' => depth += 1,
                    b'(' => {
                        if depth == 0 {
                            open = Some(i);
                            break;
                        }
                        depth -= 1;
                    }
                    _ => {}
                }
            }
            let open = open?;
            let inner_callee = &inner[..open];
            let inner_args = inner[open + 1..].trim();

            // Inner callee may itself be a chain — recurse.
            let ret_type = Self::resolve_raw_type_from_call_chain(
                inner_callee,
                inner_args,
                current_class,
                all_classes,
                class_loader,
            )
            .or_else(|| {
                // Single-level: `$this->method`
                if let Some(m) = inner_callee
                    .strip_prefix("$this->")
                    .or_else(|| inner_callee.strip_prefix("$this?->"))
                {
                    let owner = current_class?;
                    return Self::resolve_method_return_type(owner, m, class_loader);
                }
                // `ClassName::method`
                if let Some((cls_part, m_part)) = inner_callee.rsplit_once("::") {
                    let resolved = if cls_part == "self" || cls_part == "static" {
                        current_class.cloned()
                    } else {
                        let lookup = short_name(cls_part);
                        all_classes
                            .iter()
                            .find(|c| c.name == lookup)
                            .cloned()
                            .or_else(|| class_loader(cls_part))
                    };
                    if let Some(cls) = resolved {
                        return Self::resolve_method_return_type(&cls, m_part, class_loader);
                    }
                }
                None
            })?;

            // `ret_type` is a type string — resolve it to ClassInfo.
            let clean = crate::docblock::types::clean_type(&ret_type);
            let lookup = short_name(&clean);
            return all_classes
                .iter()
                .find(|c| c.name == lookup)
                .cloned()
                .or_else(|| class_loader(&clean));
        }

        // `$this->prop` — property access
        if let Some(prop) = lhs
            .strip_prefix("$this->")
            .or_else(|| lhs.strip_prefix("$this?->"))
            && prop.chars().all(|c| c.is_alphanumeric() || c == '_')
        {
            let owner = current_class?;
            let type_str = Self::resolve_property_type_hint(owner, prop, class_loader)?;
            let clean = crate::docblock::types::clean_type(&type_str);
            let lookup = short_name(&clean);
            return all_classes
                .iter()
                .find(|c| c.name == lookup)
                .cloned()
                .or_else(|| class_loader(&clean));
        }

        None
    }
}
