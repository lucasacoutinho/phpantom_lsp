//! `@deprecated` usage diagnostics.
//!
//! Walk the precomputed [`SymbolMap`] for a file and flag every reference
//! to a class, method, property, constant, or function that carries a
//! `@deprecated` PHPDoc tag.
//!
//! Diagnostics use `Severity::Hint` with `DiagnosticTag::Deprecated`,
//! which renders as a subtle strikethrough in most editors — visible but
//! not noisy.  The message includes the deprecation reason when one is
//! provided in the tag (e.g. `@deprecated Use NewHelper instead`).

use std::collections::HashMap;

use tower_lsp::lsp_types::*;

use crate::Backend;
use crate::symbol_map::SymbolKind;
use crate::types::ClassInfo;
use crate::virtual_members::resolve_class_fully_cached;

use super::offset_range_to_lsp_range;

impl Backend {
    /// Collect `@deprecated` usage diagnostics for a single file.
    ///
    /// Appends diagnostics to `out`.  The caller is responsible for
    /// publishing them via `textDocument/publishDiagnostics`.
    pub fn collect_deprecated_diagnostics(
        &self,
        uri: &str,
        content: &str,
        out: &mut Vec<Diagnostic>,
    ) {
        // ── Gather context under locks ──────────────────────────────────
        let symbol_map = {
            let maps = match self.symbol_maps.lock() {
                Ok(m) => m,
                Err(_) => return,
            };
            match maps.get(uri) {
                Some(sm) => sm.clone(),
                None => return,
            }
        };

        let file_use_map: HashMap<String, String> = self
            .use_map
            .lock()
            .ok()
            .and_then(|m| m.get(uri).cloned())
            .unwrap_or_default();

        let file_namespace: Option<String> = self
            .namespace_map
            .lock()
            .ok()
            .and_then(|m| m.get(uri).cloned())
            .flatten();

        let local_classes: Vec<ClassInfo> = self
            .ast_map
            .lock()
            .ok()
            .and_then(|m| m.get(uri).cloned())
            .unwrap_or_default();

        let class_loader = self.class_loader_with(&local_classes, &file_use_map, &file_namespace);
        let cache = &self.resolved_class_cache;

        // ── Walk every symbol span ──────────────────────────────────────
        for span in &symbol_map.spans {
            match &span.kind {
                // ── Class references (type hints, new Foo, extends, etc.) ─
                SymbolKind::ClassReference { name, is_fqn } => {
                    let resolved_name = if *is_fqn {
                        name.strip_prefix('\\').unwrap_or(name).to_string()
                    } else {
                        // Resolve through use map / namespace like resolve_class_name
                        resolve_to_fqn(name, &file_use_map, &file_namespace)
                    };

                    if let Some(cls) = self.find_or_load_class(&resolved_name)
                        && let Some(msg) = &cls.deprecation_message
                        && let Some(range) = offset_range_to_lsp_range(
                            content,
                            span.start as usize,
                            span.end as usize,
                        )
                    {
                        out.push(deprecated_diagnostic(range, &cls.name, None, msg));
                    }
                }

                // ── Member accesses ($x->method(), Foo::CONST, etc.) ─────
                SymbolKind::MemberAccess {
                    subject_text,
                    member_name,
                    is_static,
                    is_method_call,
                } => {
                    // Resolve the subject type to a class.
                    let class_name = resolve_subject_to_class_name(
                        subject_text,
                        *is_static,
                        &file_use_map,
                        &file_namespace,
                        &local_classes,
                    );

                    let class_name = match class_name {
                        Some(n) => n,
                        None => continue,
                    };

                    let base_class = match self.find_or_load_class(&class_name) {
                        Some(c) => c,
                        None => continue,
                    };

                    // Resolve with inheritance + virtual members so we find
                    // members from parent classes and traits too.
                    let resolved = resolve_class_fully_cached(&base_class, &class_loader, cache);

                    if *is_method_call {
                        // Check method deprecation
                        if let Some(method) =
                            resolved.methods.iter().find(|m| m.name == *member_name)
                            && let Some(msg) = &method.deprecation_message
                            && let Some(range) = offset_range_to_lsp_range(
                                content,
                                span.start as usize,
                                span.end as usize,
                            )
                        {
                            out.push(deprecated_diagnostic(
                                range,
                                member_name,
                                Some(&resolved.name),
                                msg,
                            ));
                        }
                    } else {
                        // Property or constant access
                        // Try property first
                        if let Some(prop) =
                            resolved.properties.iter().find(|p| p.name == *member_name)
                            && let Some(msg) = &prop.deprecation_message
                            && let Some(range) = offset_range_to_lsp_range(
                                content,
                                span.start as usize,
                                span.end as usize,
                            )
                        {
                            out.push(deprecated_diagnostic(
                                range,
                                member_name,
                                Some(&resolved.name),
                                msg,
                            ));
                            continue;
                        }

                        // Try constant (static access like Foo::BAR)
                        if *is_static
                            && let Some(constant) =
                                resolved.constants.iter().find(|c| c.name == *member_name)
                            && let Some(msg) = &constant.deprecation_message
                            && let Some(range) = offset_range_to_lsp_range(
                                content,
                                span.start as usize,
                                span.end as usize,
                            )
                        {
                            out.push(deprecated_diagnostic(
                                range,
                                member_name,
                                Some(&resolved.name),
                                msg,
                            ));
                        }
                    }
                }

                // ── Standalone function calls ────────────────────────────
                SymbolKind::FunctionCall { name } => {
                    if let Some(func_info) =
                        self.resolve_function_name(name, &file_use_map, &file_namespace)
                        && let Some(msg) = &func_info.deprecation_message
                        && let Some(range) = offset_range_to_lsp_range(
                            content,
                            span.start as usize,
                            span.end as usize,
                        )
                    {
                        out.push(deprecated_diagnostic(range, name, None, msg));
                    }
                }

                // Other symbol kinds are not checked for deprecation.
                _ => {}
            }
        }
    }
}

