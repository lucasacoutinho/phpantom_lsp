//! Hover support (`textDocument/hover`).
//!
//! This module resolves the symbol under the cursor and returns a
//! human-readable description including type information, method
//! signatures, and docblock descriptions.
//!
//! The implementation reuses the same symbol-map lookup that powers
//! go-to-definition, and the same type-resolution pipeline that
//! powers completion.

use tower_lsp::lsp_types::*;

use crate::Backend;
use crate::completion::resolver::ResolutionCtx;
use crate::docblock::extract_template_params_full;
use crate::symbol_map::{SymbolKind, SymbolSpan, VarDefKind};
use crate::types::*;
use crate::util::{find_class_at_offset, offset_to_position, position_to_offset};

impl Backend {
    /// Handle a `textDocument/hover` request.
    ///
    /// Returns `Some(Hover)` when the symbol under the cursor can be
    /// resolved to a meaningful description, or `None` when resolution
    /// fails or the cursor is not on a navigable symbol.
    pub fn handle_hover(&self, uri: &str, content: &str, position: Position) -> Option<Hover> {
        let offset = position_to_offset(content, position);

        // Fast path: consult precomputed symbol map.
        if let Some(symbol) = self.lookup_symbol_map_for_hover(uri, offset)
            && let Some(Some(mut hover)) =
                crate::util::catch_panic_unwind_safe("hover", uri, Some(position), || {
                    self.hover_from_symbol(&symbol, uri, content, offset)
                })
        {
            hover.range = Some(symbol_span_to_range(content, &symbol));
            return Some(hover);
        }

        // Retry with offset - 1 for cursor at end-of-token (same
        // heuristic as go-to-definition).
        if offset > 0
            && let Some(symbol) = self.lookup_symbol_map_for_hover(uri, offset - 1)
            && let Some(Some(mut hover)) =
                crate::util::catch_panic_unwind_safe("hover", uri, Some(position), || {
                    self.hover_from_symbol(&symbol, uri, content, offset - 1)
                })
        {
            hover.range = Some(symbol_span_to_range(content, &symbol));
            return Some(hover);
        }

        None
    }

    /// Look up the symbol at the given byte offset for hover purposes.
    fn lookup_symbol_map_for_hover(&self, uri: &str, offset: u32) -> Option<SymbolSpan> {
        let maps = self.symbol_maps.lock().ok()?;
        let map = maps.get(uri)?;
        map.lookup(offset).cloned()
    }

    /// Dispatch a symbol-map hit to the appropriate hover path.
    fn hover_from_symbol(
        &self,
        symbol: &SymbolSpan,
        uri: &str,
        content: &str,
        cursor_offset: u32,
    ) -> Option<Hover> {
        let kind = &symbol.kind;
        let ctx = self.file_context(uri);
        let current_class = find_class_at_offset(&ctx.classes, cursor_offset);
        let class_loader = self.class_loader(&ctx);
        let function_loader = self.function_loader(&ctx);

        match kind {
            SymbolKind::Variable { name } => {
                // Suppress hover when the cursor is on a variable at its
                // definition site (parameter, foreach binding, catch, etc.)
                // — the type is already visible in the signature.
                // Assignments are the exception: the RHS type is not
                // obvious from the source text, so hover is useful there.
                if let Some(def_kind) = self.lookup_var_def_kind_at(uri, name, cursor_offset)
                    && !matches!(def_kind, VarDefKind::Assignment)
                {
                    return None;
                }
                self.hover_variable(name, uri, content, cursor_offset, current_class, &ctx)
            }

            SymbolKind::MemberAccess {
                subject_text,
                member_name,
                is_static,
                is_method_call,
            } => {
                let rctx = ResolutionCtx {
                    current_class,
                    all_classes: &ctx.classes,
                    content,
                    cursor_offset,
                    class_loader: &class_loader,
                    resolved_class_cache: Some(&self.resolved_class_cache),
                    function_loader: Some(&function_loader),
                };

                let access_kind = if *is_static {
                    AccessKind::DoubleColon
                } else {
                    AccessKind::Arrow
                };

                let candidates = crate::completion::resolver::resolve_target_classes(
                    subject_text,
                    access_kind,
                    &rctx,
                );

                for target_class in &candidates {
                    let merged = crate::virtual_members::resolve_class_fully_cached(
                        target_class,
                        &class_loader,
                        &self.resolved_class_cache,
                    );

                    if *is_method_call {
                        if let Some(method) = merged
                            .methods
                            .iter()
                            .find(|m| m.name.eq_ignore_ascii_case(member_name))
                        {
                            return Some(self.hover_for_method(method, &merged));
                        }
                    } else {
                        // Try property first, then constant
                        if let Some(prop) =
                            merged.properties.iter().find(|p| p.name == *member_name)
                        {
                            return Some(self.hover_for_property(prop, &merged));
                        }
                        if let Some(constant) =
                            merged.constants.iter().find(|c| c.name == *member_name)
                        {
                            return Some(self.hover_for_constant(constant, &merged));
                        }
                        // Could also be a method reference without call parens
                        if let Some(method) = merged
                            .methods
                            .iter()
                            .find(|m| m.name.eq_ignore_ascii_case(member_name))
                        {
                            return Some(self.hover_for_method(method, &merged));
                        }
                    }
                }
                None
            }

            SymbolKind::ClassReference { name, is_fqn } => {
                // Check whether this class reference is in a `new ClassName` context.
                // If so, show the __construct method hover instead of the class hover.
                let before = &content[..symbol.start as usize];
                let trimmed = before.trim_end();
                let is_new_context = trimmed.ends_with("new")
                    && trimmed
                        .as_bytes()
                        .get(trimmed.len().wrapping_sub(4))
                        .is_none_or(|&b| !b.is_ascii_alphanumeric() && b != b'_');

                let resolved_name;
                let lookup_name = if *is_fqn {
                    resolved_name = format!("\\{}", name);
                    &resolved_name
                } else {
                    name.as_str()
                };

                if is_new_context && let Some(cls) = class_loader(lookup_name) {
                    let merged = crate::virtual_members::resolve_class_fully_cached(
                        &cls,
                        &class_loader,
                        &self.resolved_class_cache,
                    );
                    if let Some(constructor) = merged
                        .methods
                        .iter()
                        .find(|m| m.name.eq_ignore_ascii_case("__construct"))
                    {
                        return Some(self.hover_for_method(constructor, &merged));
                    }
                }

                self.hover_class_reference(
                    lookup_name,
                    *is_fqn,
                    uri,
                    &ctx,
                    &class_loader,
                    cursor_offset,
                )
            }

            SymbolKind::ClassDeclaration { .. } => {
                // The user is already at the definition site — showing
                // hover here would just repeat what they can already see.
                None
            }

            SymbolKind::FunctionCall { name } => {
                self.hover_function_call(name, &ctx, &function_loader)
            }

            SymbolKind::SelfStaticParent { keyword } => {
                // `$this` is represented as SelfStaticParent { keyword: "static" }
                // in the symbol map.  Detect it by checking the source text.
                // The cursor may land anywhere inside the `$this` token (5 bytes),
                // so look up to 4 bytes back for the `$` and check for `$this`.
                let is_this = keyword == "static" && {
                    let off = cursor_offset as usize;
                    let search_start = off.saturating_sub(4);
                    let window = content.get(search_start..off + 5).unwrap_or("");
                    window.contains("$this")
                };

                let resolved = match keyword.as_str() {
                    "self" | "static" => current_class.cloned(),
                    "parent" => current_class
                        .and_then(|cc| cc.parent_class.as_ref())
                        .and_then(|parent_name| class_loader(parent_name)),
                    _ => None,
                };
                if let Some(cls) = resolved {
                    let mut lines = Vec::new();

                    if let Some(desc) = extract_docblock_description(cls.class_docblock.as_deref())
                    {
                        lines.push(desc);
                    }

                    if cls.is_deprecated {
                        lines.push("**@deprecated**".to_string());
                    }

                    let ns_line = namespace_line(&cls.file_namespace);
                    if is_this {
                        lines.push(format!(
                            "```php\n<?php\n{}$this = {}\n```",
                            ns_line, cls.name
                        ));
                    } else {
                        lines.push(format!(
                            "```php\n<?php\n{}{} = {}\n```",
                            ns_line, keyword, cls.name
                        ));
                    }

                    Some(make_hover(lines.join("\n\n")))
                } else {
                    let display = if is_this { "$this" } else { keyword };
                    Some(make_hover(format!("```php\n<?php\n{}\n```", display)))
                }
            }

            SymbolKind::ConstantReference { name } => {
                // Try to find the constant in global defines
                let _defines = self.global_defines.lock().ok()?;
                Some(make_hover(format!("```php\n<?php\nconst {};\n```", name)))
            }
        }
    }