// ─── Helpers ────────────────────────────────────────────────────────────────

/// Build a deprecated diagnostic.
fn deprecated_diagnostic(
    range: Range,
    symbol_name: &str,
    class_name: Option<&str>,
    deprecation_message: &str,
) -> Diagnostic {
    let display = if let Some(cls) = class_name {
        format!("{}::{}", cls, symbol_name)
    } else {
        symbol_name.to_string()
    };

    let message = if deprecation_message.is_empty() {
        format!("'{}' is deprecated", display)
    } else {
        format!("'{}' is deprecated: {}", display, deprecation_message)
    };

    Diagnostic {
        range,
        severity: Some(DiagnosticSeverity::HINT),
        code: None,
        code_description: None,
        source: Some("phpantom".to_string()),
        message,
        related_information: None,
        tags: Some(vec![DiagnosticTag::DEPRECATED]),
        data: None,
    }
}

/// Resolve an unqualified/qualified class name to a fully-qualified name
/// using the use map and namespace context.
///
/// This mirrors the logic in `Backend::resolve_class_name` but only
/// produces the FQN string without loading the class.
fn resolve_to_fqn(
    name: &str,
    use_map: &HashMap<String, String>,
    namespace: &Option<String>,
) -> String {
    // Fully qualified
    if let Some(stripped) = name.strip_prefix('\\') {
        return stripped.to_string();
    }

    // Unqualified (no backslash)
    if !name.contains('\\') {
        if let Some(fqn) = use_map.get(name) {
            return fqn.clone();
        }
        if let Some(ns) = namespace {
            return format!("{}\\{}", ns, name);
        }
        return name.to_string();
    }

    // Qualified (contains backslash, no leading backslash)
    let first_segment = name.split('\\').next().unwrap_or(name);
    if let Some(fqn_prefix) = use_map.get(first_segment) {
        let rest = &name[first_segment.len()..];
        return format!("{}{}", fqn_prefix, rest);
    }
    if let Some(ns) = namespace {
        return format!("{}\\{}", ns, name);
    }
    name.to_string()
}

/// Resolve a member access subject text to a class FQN.
///
/// Handles:
/// - `self`, `static`, `parent` → resolve from enclosing class
/// - `ClassName` (static access) → resolve via use map
/// - `$this` → resolve from enclosing class
/// - Other `$variable` subjects are not resolved here (would need
///   variable type resolution which is expensive; deferred to a
///   future enhancement).
fn resolve_subject_to_class_name(
    subject_text: &str,
    is_static: bool,
    file_use_map: &HashMap<String, String>,
    file_namespace: &Option<String>,
    local_classes: &[ClassInfo],
) -> Option<String> {
    let trimmed = subject_text.trim();

    match trimmed {
        "self" | "static" => {
            // Find the enclosing class in this file
            find_enclosing_class_fqn(local_classes, file_namespace)
        }
        "parent" => {
            // Find the enclosing class that actually has a parent.
            // Prefer a class with `parent_class` set — that's the one
            // where `parent::` is meaningful.  Fall back to the first
            // non-anonymous class if none has a parent (shouldn't happen
            // in valid code, but be defensive).
            let cls = local_classes
                .iter()
                .find(|c| !c.name.starts_with("__anonymous@") && c.parent_class.is_some())
                .or_else(|| {
                    local_classes
                        .iter()
                        .find(|c| !c.name.starts_with("__anonymous@"))
                });
            cls.and_then(|c| {
                c.parent_class
                    .as_ref()
                    .map(|p| resolve_to_fqn(p, file_use_map, file_namespace))
            })
        }
        "$this" => find_enclosing_class_fqn(local_classes, file_namespace),
        _ if is_static && !trimmed.starts_with('$') => {
            // Static access on a class name: `ClassName::method()`
            Some(resolve_to_fqn(trimmed, file_use_map, file_namespace))
        }
        _ if trimmed.starts_with('$') => {
            // Variable access — would need full type resolution.
            // We skip these to avoid false negatives (but never
            // produce false positives).
            None
        }
        _ => {
            // Could be a function return or expression — skip for now
            None
        }
    }
}

/// Find the FQN of the first non-anonymous class in the file (heuristic
/// for the "enclosing class" in single-class-per-file projects).
fn find_enclosing_class_fqn(
    local_classes: &[ClassInfo],
    file_namespace: &Option<String>,
) -> Option<String> {
    // Skip anonymous classes
    let cls = local_classes
        .iter()
        .find(|c| !c.name.starts_with("__anonymous@"))?;
    if let Some(ns) = file_namespace {
        Some(format!("{}\\{}", ns, cls.name))
    } else {
        Some(cls.name.clone())
    }
}