    /// Produce hover information for a variable.
    fn hover_variable(
        &self,
        name: &str,
        _uri: &str,
        content: &str,
        cursor_offset: u32,
        current_class: Option<&ClassInfo>,
        ctx: &FileContext,
    ) -> Option<Hover> {
        let var_name = format!("${}", name);

        // $this resolves to the enclosing class
        if name == "this" {
            if let Some(cc) = current_class {
                let ns_line = namespace_line(&cc.file_namespace);
                return Some(make_hover(format!(
                    "```php\n<?php\n{}$this = {}\n```",
                    ns_line, cc.name
                )));
            }
            return Some(make_hover("```php\n<?php\n$this\n```".to_string()));
        }

        let class_loader = self.class_loader(ctx);
        let function_loader = self.function_loader(ctx);

        // Use the dummy class approach same as completion for top-level code
        let dummy_class;
        let effective_class = match current_class {
            Some(cc) => cc,
            None => {
                dummy_class = ClassInfo::default();
                &dummy_class
            }
        };

        let types = Self::resolve_variable_types(
            &var_name,
            effective_class,
            &ctx.classes,
            content,
            cursor_offset,
            &class_loader,
            Some(&function_loader as &dyn Fn(&str) -> Option<FunctionInfo>),
        );

        if types.is_empty() {
            return Some(make_hover(format!("```php\n<?php\n{}\n```", var_name)));
        }

        let type_names: Vec<&str> = types.iter().map(|c| c.name.as_str()).collect();
        let type_str = type_names.join("|");

        Some(make_hover(format!(
            "```php\n<?php\n{} = {}\n```",
            var_name, type_str
        )))
    }

    /// Produce hover information for a class reference.
    fn hover_class_reference(
        &self,
        name: &str,
        _is_fqn: bool,
        uri: &str,
        _ctx: &FileContext,
        class_loader: &dyn Fn(&str) -> Option<ClassInfo>,
        cursor_offset: u32,
    ) -> Option<Hover> {
        // The caller already prepends `\` for FQN names, so we can
        // call class_loader directly.
        let class_info = class_loader(name);

        if let Some(cls) = class_info {
            Some(self.hover_for_class_info(&cls))
        } else {
            // Check whether this is a template parameter in scope.
            let bare_name = name.strip_prefix('\\').unwrap_or(name);
            if let Some(tpl) = self.find_template_def_for_hover(uri, bare_name, cursor_offset) {
                return Some(tpl);
            }
            // Unknown class, just show the name
            Some(make_hover(format!(
                "```php\n<?php\nclass {};\n```",
                bare_name
            )))
        }
    }

    /// Check whether `name` is a `@template` parameter in scope at
    /// `cursor_offset` and, if so, produce a hover showing the template
    /// name and its upper bound.
    fn find_template_def_for_hover(
        &self,
        uri: &str,
        name: &str,
        cursor_offset: u32,
    ) -> Option<Hover> {
        let maps = self.symbol_maps.lock().ok()?;
        let map = maps.get(uri)?;
        let def = map.find_template_def(name, cursor_offset)?;

        let bound_display = if let Some(ref bound) = def.bound {
            format!(" of `{}`", bound)
        } else {
            String::new()
        };

        Some(make_hover(format!(
            "**{}** `{}`{}",
            def.variance.tag_name(),
            def.name,
            bound_display
        )))
    }

    /// Produce hover information for a function call.
    fn hover_function_call(
        &self,
        name: &str,
        _ctx: &FileContext,
        function_loader: &dyn Fn(&str) -> Option<FunctionInfo>,
    ) -> Option<Hover> {
        if let Some(func) = function_loader(name) {
            Some(hover_for_function(&func))
        } else {
            Some(make_hover(format!(
                "```php\n<?php\nfunction {}();\n```",
                name
            )))
        }
    }

    /// Build hover content for a method.
    fn hover_for_method(&self, method: &MethodInfo, owner: &ClassInfo) -> Hover {
        let visibility = format_visibility(method.visibility);
        let static_kw = if method.is_static { "static " } else { "" };
        let native_params = format_native_params(&method.parameters);

        // Use native return type in the code block, effective type as docblock annotation.
        let native_ret = method
            .native_return_type
            .as_ref()
            .map(|r| format!(": {}", r))
            .unwrap_or_default();

        let member_line = format!(
            "{}{}function {}({}){};",
            visibility, static_kw, method.name, native_params, native_ret
        );

        let mut lines = Vec::new();

        if let Some(ref desc) = method.description {
            lines.push(desc.clone());
        }

        if method.is_deprecated {
            lines.push("**@deprecated**".to_string());
        }

        if let Some(ref url) = method.link {
            lines.push(format!("[{}]({})", url, url));
        }

        // Build the readable param/return section as markdown.
        if let Some(section) = build_param_return_section(
            &method.parameters,
            method.return_type.as_deref(),
            method.native_return_type.as_deref(),
            method.return_description.as_deref(),
        ) {
            lines.push(section);
        }

        let code = build_class_member_block(
            &owner.name,
            &owner.file_namespace,
            owner_kind_keyword(owner),
            &owner_name_suffix(owner),
            &member_line,
        );
        lines.push(code);

        make_hover(lines.join("\n\n"))
    }

    /// Build hover content for a property.
    fn hover_for_property(&self, property: &PropertyInfo, owner: &ClassInfo) -> Hover {
        let visibility = format_visibility(property.visibility);
        let static_kw = if property.is_static { "static " } else { "" };

        // Use native type hint in the code block, effective type as docblock annotation.
        let native_type = property
            .native_type_hint
            .as_ref()
            .map(|t| format!("{} ", t))
            .unwrap_or_default();

        let member_line = format!(
            "{}{}{}${};",
            visibility, static_kw, native_type, property.name
        );

        // Build the docblock annotation showing the effective type
        // when it differs from the native one.
        let var_annotation = build_var_annotation(
            property.type_hint.as_deref(),
            property.native_type_hint.as_deref(),
        );

        let mut lines = Vec::new();

        if let Some(ref desc) = property.description {
            lines.push(desc.clone());
        }

        if property.is_deprecated {
            lines.push("**@deprecated**".to_string());
        }

        let code = build_class_member_block_with_var(
            &owner.name,
            &owner.file_namespace,
            owner_kind_keyword(owner),
            &owner_name_suffix(owner),
            &var_annotation,
            &member_line,
        );
        lines.push(code);

        make_hover(lines.join("\n\n"))
    }

    /// Build hover content for a class constant.
    fn hover_for_constant(&self, constant: &ConstantInfo, owner: &ClassInfo) -> Hover {
        let member_line = if constant.is_enum_case {
            if let Some(ref val) = constant.enum_value {
                format!("case {} = {};", constant.name, val)
            } else {
                format!("case {};", constant.name)
            }
        } else {
            let visibility = format_visibility(constant.visibility);
            let type_hint = constant
                .type_hint
                .as_ref()
                .map(|t| format!(": {}", t))
                .unwrap_or_default();
            format!("{}const {}{};", visibility, constant.name, type_hint)
        };

        let mut lines = Vec::new();

        if let Some(ref desc) = constant.description {
            lines.push(desc.clone());
        }

        if constant.is_deprecated {
            lines.push("**@deprecated**".to_string());
        }

        // Constants don't have a native vs effective type split, so no doc annotation.
        let code = build_class_member_block(
            &owner.name,
            &owner.file_namespace,
            owner_kind_keyword(owner),
            &owner_name_suffix(owner),
            &member_line,
        );
        lines.push(code);

        make_hover(lines.join("\n\n"))
    }

    /// Build hover content for a class/interface/trait/enum.
    fn hover_for_class_info(&self, cls: &ClassInfo) -> Hover {
        let kind_str = match cls.kind {
            ClassLikeKind::Class => {
                if cls.is_abstract {
                    "abstract class"
                } else if cls.is_final {
                    "final class"
                } else {
                    "class"
                }
            }
            ClassLikeKind::Interface => "interface",
            ClassLikeKind::Trait => "trait",
            ClassLikeKind::Enum => "enum",
        };

        let mut extends_implements = String::new();

        // For interfaces, `parent_class` is the first element of
        // `interfaces` (both come from the same `extends` clause),
        // so skip it to avoid duplicating the name.
        if cls.kind != ClassLikeKind::Interface
            && let Some(ref parent) = cls.parent_class
        {
            extends_implements.push_str(&format!(" extends {}", short_name(parent)));
        }

        if !cls.interfaces.is_empty() {
            let keyword = if cls.kind == ClassLikeKind::Interface {
                "extends"
            } else {
                "implements"
            };
            let short_ifaces: Vec<&str> = cls.interfaces.iter().map(|i| short_name(i)).collect();
            extends_implements.push_str(&format!(" {} {}", keyword, short_ifaces.join(", ")));
        }

        let signature = format!("{} {}{}", kind_str, cls.name, extends_implements);
        let ns_line = namespace_line(&cls.file_namespace);

        let mut lines = Vec::new();

        if let Some(desc) = extract_docblock_description(cls.class_docblock.as_deref()) {
            lines.push(desc);
        }

        if cls.is_deprecated {
            lines.push("**@deprecated**".to_string());
        }

        if let Some(ref url) = cls.link {
            lines.push(format!("[{}]({})", url, url));
        }

        // Show template parameters with variance and bounds.
        if let Some(ref docblock) = cls.class_docblock {
            let tpl_entries: Vec<String> = extract_template_params_full(docblock)
                .into_iter()
                .map(|(name, bound, variance)| {
                    let bound_display = bound
                        .map(|b| format!(" of `{}`", shorten_type_string(&b)))
                        .unwrap_or_default();
                    format!("**{}** `{}`{}", variance.tag_name(), name, bound_display)
                })
                .collect();
            if !tpl_entries.is_empty() {
                lines.push(tpl_entries.join("  \n"));
            }
        }

        lines.push(format!("```php\n<?php\n{}{}\n```", ns_line, signature));

        make_hover(lines.join("\n\n"))
    }
}

// ─── Free helper functions ──────────────────────────────────────────────────

/// Convert a `SymbolSpan`'s byte offsets to an LSP `Range`.
fn symbol_span_to_range(content: &str, symbol: &SymbolSpan) -> Range {
    Range {
        start: offset_to_position(content, symbol.start as usize),
        end: offset_to_position(content, symbol.end as usize),
    }
}

/// Create a `Hover` with Markdown content.
fn make_hover(contents: String) -> Hover {
    Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: contents,
        }),
        range: None,
    }
}

/// Format a visibility keyword.
fn format_visibility(vis: Visibility) -> &'static str {
    match vis {
        Visibility::Public => "public ",
        Visibility::Protected => "protected ",
        Visibility::Private => "private ",
    }
}

/// Format a parameter list for display using effective (docblock-overridden) types.
///
/// Used in the FQN header line and signature help where the richer
/// docblock type information is most useful.
#[allow(dead_code)]
fn format_params(params: &[ParameterInfo]) -> String {
    format_params_inner(params, false)
}

/// Format a parameter list using native PHP type hints only.
///
/// Used inside `<?php` code blocks so the displayed declaration matches
/// what the actual PHP source code looks like.
fn format_native_params(params: &[ParameterInfo]) -> String {
    format_params_inner(params, true)
}

/// Shared implementation for parameter formatting.
///
/// When `use_native` is true, uses `native_type_hint` (falling back to
/// `type_hint` when no native hint is stored — e.g. for virtual members
/// synthesised from docblocks).  Otherwise uses `type_hint` (effective).
fn format_params_inner(params: &[ParameterInfo], use_native: bool) -> String {
    params
        .iter()
        .map(|p| {
            let mut parts = Vec::new();
            let hint = if use_native {
                p.native_type_hint.as_ref()
            } else {
                p.type_hint.as_ref()
            };
            if let Some(th) = hint {
                parts.push(th.clone());
            }
            if p.is_variadic {
                parts.push(format!("...{}", p.name));
            } else if p.is_reference {
                parts.push(format!("&{}", p.name));
            } else {
                parts.push(p.name.clone());
            }
            let param_str = parts.join(" ");
            if !p.is_required && !p.is_variadic {
                if let Some(ref dv) = p.default_value {
                    format!("{} = {}", param_str, dv)
                } else {
                    format!("{} = ...", param_str)
                }
            } else {
                param_str
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Build a `namespace Foo;\n` line for use inside PHP code blocks.
/// Returns an empty string when the namespace is global (None).
fn namespace_line(namespace: &Option<String>) -> String {
    if let Some(ns) = namespace {
        format!("namespace {};\n", ns)
    } else {
        String::new()
    }
}

/// Build a `@var` docblock annotation when the effective type differs from
/// the native type.  Returns `None` when they are identical or when there
/// is no effective type.
fn build_var_annotation(effective: Option<&str>, native: Option<&str>) -> Option<String> {
    let eff = effective?;
    // When there is no native type hint, `mixed` is the implicit type
    // in PHP — showing `@var mixed` would be noise.
    if native.is_none() && eff == "mixed" {
        return None;
    }
    if native.is_some_and(|n| types_equivalent(n, eff)) {
        return None;
    }
    Some(format!("@var {}", shorten_type_string(eff)))
}

/// Build a readable markdown section showing parameter and return type
/// information.
///
/// Produces output like:
///
/// ```text
/// **$callback** `(callable(TItem): TReturn)|null`
///     Callback function to run for each element.
/// **$array** `array<string|int, TItem>`
///     An array to run through the callback function.
/// **$arrays** `array<string|int, TItem>` ...
/// **return** `array<string|int, TReturn>`
///     an array containing all the elements of arr1 ...
/// ```
///
/// A parameter entry is emitted when:
///   - the effective type differs from the native type, OR
///   - the parameter has a description.
///
/// When types are the same, the type is shown alongside the description.
/// When types differ but there is no description, only the type is shown.
///
/// A return entry is emitted when:
///   - the effective return type differs from the native return type, OR
///   - there is a return description.
///
/// Returns `None` when there is nothing to show.
fn build_param_return_section(
    params: &[ParameterInfo],
    effective_return: Option<&str>,
    native_return: Option<&str>,
    return_description: Option<&str>,
) -> Option<String> {
    let mut entries = Vec::new();

    for p in params {
        let type_differs = match (p.type_hint.as_deref(), p.native_type_hint.as_deref()) {
            (Some(eff), Some(nat)) => !types_equivalent(eff, nat),
            (Some(eff), None) => eff != "mixed",
            _ => false,
        };
        let has_desc = p.description.as_ref().is_some_and(|d| !d.is_empty());

        if !type_differs && !has_desc {
            continue;
        }

        let mut entry = format!("**{}**", p.name);
        if type_differs {
            if let Some(ref eff) = p.type_hint {
                entry.push_str(&format!(" `{}`", shorten_type_string(eff)));
            }
            if p.is_variadic {
                entry.push_str(" ...");
            }
            if has_desc {
                entry.push_str("  \n\u{00a0}\u{00a0}\u{00a0}\u{00a0}");
                entry.push_str(p.description.as_deref().unwrap());
            }
        } else if has_desc {
            // Types match — show description directly after the name.
            entry.push(' ');
            entry.push_str(p.description.as_deref().unwrap());
        }
        entries.push(entry);
    }

    // return entry
    let ret_type_differs = match (effective_return, native_return) {
        (Some(eff), Some(nat)) => !types_equivalent(eff, nat),
        (Some(eff), None) => eff != "mixed",
        _ => false,
    };
    let has_ret_desc = return_description.is_some_and(|d| !d.is_empty());

    if ret_type_differs || has_ret_desc {
        let mut entry = String::from("**return**");
        if ret_type_differs {
            if let Some(eff) = effective_return {
                entry.push_str(&format!(" `{}`", shorten_type_string(eff)));
            }
            if has_ret_desc {
                entry.push_str("  \n\u{00a0}\u{00a0}\u{00a0}\u{00a0}");
                entry.push_str(return_description.unwrap());
            }
        } else if has_ret_desc {
            entry.push(' ');
            entry.push_str(return_description.unwrap());
        }
        entries.push(entry);
    }

    if entries.is_empty() {
        None
    } else {
        Some(entries.join("  \n"))
    }
}

/// Build a PHP code block wrapping a member inside its owning class.
///
/// Produces a fenced `php` block containing:
///
///   - `<?php`
///   - `namespace Foo;` (omitted when global)
///   - `class ShortName {`
///   - (optional) `    /** @var effective_type */`
///   - `    public string $name;`
///   - `}`
///
/// When `doc_annotation` is `None`, the docblock line is omitted.
fn build_class_member_block(
    owner_name: &str,
    owner_namespace: &Option<String>,
    kind_keyword: &str,
    name_suffix: &str,
    member_line: &str,
) -> String {
    let mut body = String::new();
    let ns_line = namespace_line(owner_namespace);
    body.push_str("```php\n<?php\n");
    body.push_str(&ns_line);
    body.push_str(kind_keyword);
    body.push(' ');
    body.push_str(owner_name);
    body.push_str(name_suffix);
    body.push_str(" {\n    ");
    body.push_str(member_line);
    body.push_str("\n}\n```");
    body
}

/// Return the PHP keyword for a class-like owner.
///
/// Produces `"class"`, `"interface"`, `"trait"`, or `"enum"`.
fn owner_kind_keyword(owner: &ClassInfo) -> &'static str {
    match owner.kind {
        ClassLikeKind::Interface => "interface",
        ClassLikeKind::Trait => "trait",
        ClassLikeKind::Enum => "enum",
        _ => "class",
    }
}

/// Return the suffix after the owner name for backed enums (e.g. `": string"`).
///
/// Returns an empty string for non-enums and unit enums.
fn owner_name_suffix(owner: &ClassInfo) -> String {
    if let Some(ref bt) = owner.backed_type {
        format!(": {}", bt)
    } else {
        String::new()
    }
}

/// Build a PHP code block wrapping a member inside its owning class,
/// with an optional single-line `/** @var ... */` annotation above it.
///
/// Used for properties where the effective (docblock) type differs from
/// the native PHP type hint.
fn build_class_member_block_with_var(
    owner_name: &str,
    owner_namespace: &Option<String>,
    kind_keyword: &str,
    name_suffix: &str,
    var_annotation: &Option<String>,
    member_line: &str,
) -> String {
    let mut body = String::new();
    let ns_line = namespace_line(owner_namespace);
    body.push_str("```php\n<?php\n");
    body.push_str(&ns_line);
    body.push_str(kind_keyword);
    body.push(' ');
    body.push_str(owner_name);
    body.push_str(name_suffix);
    body.push_str(" {\n");
    if let Some(annotation) = var_annotation {
        body.push_str("    /** ");
        body.push_str(annotation);
        body.push_str(" */\n");
    }
    body.push_str("    ");
    body.push_str(member_line);
    body.push_str("\n}\n```");
    body
}

/// Build hover content for a standalone function.
fn hover_for_function(func: &FunctionInfo) -> Hover {
    let native_params = format_native_params(&func.parameters);

    // Use native return type in the code block.
    let native_ret = func
        .native_return_type
        .as_ref()
        .map(|r| format!(": {}", r))
        .unwrap_or_default();

    let signature = format!("function {}({}){}", func.name, native_params, native_ret);
    let ns_line = namespace_line(&func.namespace);

    let mut lines = Vec::new();

    if let Some(ref desc) = func.description {
        lines.push(desc.clone());
    }

    if func.is_deprecated {
        lines.push("**@deprecated**".to_string());
    }

    if let Some(ref url) = func.link {
        lines.push(format!("[{}]({})", url, url));
    }

    // Build the readable param/return section as markdown.
    if let Some(section) = build_param_return_section(
        &func.parameters,
        func.return_type.as_deref(),
        func.native_return_type.as_deref(),
        func.return_description.as_deref(),
    ) {
        lines.push(section);
    }

    // Build a clean code block with just the signature.
    let code = format!("```php\n<?php\n{}{};\n```", ns_line, signature);
    lines.push(code);

    make_hover(lines.join("\n\n"))
}

/// Extract the trailing description from a `@var` tag line.
///
/// Handles formats like:
///   - `/** @var list<Pen> The batches */`       → `Some("The batches")`
///   - `/** @var list<Pen> $batch The batches */` → `Some("The batches")`
///   - `/** @var list<Pen> */`                    → `None`
///
/// Only looks at the `@var` line itself.  For multi-line docblocks where
/// the description precedes the `@var` tag, use `extract_docblock_description`.
pub(crate) fn extract_var_description(docblock: &str) -> Option<String> {
    let inner = docblock
        .trim()
        .strip_prefix("/**")
        .unwrap_or(docblock)
        .strip_suffix("*/")
        .unwrap_or(docblock);

    for line in inner.lines() {
        let trimmed = line.trim().trim_start_matches('*').trim();
        if let Some(rest) = trimmed.strip_prefix("@var") {
            let rest = rest.trim_start();
            if rest.is_empty() {
                return None;
            }
            // Skip past the type token (respecting `<…>` nesting).
            let after_type = skip_type_token(rest);
            let after_type = after_type.trim_start();
            if after_type.is_empty() {
                return None;
            }
            // Skip an optional `$variable` name.
            let after_var = if after_type.starts_with('$') {
                after_type
                    .split_once(|c: char| c.is_whitespace())
                    .map(|(_, rest)| rest.trim_start())
                    .unwrap_or("")
            } else {
                after_type
            };
            if after_var.is_empty() {
                return None;
            }
            return Some(after_var.to_string());
        }
    }
    None
}

/// Skip past a type token in a docblock string, respecting `<…>` nesting.
///
/// Returns the remainder of the string after the type token.
fn skip_type_token(s: &str) -> &str {
    let mut depth = 0i32;
    let mut end = 0;
    for (i, c) in s.char_indices() {
        match c {
            '<' | '(' | '{' => depth += 1,
            '>' | ')' | '}' => depth -= 1,
            _ if c.is_whitespace() && depth == 0 => {
                end = i;
                break;
            }
            _ => {}
        }
        end = i + c.len_utf8();
    }
    &s[end..]
}

/// Extract the human-readable description text from a raw docblock string.
///
/// Strips the `/**` and `*/` delimiters, leading `*` characters, and all
/// `@tag` lines. Returns `None` if no description text remains.
pub(crate) fn extract_docblock_description(docblock: Option<&str>) -> Option<String> {
    let raw = docblock?;
    let inner = raw
        .trim()
        .strip_prefix("/**")
        .unwrap_or(raw)
        .strip_suffix("*/")
        .unwrap_or(raw);

    let mut lines = Vec::new();
    for line in inner.lines() {
        let trimmed = line.trim().trim_start_matches('*').trim();

        // Skip empty lines at the very start
        if lines.is_empty() && trimmed.is_empty() {
            continue;
        }

        // Stop at the first @tag
        if trimmed.starts_with('@') {
            break;
        }

        lines.push(trimmed.to_string());
    }

    // Trim trailing empty lines
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }

    if lines.is_empty() {
        None
    } else {
        Some(lines.join("\n"))
    }
}

/// Shorten all namespace-qualified class names in a type string to their
/// short (unqualified) form.
///
/// Handles union (`|`), intersection (`&`), nullable (`?`), and generic
/// (`<`, `,`) type syntax.  For example:
///
///   - `"App\\Models\\User"` → `"User"`
///   - `"list<App\\Models\\User>"` → `"list<User>"`
///   - `"App\\Foo|App\\Bar|null"` → `"Foo|Bar|null"`
fn shorten_type_string(ty: &str) -> String {
    let mut result = String::with_capacity(ty.len());
    let mut segment_start = 0;
    let bytes = ty.as_bytes();

    for (i, &b) in bytes.iter().enumerate() {
        if matches!(
            b,
            b'|' | b'&' | b'<' | b'>' | b',' | b' ' | b'?' | b'{' | b'}' | b':' | b'(' | b')'
        ) {
            if i > segment_start {
                result.push_str(short_name(&ty[segment_start..i]));
            }
            result.push(b as char);
            segment_start = i + 1;
        }
    }
    // Flush trailing segment.
    if segment_start < ty.len() {
        result.push_str(short_name(&ty[segment_start..]));
    }
    result
}

/// Check whether two type strings refer to the same type, ignoring
/// namespace qualification differences.
///
/// Returns `true` when the only difference between `a` and `b` is that
/// one uses a fully-qualified class name (e.g. `App\Models\User`) while
/// the other uses the short name (`User`).  Handles nullable (`?`),
/// union (`|`), and intersection (`&`) types by comparing each component
/// after stripping namespace prefixes and a leading `\`.
fn types_equivalent(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }

    // Strip nullable `?` prefix from both sides.
    let a = a.strip_prefix('?').unwrap_or(a);
    let b = b.strip_prefix('?').unwrap_or(b);

    // Split on `|` and `&` to handle union and intersection types.
    // We compare component counts first, then each pair after
    // normalising namespace prefixes.
    let parts_a: Vec<&str> = a.split('|').flat_map(|part| part.split('&')).collect();
    let parts_b: Vec<&str> = b.split('|').flat_map(|part| part.split('&')).collect();

    if parts_a.len() != parts_b.len() {
        return false;
    }

    // Sort both sides so that `Foo|null` matches `null|Foo`.
    let mut sorted_a: Vec<&str> = parts_a.iter().map(|s| short_name(s)).collect();
    let mut sorted_b: Vec<&str> = parts_b.iter().map(|s| short_name(s)).collect();
    sorted_a.sort_unstable();
    sorted_b.sort_unstable();

    sorted_a == sorted_b
}

/// Return the short (unqualified) class name from a potentially
/// namespace-qualified type string.  Strips a leading `\` and returns
/// only the part after the last `\`.  Non-class types (scalars,
/// `array`, etc.) pass through unchanged.
fn short_name(ty: &str) -> &str {
    let t = ty.trim();
    let t = t.strip_prefix('\\').unwrap_or(t);
    t.rsplit('\\').next().unwrap_or(t)
}

#[cfg(test)]
mod tests;
