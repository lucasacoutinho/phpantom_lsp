/// Type resolution for completion subjects.
///
/// This module contains the core entry points for resolving a completion
/// subject (e.g. `$this`, `self`, `static`, `$var`, `$this->prop`,
/// `ClassName`) to a concrete `ClassInfo` so that the correct completion
/// items can be offered.
///
/// The resolution logic is split across several sibling modules:
///
/// - [`super::source_helpers`]: Source-text scanning helpers (closure return
///   types, first-class callable resolution, `new` expression parsing,
///   array access segment walking).
/// - [`super::variable_resolution`]: Variable type resolution via
///   assignment scanning and parameter type hints.
/// - [`super::type_narrowing`]: instanceof / assert / custom type guard
///   narrowing.
/// - [`super::closure_resolution`]: Closure and arrow-function parameter
///   resolution.
/// - [`crate::inheritance`]: Class inheritance merging (traits, mixins,
///   parent chain).
/// - [`super::conditional_resolution`]: PHPStan conditional return type
///   resolution at call sites.
use std::collections::HashMap;

use tower_lsp::lsp_types::Position;

use crate::Backend;
use crate::docblock;
use crate::docblock::types::{
    parse_generic_args, split_intersection_depth0, split_union_depth0, strip_generics,
};
use crate::inheritance::{apply_generic_args, apply_substitution};
use crate::types::*;
use crate::util::{ARRAY_ELEMENT_FUNCS, ARRAY_PRESERVING_FUNCS, short_name};

use crate::virtual_members::laravel::{ELOQUENT_BUILDER_FQN, build_scope_methods_for_builder};

use super::conditional_resolution::{
    VarClassStringResolver, resolve_conditional_with_text_args, resolve_conditional_without_args,
    split_call_subject, split_text_args,
};

/// Build a [`VarClassStringResolver`] closure from a [`ResolutionCtx`].
///
/// The returned closure resolves a variable name (e.g. `"$requestType"`)
/// to the class names it holds as class-string values by delegating to
/// [`Backend::resolve_class_string_targets`].
fn build_var_resolver<'a>(ctx: &'a ResolutionCtx<'a>) -> impl Fn(&str) -> Vec<String> + 'a {
    move |var_name: &str| -> Vec<String> {
        if let Some(cc) = ctx.current_class {
            Backend::resolve_class_string_targets(
                var_name,
                cc,
                ctx.all_classes,
                ctx.content,
                ctx.cursor_offset,
                ctx.class_loader,
            )
            .iter()
            .map(|c| c.name.clone())
            .collect()
        } else {
            vec![]
        }
    }
}

/// Type alias for the optional function-loader closure passed through
/// the resolution chain.  Reduces clippy `type_complexity` warnings.
pub(crate) type FunctionLoaderFn<'a> = Option<&'a dyn Fn(&str) -> Option<FunctionInfo>>;

/// Bundles the context needed by [`Backend::resolve_target_classes`] and
/// the functions it delegates to.
///
/// Introduced to replace the 8-parameter signature of
/// `resolve_target_classes` with a cleaner `(subject, access_kind, ctx)`
/// triple.  Also used directly by `resolve_call_return_types_expr` and
/// `resolve_arg_text_to_type` (formerly `CallResolutionCtx`).
pub(crate) struct ResolutionCtx<'a> {
    /// The class the cursor is inside, if any.
    pub current_class: Option<&'a ClassInfo>,
    /// All classes known in the current file.
    pub all_classes: &'a [ClassInfo],
    /// The full source text of the current file.
    pub content: &'a str,
    /// Byte offset of the cursor in `content`.
    pub cursor_offset: u32,
    /// Cross-file class resolution callback.
    pub class_loader: &'a dyn Fn(&str) -> Option<ClassInfo>,
    /// Cross-file function resolution callback (optional).
    pub function_loader: FunctionLoaderFn<'a>,
}

/// Bundles the common parameters threaded through variable-type resolution.
///
/// Introducing this struct avoids passing 7–10 individual arguments to
/// every helper in the resolution chain, which keeps clippy happy and
/// makes call-sites much easier to read.
pub(super) struct VarResolutionCtx<'a> {
    pub var_name: &'a str,
    pub current_class: &'a ClassInfo,
    pub all_classes: &'a [ClassInfo],
    pub content: &'a str,
    pub cursor_offset: u32,
    pub class_loader: &'a dyn Fn(&str) -> Option<ClassInfo>,
    pub function_loader: FunctionLoaderFn<'a>,
    /// The `@return` type annotation of the enclosing function/method,
    /// if known.  Used inside generator bodies to reverse-infer variable
    /// types from `Generator<TKey, TValue, TSend, TReturn>`.
    pub enclosing_return_type: Option<String>,
}

impl<'a> VarResolutionCtx<'a> {
    /// Create a [`ResolutionCtx`] from this variable resolution context.
    ///
    /// The non-optional `current_class` is wrapped in `Some(…)`.
    pub(crate) fn as_resolution_ctx(&self) -> ResolutionCtx<'a> {
        ResolutionCtx {
            current_class: Some(self.current_class),
            all_classes: self.all_classes,
            content: self.content,
            cursor_offset: self.cursor_offset,
            class_loader: self.class_loader,
            function_loader: self.function_loader,
        }
    }
}

/// Build a fully-qualified name from a short name and optional namespace.
///
/// Used by [`Backend::resolve_callable_target`] to construct human-readable
/// labels for signature help and named-argument completion.
fn format_callable_fqn(name: &str, namespace: &Option<String>) -> String {
    if let Some(ns) = namespace {
        format!("{}\\{}", ns, name)
    } else {
        name.to_string()
    }
}

/// Find a class in `all_classes` by name, preferring namespace-aware
/// matching when the name is fully qualified.
///
/// When `name` contains backslashes (e.g. `Illuminate\Database\Eloquent\Builder`),
/// the lookup checks each candidate's `file_namespace` field so that the
/// correct class is returned even when multiple classes share the same short
/// name but live in different namespace blocks within the same file (e.g.
/// `Demo\Builder` vs `Illuminate\Database\Eloquent\Builder`).
///
/// When `name` is a bare short name (no backslashes), the first class with
/// a matching `name` field is returned (preserving existing behavior).
pub(crate) fn find_class_by_name<'a>(
    all_classes: &'a [ClassInfo],
    name: &str,
) -> Option<&'a ClassInfo> {
    let clean = name.strip_prefix('\\').unwrap_or(name);
    let short = short_name(clean);

    if clean.contains('\\') {
        let expected_ns = clean.rsplit_once('\\').map(|(ns, _)| ns);
        all_classes
            .iter()
            .find(|c| c.name == short && c.file_namespace.as_deref() == expected_ns)
    } else {
        all_classes.iter().find(|c| c.name == short)
    }
}

impl Backend {
    /// Resolve a completion subject to all candidate class types.
    ///
    /// When a variable is assigned different types in conditional branches
    /// (e.g. an `if` block reassigns `$thing`), this returns every possible
    /// type so the caller can try each one when looking up members.
    ///
    /// Internally parses the subject string into a [`SubjectExpr`] and
    /// dispatches via `match` for exhaustive, type-safe routing.
    pub(crate) fn resolve_target_classes(
        subject: &str,
        access_kind: AccessKind,
        ctx: &ResolutionCtx<'_>,
    ) -> Vec<ClassInfo> {
        let expr = SubjectExpr::parse(subject);
        Self::resolve_target_classes_expr(&expr, subject, access_kind, ctx)
    }

    /// Core dispatch for [`resolve_target_classes`], operating on a
    /// pre-parsed [`SubjectExpr`].
    ///
    /// The `raw_subject` parameter carries the original string so that
    /// callees that haven't been migrated to `SubjectExpr` yet can still
    /// receive the text they expect.
    fn resolve_target_classes_expr(
        expr: &SubjectExpr,
        _raw_subject: &str,
        access_kind: AccessKind,
        ctx: &ResolutionCtx<'_>,
    ) -> Vec<ClassInfo> {
        let current_class = ctx.current_class;
        let all_classes = ctx.all_classes;
        let class_loader = ctx.class_loader;

        match expr {
            // ── Keywords that always mean "current class" ────────────
            SubjectExpr::This | SubjectExpr::SelfKw | SubjectExpr::StaticKw => {
                current_class.cloned().into_iter().collect()
            }

            // ── `parent::` — resolve to the current class's parent ──
            SubjectExpr::Parent => {
                if let Some(cc) = current_class
                    && let Some(ref parent_name) = cc.parent_class
                {
                    if let Some(cls) = find_class_by_name(all_classes, parent_name) {
                        return vec![cls.clone()];
                    }
                    return class_loader(parent_name).into_iter().collect();
                }
                vec![]
            }

            // ── Inline array literal with index access ──────────────
            SubjectExpr::InlineArray { elements, .. } => {
                let mut element_classes = Vec::new();
                for elem_text in elements {
                    let elem = elem_text.trim();
                    if elem.is_empty() {
                        continue;
                    }
                    let elem_expr = SubjectExpr::parse(elem);
                    let resolved =
                        Self::resolve_target_classes_expr(&elem_expr, elem, AccessKind::Arrow, ctx);
                    ClassInfo::extend_unique(&mut element_classes, resolved);
                }
                element_classes
            }

            // ── Enum case / static member access ────────────────────
            SubjectExpr::StaticAccess { class, .. } => {
                if let Some(cls) = find_class_by_name(all_classes, class) {
                    return vec![cls.clone()];
                }
                class_loader(class).into_iter().collect()
            }

            // ── Bare class name ─────────────────────────────────────
            SubjectExpr::ClassName(name) => {
                if let Some(cls) = find_class_by_name(all_classes, name) {
                    return vec![cls.clone()];
                }
                class_loader(name).into_iter().collect()
            }

            // ── `new ClassName` (without trailing call parens) ───────
            SubjectExpr::NewExpr { class_name } => {
                if let Some(cls) = find_class_by_name(all_classes, class_name) {
                    return vec![cls.clone()];
                }
                class_loader(class_name).into_iter().collect()
            }

            // ── Call expression ─────────────────────────────────────
            SubjectExpr::CallExpr { callee, args_text } => {
                Self::resolve_call_return_types_expr(callee, args_text, ctx)
            }

            // ── Property chain ──────────────────────────────────────
            SubjectExpr::PropertyChain { base, property } => {
                let base_text = base.to_subject_text();
                let base_classes =
                    Self::resolve_target_classes_expr(base, &base_text, access_kind, ctx);
                let mut results = Vec::new();
                for cls in &base_classes {
                    let resolved =
                        Self::resolve_property_types(property, cls, all_classes, class_loader);
                    ClassInfo::extend_unique(&mut results, resolved);
                }
                results
            }

            // ── Array access on variable ────────────────────────────
            SubjectExpr::ArrayAccess { base, segments } => {
                let base_var = base.to_subject_text();

                // Build candidate raw types from multiple strategies.
                // Each is tried as a complete pipeline (raw type →
                // segment walk → ClassInfo); the first that succeeds
                // through all segments wins.
                let docblock_type = docblock::find_iterable_raw_type_in_source(
                    ctx.content,
                    ctx.cursor_offset as usize,
                    &base_var,
                );
                let ast_type = Self::resolve_variable_assignment_raw_type(
                    &base_var,
                    ctx.content,
                    ctx.cursor_offset,
                    current_class,
                    all_classes,
                    class_loader,
                    ctx.function_loader,
                );

                let candidates = docblock_type.into_iter().chain(ast_type);

                if let Some(resolved) = Self::try_chained_array_access_with_candidates(
                    candidates,
                    segments,
                    current_class,
                    all_classes,
                    class_loader,
                ) {
                    return resolved;
                }
                // Fall through to variable resolution if the base is a bare variable
                if let SubjectExpr::Variable(_) = **base {
                    Self::resolve_variable_fallback(&base_var, access_kind, ctx)
                } else {
                    vec![]
                }
            }

            // ── Bare variable ───────────────────────────────────────
            SubjectExpr::Variable(var_name) => {
                Self::resolve_variable_fallback(var_name, access_kind, ctx)
            }

            // ── Callee-only variants (MethodCall, StaticMethodCall,
            //    FunctionCall) should not appear as top-level subjects;
            //    they are wrapped in CallExpr.  If they do appear
            //    (e.g. from a partial parse), treat as class name. ────
            SubjectExpr::MethodCall { .. }
            | SubjectExpr::StaticMethodCall { .. }
            | SubjectExpr::FunctionCall(_) => {
                let text = expr.to_subject_text();
                if let Some(cls) = find_class_by_name(all_classes, &text) {
                    return vec![cls.clone()];
                }
                class_loader(&text).into_iter().collect()
            }
        }
    }

    /// Shared variable-resolution logic extracted from the former
    /// bare-`$var` branch of `resolve_target_classes`.
    fn resolve_variable_fallback(
        var_name: &str,
        access_kind: AccessKind,
        ctx: &ResolutionCtx<'_>,
    ) -> Vec<ClassInfo> {
        let current_class = ctx.current_class;
        let all_classes = ctx.all_classes;
        let class_loader = ctx.class_loader;
        let function_loader = ctx.function_loader;

        let dummy_class;
        let effective_class = match current_class {
            Some(cc) => cc,
            None => {
                dummy_class = ClassInfo::default();
                &dummy_class
            }
        };

        // ── `$var::` where `$var` holds a class-string ──
        if access_kind == AccessKind::DoubleColon {
            let class_string_targets = Self::resolve_class_string_targets(
                var_name,
                effective_class,
                all_classes,
                ctx.content,
                ctx.cursor_offset,
                class_loader,
            );
            if !class_string_targets.is_empty() {
                return class_string_targets;
            }
        }

        Self::resolve_variable_types(
            var_name,
            effective_class,
            all_classes,
            ctx.content,
            ctx.cursor_offset,
            class_loader,
            function_loader,
        )
    }

    /// Resolve a call expression string to the callable's owner class and
    /// method (or standalone function), returning a
    /// [`ResolvedCallableTarget`] with the label, parameters, and return
    /// type.
    ///
    /// This is the single shared implementation used by both signature
    /// help (`resolve_callable`) and named-argument completion
    /// (`resolve_named_arg_params`).  Each caller projects the fields it
    /// needs from the result.
    ///
    /// The `expr` parameter uses the same format as the symbol map's
    /// `CallSite::call_expression`:
    ///   - `"functionName"` for standalone function calls
    ///   - `"$subject->method"` for instance/null-safe method calls
    ///   - `"ClassName::method"` for static method calls
    ///   - `"new ClassName"` for constructor calls
    pub(crate) fn resolve_callable_target(
        &self,
        expr: &str,
        content: &str,
        position: Position,
        file_ctx: &FileContext,
    ) -> Option<ResolvedCallableTarget> {
        let class_loader = self.class_loader(file_ctx);
        let function_loader_cl = self.function_loader(file_ctx);
        let cursor_offset = Self::position_to_offset(content, position);
        let current_class = Self::find_class_at_offset(&file_ctx.classes, cursor_offset);

        let parsed = SubjectExpr::parse(expr);

        match parsed {
            // ── Constructor: `new ClassName` or `new ClassName()` ────
            SubjectExpr::NewExpr { ref class_name } => {
                let ci = class_loader(class_name)?;
                let merged = Self::resolve_class_fully(&ci, &class_loader);
                let fqn = format_callable_fqn(&merged.name, &merged.file_namespace);
                let (parameters, return_type) =
                    if let Some(ctor) = merged.methods.iter().find(|m| m.name == "__construct") {
                        (ctor.parameters.clone(), ctor.return_type.clone())
                    } else {
                        (vec![], None)
                    };
                Some(ResolvedCallableTarget {
                    label_prefix: fqn,
                    parameters,
                    return_type,
                })
            }

            // ── Call wrapping a NewExpr: `new ClassName(args)` ───────
            SubjectExpr::CallExpr { ref callee, .. }
                if matches!(**callee, SubjectExpr::NewExpr { .. }) =>
            {
                if let SubjectExpr::NewExpr { ref class_name } = **callee {
                    let ci = class_loader(class_name)?;
                    let merged = Self::resolve_class_fully(&ci, &class_loader);
                    let fqn = format_callable_fqn(&merged.name, &merged.file_namespace);
                    let (parameters, return_type) = if let Some(ctor) =
                        merged.methods.iter().find(|m| m.name == "__construct")
                    {
                        (ctor.parameters.clone(), ctor.return_type.clone())
                    } else {
                        (vec![], None)
                    };
                    Some(ResolvedCallableTarget {
                        label_prefix: fqn,
                        parameters,
                        return_type,
                    })
                } else {
                    None
                }
            }

            // ── Call wrapping a MethodCall: `$subject->method(…)` ────
            SubjectExpr::CallExpr { ref callee, .. }
                if matches!(**callee, SubjectExpr::MethodCall { .. }) =>
            {
                if let SubjectExpr::MethodCall {
                    ref base,
                    ref method,
                } = **callee
                {
                    let subject_text = base.to_subject_text();
                    let owner_classes: Vec<ClassInfo> = if base.is_self_like() {
                        current_class.cloned().into_iter().collect()
                    } else {
                        let rctx = ResolutionCtx {
                            current_class,
                            all_classes: &file_ctx.classes,
                            content,
                            cursor_offset,
                            class_loader: &class_loader,
                            function_loader: Some(&function_loader_cl),
                        };
                        Self::resolve_target_classes(&subject_text, crate::AccessKind::Arrow, &rctx)
                    };

                    for owner in &owner_classes {
                        let merged = Self::resolve_class_fully(owner, &class_loader);
                        if let Some(m) = merged
                            .methods
                            .iter()
                            .find(|m| m.name.eq_ignore_ascii_case(method))
                        {
                            let owner_fqn =
                                format_callable_fqn(&merged.name, &merged.file_namespace);
                            return Some(ResolvedCallableTarget {
                                label_prefix: format!("{}::{}", owner_fqn, m.name),
                                parameters: m.parameters.clone(),
                                return_type: m.return_type.clone(),
                            });
                        }
                    }
                    None
                } else {
                    None
                }
            }

            // ── Call wrapping a StaticMethodCall: `Class::method(…)` ─
            SubjectExpr::CallExpr { ref callee, .. }
                if matches!(**callee, SubjectExpr::StaticMethodCall { .. }) =>
            {
                if let SubjectExpr::StaticMethodCall {
                    ref class,
                    ref method,
                } = **callee
                {
                    let owner_class = if class == "self" || class == "static" {
                        current_class.cloned()
                    } else if class == "parent" {
                        current_class
                            .and_then(|cc| cc.parent_class.as_ref())
                            .and_then(|p| class_loader(p))
                    } else {
                        class_loader(class).or_else(|| {
                            let rctx = ResolutionCtx {
                                current_class,
                                all_classes: &file_ctx.classes,
                                content,
                                cursor_offset,
                                class_loader: &class_loader,
                                function_loader: Some(&function_loader_cl),
                            };
                            Self::resolve_target_classes(
                                class,
                                crate::AccessKind::DoubleColon,
                                &rctx,
                            )
                            .into_iter()
                            .next()
                        })
                    };

                    let owner = owner_class?;
                    let merged = Self::resolve_class_fully(&owner, &class_loader);
                    let m = merged
                        .methods
                        .iter()
                        .find(|m| m.name.eq_ignore_ascii_case(method))?;
                    let owner_fqn = format_callable_fqn(&merged.name, &merged.file_namespace);
                    Some(ResolvedCallableTarget {
                        label_prefix: format!("{}::{}", owner_fqn, m.name),
                        parameters: m.parameters.clone(),
                        return_type: m.return_type.clone(),
                    })
                } else {
                    None
                }
            }

            // ── Call wrapping a FunctionCall: `functionName(…)` ──────
            SubjectExpr::CallExpr { ref callee, .. }
                if matches!(**callee, SubjectExpr::FunctionCall(_)) =>
            {
                if let SubjectExpr::FunctionCall(ref name) = **callee {
                    let func =
                        self.resolve_function_name(name, &file_ctx.use_map, &file_ctx.namespace)?;
                    let fqn = if let Some(ref ns) = func.namespace {
                        format!("{}\\{}", ns, func.name)
                    } else {
                        func.name.clone()
                    };
                    Some(ResolvedCallableTarget {
                        label_prefix: fqn,
                        parameters: func.parameters.clone(),
                        return_type: func.return_type.clone(),
                    })
                } else {
                    None
                }
            }

            // ── Call wrapping a Variable: `$fn(…)` ──────────────────
            SubjectExpr::CallExpr { ref callee, .. }
                if matches!(**callee, SubjectExpr::Variable(_)) =>
            {
                if let SubjectExpr::Variable(ref var_name) = **callee
                    && let Some(callable_target) = Self::extract_callable_target_from_variable(
                        var_name,
                        content,
                        cursor_offset,
                    )
                {
                    return self.resolve_callable_target(
                        &callable_target,
                        content,
                        position,
                        file_ctx,
                    );
                }
                None
            }

            // ── Bare function name (no parens — text fallback) ──────
            SubjectExpr::FunctionCall(ref name) => {
                let func =
                    self.resolve_function_name(name, &file_ctx.use_map, &file_ctx.namespace)?;
                let fqn = if let Some(ref ns) = func.namespace {
                    format!("{}\\{}", ns, func.name)
                } else {
                    func.name.clone()
                };
                Some(ResolvedCallableTarget {
                    label_prefix: fqn,
                    parameters: func.parameters.clone(),
                    return_type: func.return_type.clone(),
                })
            }

            // ── Instance method callee without parens (text fallback):
            //    `$subject->method` ──────────────────────────────────
            SubjectExpr::MethodCall {
                ref base,
                ref method,
            } => {
                let subject_text = base.to_subject_text();
                let owner_classes: Vec<ClassInfo> = if base.is_self_like() {
                    current_class.cloned().into_iter().collect()
                } else {
                    let rctx = ResolutionCtx {
                        current_class,
                        all_classes: &file_ctx.classes,
                        content,
                        cursor_offset,
                        class_loader: &class_loader,
                        function_loader: Some(&function_loader_cl),
                    };
                    Self::resolve_target_classes(&subject_text, crate::AccessKind::Arrow, &rctx)
                };

                for owner in &owner_classes {
                    let merged = Self::resolve_class_fully(owner, &class_loader);
                    if let Some(m) = merged
                        .methods
                        .iter()
                        .find(|m| m.name.eq_ignore_ascii_case(method))
                    {
                        let owner_fqn = format_callable_fqn(&merged.name, &merged.file_namespace);
                        return Some(ResolvedCallableTarget {
                            label_prefix: format!("{}::{}", owner_fqn, m.name),
                            parameters: m.parameters.clone(),
                            return_type: m.return_type.clone(),
                        });
                    }
                }
                None
            }

            // ── Static method callee without parens (text fallback):
            //    `ClassName::method` ─────────────────────────────────
            SubjectExpr::StaticMethodCall {
                ref class,
                ref method,
            } => {
                let owner_class = if class == "self" || class == "static" {
                    current_class.cloned()
                } else if class == "parent" {
                    current_class
                        .and_then(|cc| cc.parent_class.as_ref())
                        .and_then(|p| class_loader(p))
                } else {
                    class_loader(class).or_else(|| {
                        let rctx = ResolutionCtx {
                            current_class,
                            all_classes: &file_ctx.classes,
                            content,
                            cursor_offset,
                            class_loader: &class_loader,
                            function_loader: Some(&function_loader_cl),
                        };
                        Self::resolve_target_classes(class, crate::AccessKind::DoubleColon, &rctx)
                            .into_iter()
                            .next()
                    })
                };

                let owner = owner_class?;
                let merged = Self::resolve_class_fully(&owner, &class_loader);
                let m = merged
                    .methods
                    .iter()
                    .find(|m| m.name.eq_ignore_ascii_case(method))?;
                let owner_fqn = format_callable_fqn(&merged.name, &merged.file_namespace);
                Some(ResolvedCallableTarget {
                    label_prefix: format!("{}::{}", owner_fqn, m.name),
                    parameters: m.parameters.clone(),
                    return_type: m.return_type.clone(),
                })
            }

            // ── Constructor without call parens: `new ClassName` ─────
            // (handled above, but listed for exhaustiveness of text
            // fallback paths)

            // ── Bare variable used as a callable target: `$fn` ──────
            // Signature help and named-arg contexts may pass `"$fn"`
            // (without trailing `()`) when the call site is `$fn()`.
            // Check for a first-class callable assignment and recurse.
            SubjectExpr::Variable(ref var_name) => {
                if let Some(callable_target) =
                    Self::extract_callable_target_from_variable(var_name, content, cursor_offset)
                {
                    return self.resolve_callable_target(
                        &callable_target,
                        content,
                        position,
                        file_ctx,
                    );
                }
                None
            }

            // ── Bare class name used as a function name ─────────────
            // Named-arg and signature-help contexts pass bare function
            // names like `"foo"` which `SubjectExpr::parse` produces
            // as `ClassName` (since it can't distinguish class names
            // from function names without context).
            SubjectExpr::ClassName(ref name) => {
                let func =
                    self.resolve_function_name(name, &file_ctx.use_map, &file_ctx.namespace)?;
                let fqn = if let Some(ref ns) = func.namespace {
                    format!("{}\\{}", ns, func.name)
                } else {
                    func.name.clone()
                };
                Some(ResolvedCallableTarget {
                    label_prefix: fqn,
                    parameters: func.parameters.clone(),
                    return_type: func.return_type.clone(),
                })
            }

            // ── PropertyChain used as a callable target ──────────────
            // Named-arg and signature-help contexts pass expressions
            // like `"$this->method"` (without trailing `()`), which
            // `SubjectExpr::parse` produces as `PropertyChain`.  Treat
            // the trailing property as a method name.
            SubjectExpr::PropertyChain {
                ref base,
                ref property,
            } => {
                let subject_text = base.to_subject_text();
                let owner_classes: Vec<ClassInfo> = if base.is_self_like() {
                    current_class.cloned().into_iter().collect()
                } else {
                    let rctx = ResolutionCtx {
                        current_class,
                        all_classes: &file_ctx.classes,
                        content,
                        cursor_offset,
                        class_loader: &class_loader,
                        function_loader: Some(&function_loader_cl),
                    };
                    Self::resolve_target_classes(&subject_text, crate::AccessKind::Arrow, &rctx)
                };

                for owner in &owner_classes {
                    let merged = Self::resolve_class_fully(owner, &class_loader);
                    if let Some(m) = merged
                        .methods
                        .iter()
                        .find(|m| m.name.eq_ignore_ascii_case(property))
                    {
                        let owner_fqn = format_callable_fqn(&merged.name, &merged.file_namespace);
                        return Some(ResolvedCallableTarget {
                            label_prefix: format!("{}::{}", owner_fqn, m.name),
                            parameters: m.parameters.clone(),
                            return_type: m.return_type.clone(),
                        });
                    }
                }
                None
            }

            // ── StaticAccess used as a callable target ──────────────
            // Same situation: `"ClassName::method"` without `()` parses
            // as `StaticAccess` rather than `StaticMethodCall`.
            SubjectExpr::StaticAccess {
                ref class,
                ref member,
            } => {
                let owner_class = if class == "self" || class == "static" {
                    current_class.cloned()
                } else if class == "parent" {
                    current_class
                        .and_then(|cc| cc.parent_class.as_ref())
                        .and_then(|p| class_loader(p))
                } else {
                    class_loader(class).or_else(|| {
                        let rctx = ResolutionCtx {
                            current_class,
                            all_classes: &file_ctx.classes,
                            content,
                            cursor_offset,
                            class_loader: &class_loader,
                            function_loader: Some(&function_loader_cl),
                        };
                        Self::resolve_target_classes(class, crate::AccessKind::DoubleColon, &rctx)
                            .into_iter()
                            .next()
                    })
                };

                let owner = owner_class?;
                let merged = Self::resolve_class_fully(&owner, &class_loader);
                let m = merged
                    .methods
                    .iter()
                    .find(|m| m.name.eq_ignore_ascii_case(member))?;
                let owner_fqn = format_callable_fqn(&merged.name, &merged.file_namespace);
                Some(ResolvedCallableTarget {
                    label_prefix: format!("{}::{}", owner_fqn, m.name),
                    parameters: m.parameters.clone(),
                    return_type: m.return_type.clone(),
                })
            }

            // ── Anything else doesn't resolve to a callable ─────────
            _ => None,
        }
    }

    /// Resolve the return type of a call expression given a structured
    /// [`SubjectExpr`] callee and argument text, returning zero or more
    /// `ClassInfo` values.
    ///
    /// This is the primary entry point for call return type resolution.
    /// The callee should be one of the "callee" variants produced by
    /// `parse_callee`: [`SubjectExpr::MethodCall`],
    /// [`SubjectExpr::StaticMethodCall`], [`SubjectExpr::FunctionCall`],
    /// [`SubjectExpr::Variable`], or [`SubjectExpr::NewExpr`].
    /// Any other variant falls through to `resolve_target_classes_expr`.
    pub(super) fn resolve_call_return_types_expr(
        callee: &SubjectExpr,
        text_args: &str,
        ctx: &ResolutionCtx<'_>,
    ) -> Vec<ClassInfo> {
        let current_class = ctx.current_class;
        let all_classes = ctx.all_classes;
        let class_loader = ctx.class_loader;
        let function_loader = ctx.function_loader;

        match callee {
            // ── Instance method call: base->method(…) ───────────────
            SubjectExpr::MethodCall { base, method } => {
                let method_name = method.as_str();

                // Resolve the base expression to class(es).
                let lhs_classes: Vec<ClassInfo> = Self::resolve_target_classes_expr(
                    base,
                    &base.to_subject_text(),
                    AccessKind::Arrow,
                    ctx,
                );

                let mut results = Vec::new();
                for owner in &lhs_classes {
                    let template_subs = if !text_args.is_empty() {
                        Self::build_method_template_subs(
                            owner,
                            method_name,
                            text_args,
                            ctx,
                            class_loader,
                        )
                    } else {
                        HashMap::new()
                    };
                    let var_resolver = build_var_resolver(ctx);
                    results.extend(Self::resolve_method_return_types_with_args(
                        owner,
                        method_name,
                        text_args,
                        all_classes,
                        class_loader,
                        &template_subs,
                        Some(&var_resolver),
                    ));
                }
                results
            }

            // ── Static method call: Class::method(…) ────────────────
            SubjectExpr::StaticMethodCall { class, method } => {
                let method_name = method.as_str();

                let owner_class = if class == "self" || class == "static" {
                    current_class.cloned()
                } else if class == "parent" {
                    current_class
                        .and_then(|cc| cc.parent_class.as_ref())
                        .and_then(|p| class_loader(p))
                } else if class.starts_with('$') {
                    // Variable holding a class-string (e.g. `$cls::make()`).
                    Self::resolve_target_classes(class, AccessKind::DoubleColon, ctx)
                        .into_iter()
                        .next()
                } else {
                    find_class_by_name(all_classes, class)
                        .cloned()
                        .or_else(|| class_loader(class))
                };

                if let Some(ref owner) = owner_class {
                    let template_subs = if !text_args.is_empty() {
                        Self::build_method_template_subs(
                            owner,
                            method_name,
                            text_args,
                            ctx,
                            class_loader,
                        )
                    } else {
                        HashMap::new()
                    };
                    let var_resolver = build_var_resolver(ctx);
                    return Self::resolve_method_return_types_with_args(
                        owner,
                        method_name,
                        text_args,
                        all_classes,
                        class_loader,
                        &template_subs,
                        Some(&var_resolver),
                    );
                }
                vec![]
            }

            // ── Standalone function call: app(…) / myHelper(…) ──────
            SubjectExpr::FunctionCall(func_name) => {
                let func_name = func_name.as_str();

                // Check for array element/preserving functions first.
                let is_array_element_func = ARRAY_ELEMENT_FUNCS
                    .iter()
                    .any(|f| f.eq_ignore_ascii_case(func_name));
                let is_array_preserving_func = ARRAY_PRESERVING_FUNCS
                    .iter()
                    .any(|f| f.eq_ignore_ascii_case(func_name));

                if (is_array_element_func || is_array_preserving_func)
                    && !text_args.is_empty()
                    && let Some(first_arg) = Self::extract_first_arg_text(text_args)
                {
                    let arg_raw_type = Self::resolve_inline_arg_raw_type(&first_arg, ctx);

                    if let Some(ref raw) = arg_raw_type
                        && let Some(element_type) = docblock::types::extract_generic_value_type(raw)
                    {
                        let owner_name = current_class.map(|c| c.name.as_str()).unwrap_or("");
                        let classes = Self::type_hint_to_classes(
                            &element_type,
                            owner_name,
                            all_classes,
                            class_loader,
                        );
                        if !classes.is_empty() {
                            return classes;
                        }
                    }
                }

                // Regular function lookup.
                if let Some(fl) = function_loader
                    && let Some(func_info) = fl(func_name)
                {
                    if let Some(ref cond) = func_info.conditional_return {
                        let var_resolver = build_var_resolver(ctx);
                        let resolved_type = if !text_args.is_empty() {
                            resolve_conditional_with_text_args(
                                cond,
                                &func_info.parameters,
                                text_args,
                                Some(&var_resolver),
                            )
                        } else {
                            resolve_conditional_without_args(cond, &func_info.parameters)
                        };
                        if let Some(ref ty) = resolved_type {
                            let classes =
                                Self::type_hint_to_classes(ty, "", all_classes, class_loader);
                            if !classes.is_empty() {
                                return classes;
                            }
                        }
                    }
                    if let Some(ref ret) = func_info.return_type {
                        return Self::type_hint_to_classes(ret, "", all_classes, class_loader);
                    }
                }

                vec![]
            }

            // ── Variable invocation: $fn(…) ─────────────────────────
            SubjectExpr::Variable(var_name) => {
                let content = ctx.content;
                let cursor_offset = ctx.cursor_offset;

                // 1. Try docblock annotation: `@var Closure(): User $fn`
                if let Some(raw_type) = crate::docblock::find_iterable_raw_type_in_source(
                    content,
                    cursor_offset as usize,
                    var_name,
                ) && let Some(ret) = crate::docblock::extract_callable_return_type(&raw_type)
                {
                    let classes = Self::type_hint_to_classes(&ret, "", all_classes, class_loader);
                    if !classes.is_empty() {
                        return classes;
                    }
                }

                // 2. Scan for closure/arrow-function literal assignment.
                if let Some(ret) = Self::extract_closure_return_type_from_assignment(
                    var_name,
                    content,
                    cursor_offset,
                ) {
                    let classes = Self::type_hint_to_classes(&ret, "", all_classes, class_loader);
                    if !classes.is_empty() {
                        return classes;
                    }
                }

                // 3. Scan for first-class callable assignment.
                if let Some(ret) = Self::extract_first_class_callable_return_type(
                    var_name,
                    content,
                    cursor_offset,
                    current_class,
                    all_classes,
                    class_loader,
                    function_loader,
                ) {
                    let classes = Self::type_hint_to_classes(&ret, "", all_classes, class_loader);
                    if !classes.is_empty() {
                        return classes;
                    }
                }

                vec![]
            }

            // ── Constructor call: new ClassName(…) ──────────────────
            // A `NewExpr` callee means the call is `new Foo(…)` — the
            // return type is always the class itself.
            SubjectExpr::NewExpr { class_name } => find_class_by_name(all_classes, class_name)
                .cloned()
                .or_else(|| class_loader(class_name))
                .into_iter()
                .collect(),

            // ── Any other callee form (e.g. a nested CallExpr used as
            //    a callee, or a ClassName that SubjectExpr::parse
            //    couldn't distinguish from a function name) ───────────
            _ => {
                // Resolve via the target-classes path which handles
                // all remaining SubjectExpr variants.  This avoids
                // round-tripping through text and back.
                Self::resolve_target_classes_expr(
                    callee,
                    &callee.to_subject_text(),
                    AccessKind::Arrow,
                    ctx,
                )
            }
        }
    }

    /// Resolve a method call's return type, taking into account PHPStan
    /// conditional return types when `text_args` is provided, and
    /// method-level `@template` substitutions when `template_subs` is
    /// non-empty.
    ///
    /// This is the workhorse behind both `resolve_method_return_types`
    /// (which passes `""`) and the inline call-chain path (which passes
    /// the raw argument text from the source, e.g. `"CurrentCart::class"`).
    pub(super) fn resolve_method_return_types_with_args(
        class_info: &ClassInfo,
        method_name: &str,
        text_args: &str,
        all_classes: &[ClassInfo],
        class_loader: &dyn Fn(&str) -> Option<ClassInfo>,
        template_subs: &HashMap<String, String>,
        var_resolver: VarClassStringResolver<'_>,
    ) -> Vec<ClassInfo> {
        // Helper: try to resolve a method's conditional return type, falling
        // back to template-substituted return type, then plain return type.
        let resolve_method = |method: &MethodInfo| -> Vec<ClassInfo> {
            // Try conditional return type first (PHPStan syntax)
            if let Some(ref cond) = method.conditional_return {
                let resolved_type = if !text_args.is_empty() {
                    resolve_conditional_with_text_args(
                        cond,
                        &method.parameters,
                        text_args,
                        var_resolver,
                    )
                } else {
                    resolve_conditional_without_args(cond, &method.parameters)
                };
                if let Some(ref ty) = resolved_type {
                    // Apply method-level template substitutions to the
                    // resolved conditional type (e.g. `TModel` → concrete
                    // class when TModel is a method-level @template param).
                    let effective_ty = if !template_subs.is_empty() {
                        apply_substitution(ty, template_subs)
                    } else {
                        ty.clone()
                    };
                    let classes = Self::type_hint_to_classes(
                        &effective_ty,
                        &class_info.name,
                        all_classes,
                        class_loader,
                    );
                    if !classes.is_empty() {
                        return classes;
                    }
                }
            }

            // Try method-level @template substitution on the return type.
            // This handles the general case where the return type references
            // a template param (e.g. `@return Collection<T>`) and we have
            // resolved bindings from the call-site arguments.
            if !template_subs.is_empty()
                && let Some(ref ret) = method.return_type
            {
                let substituted = apply_substitution(ret, template_subs);
                if substituted != *ret {
                    let classes = Self::type_hint_to_classes(
                        &substituted,
                        &class_info.name,
                        all_classes,
                        class_loader,
                    );
                    if !classes.is_empty() {
                        return classes;
                    }
                }
            }

            // Fall back to plain return type
            if let Some(ref ret) = method.return_type {
                // When the return type is `static`, `self`, or `$this`,
                // return the owning class directly.  This avoids a lookup
                // by short name (e.g. "Builder") which fails when the
                // class was loaded cross-file and the short name is not
                // in the current file's use-map or local classes.
                // Returning class_info preserves any generic substitutions
                // already applied (e.g. Builder<User> stays Builder<User>).
                let trimmed = ret.trim();
                if trimmed == "static" || trimmed == "self" || trimmed == "$this" {
                    return vec![class_info.clone()];
                }
                return Self::type_hint_to_classes(
                    ret,
                    &class_info.name,
                    all_classes,
                    class_loader,
                );
            }
            vec![]
        };

        // First check the class itself
        if let Some(method) = class_info.methods.iter().find(|m| m.name == method_name) {
            return resolve_method(method);
        }

        // Walk up the inheritance chain
        let merged = Self::resolve_class_fully(class_info, class_loader);
        if let Some(method) = merged.methods.iter().find(|m| m.name == method_name) {
            return resolve_method(method);
        }

        vec![]
    }

    /// Build a template substitution map for a method-level `@template` call.
    ///
    /// Finds the method on the class (or inherited), checks for template
    /// params and bindings, resolves argument types from `text_args` using
    /// the call resolution context, and returns a `HashMap` mapping template
    /// parameter names to their resolved concrete types.
    ///
    /// Returns an empty map if the method has no template params, no
    /// bindings, or if argument types cannot be resolved.
    pub(super) fn build_method_template_subs(
        class_info: &ClassInfo,
        method_name: &str,
        text_args: &str,
        ctx: &ResolutionCtx<'_>,
        class_loader: &dyn Fn(&str) -> Option<ClassInfo>,
    ) -> HashMap<String, String> {
        // Find the method — first on the class directly, then via inheritance.
        let method = class_info
            .methods
            .iter()
            .find(|m| m.name == method_name)
            .cloned()
            .or_else(|| {
                let merged = Self::resolve_class_fully(class_info, class_loader);
                merged.methods.into_iter().find(|m| m.name == method_name)
            });

        let method = match method {
            Some(m) if !m.template_params.is_empty() && !m.template_bindings.is_empty() => m,
            _ => return HashMap::new(),
        };

        let args = split_text_args(text_args);
        let mut subs = HashMap::new();

        for (tpl_name, param_name) in &method.template_bindings {
            // Find the parameter index for this binding.
            let param_idx = match method.parameters.iter().position(|p| p.name == *param_name) {
                Some(idx) => idx,
                None => continue,
            };

            // Get the corresponding argument text.
            let arg_text = match args.get(param_idx) {
                Some(text) => text.trim(),
                None => continue,
            };

            // Try to resolve the argument text to a type name.
            if let Some(type_name) = Self::resolve_arg_text_to_type(arg_text, ctx) {
                subs.insert(tpl_name.clone(), type_name);
            }
        }

        subs
    }

    /// Resolve an argument text string to a type name.
    ///
    /// Handles common patterns:
    /// - `ClassName::class` → `ClassName`
    /// - `new ClassName(…)` → `ClassName`
    /// - `$this` / `self` / `static` → current class name
    /// - `$this->prop` → property type
    /// - `$var` → variable type via assignment scanning
    fn resolve_arg_text_to_type(arg_text: &str, ctx: &ResolutionCtx<'_>) -> Option<String> {
        let trimmed = arg_text.trim();

        // ClassName::class → ClassName
        if let Some(name) = trimmed.strip_suffix("::class")
            && !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == '\\')
        {
            return Some(name.strip_prefix('\\').unwrap_or(name).to_string());
        }

        // new ClassName(…) → ClassName
        if let Some(class_name) = Self::extract_new_expression_class(trimmed) {
            return Some(class_name);
        }

        // $this / self / static → current class
        if trimmed == "$this" || trimmed == "self" || trimmed == "static" {
            return ctx.current_class.map(|c| c.name.clone());
        }

        // $this->prop → property type
        if let Some(prop) = trimmed
            .strip_prefix("$this->")
            .or_else(|| trimmed.strip_prefix("$this?->"))
            && prop.chars().all(|c| c.is_alphanumeric() || c == '_')
            && let Some(owner) = ctx.current_class
        {
            let types =
                Self::resolve_property_types(prop, owner, ctx.all_classes, ctx.class_loader);
            if let Some(first) = types.first() {
                return Some(first.name.clone());
            }
        }

        // $var → resolve variable type
        if trimmed.starts_with('$') {
            let classes =
                Self::resolve_target_classes(trimmed, crate::types::AccessKind::Arrow, ctx);
            if let Some(first) = classes.first() {
                return Some(first.name.clone());
            }
        }

        None
    }

    /// Look up a property's type hint and resolve all candidate classes.
    ///
    /// When the type hint is a union (e.g. `A|B`), every resolvable part
    /// is returned.
    pub(crate) fn resolve_property_types(
        prop_name: &str,
        class_info: &ClassInfo,
        all_classes: &[ClassInfo],
        class_loader: &dyn Fn(&str) -> Option<ClassInfo>,
    ) -> Vec<ClassInfo> {
        // Resolve inheritance so that inherited (and generic-substituted)
        // properties are visible.  For example, if `ConfigWrapper extends
        // Wrapper<Config>` and `Wrapper` has `/** @var T */ public $value`,
        // the merged class will have `$value` with type `Config`.
        let type_hint = match Self::resolve_property_type_hint(class_info, prop_name, class_loader)
        {
            Some(h) => h,
            None => return vec![],
        };
        Self::type_hint_to_classes(&type_hint, &class_info.name, all_classes, class_loader)
    }

    /// Map a type-hint string to all matching `ClassInfo` values.
    ///
    /// Handles:
    ///   - Nullable types: `?Foo` → strips `?`, resolves `Foo`
    ///   - Union types: `A|B|C` → resolves each part independently
    ///     (respects `<…>` nesting so `Collection<int|string>` is not split)
    ///   - Intersection types: `A&B` → resolves each part independently
    ///   - Generic types: `Collection<int, User>` → resolves `Collection`,
    ///     then applies generic substitution (`TKey→int`, `TValue→User`)
    ///   - `self` / `static` / `$this` → owning class
    ///   - Scalar/built-in types (`int`, `string`, `bool`, `float`, `array`,
    ///     `void`, `null`, `mixed`, `never`, `object`, `callable`, `iterable`,
    ///     `false`, `true`) → skipped (not class types)
    ///
    /// Each resolvable class-like part is returned as a separate entry.
    pub(crate) fn type_hint_to_classes(
        type_hint: &str,
        owning_class_name: &str,
        all_classes: &[ClassInfo],
        class_loader: &dyn Fn(&str) -> Option<ClassInfo>,
    ) -> Vec<ClassInfo> {
        Self::type_hint_to_classes_depth(type_hint, owning_class_name, all_classes, class_loader, 0)
    }

    /// Inner implementation of [`type_hint_to_classes`] with a recursion
    /// depth guard to prevent infinite loops from circular type aliases.
    fn type_hint_to_classes_depth(
        type_hint: &str,
        owning_class_name: &str,
        all_classes: &[ClassInfo],
        class_loader: &dyn Fn(&str) -> Option<ClassInfo>,
        depth: u8,
    ) -> Vec<ClassInfo> {
        if depth > MAX_ALIAS_DEPTH {
            return vec![];
        }

        let hint = type_hint.strip_prefix('?').unwrap_or(type_hint);

        // Strip surrounding parentheses that appear in DNF types like `(A&B)|C`.
        let hint = hint
            .strip_prefix('(')
            .and_then(|h| h.strip_suffix(')'))
            .unwrap_or(hint);

        // ── Type alias resolution ──────────────────────────────────────
        // Check if `hint` is a type alias defined on the owning class
        // (via `@phpstan-type` / `@psalm-type` / `@phpstan-import-type`).
        // If so, expand the alias and resolve the underlying definition.
        //
        // This runs before union/intersection splitting because the alias
        // itself may expand to a union or intersection type.
        if let Some(alias_def) =
            Self::resolve_type_alias(hint, owning_class_name, all_classes, class_loader)
        {
            return Self::type_hint_to_classes_depth(
                &alias_def,
                owning_class_name,
                all_classes,
                class_loader,
                depth + 1,
            );
        }

        // ── Union type: split on `|` at depth 0, respecting `<…>` nesting ──
        let union_parts = split_union_depth0(hint);
        if union_parts.len() > 1 {
            let mut results = Vec::new();
            for part in union_parts {
                let part = part.trim();
                if part.is_empty() {
                    continue;
                }
                // Recursively resolve each part (handles self/static, scalars,
                // intersection components, etc.)
                let resolved = Self::type_hint_to_classes_depth(
                    part,
                    owning_class_name,
                    all_classes,
                    class_loader,
                    depth,
                );
                ClassInfo::extend_unique(&mut results, resolved);
            }
            return results;
        }

        // ── Intersection type: split on `&` at depth 0 and resolve each part ──
        // `User&JsonSerializable` means the value satisfies *all* listed
        // types, so completions should include members from every part.
        // Uses depth-aware splitting so that `&` inside `{…}` or `<…>`
        // (e.g. `object{foo: A&B}`) is not treated as a top-level split.
        let intersection_parts = split_intersection_depth0(hint);
        if intersection_parts.len() > 1 {
            let mut results = Vec::new();
            for part in intersection_parts {
                let part = part.trim();
                if part.is_empty() {
                    continue;
                }
                let resolved = Self::type_hint_to_classes_depth(
                    part,
                    owning_class_name,
                    all_classes,
                    class_loader,
                    depth,
                );
                ClassInfo::extend_unique(&mut results, resolved);
            }
            return results;
        }

        // ── Object shape: `object{foo: int, bar: string}` ──────────────
        // Synthesise a ClassInfo with public properties from the shape
        // entries so that `$var->foo` resolves through normal property
        // resolution.  Object shape properties are read-only.
        if docblock::types::is_object_shape(hint)
            && let Some(entries) = docblock::parse_object_shape(hint)
        {
            let properties = entries
                .into_iter()
                .map(|e| PropertyInfo {
                    name: e.key,
                    name_offset: 0,
                    type_hint: Some(e.value_type),
                    is_static: false,
                    visibility: Visibility::Public,
                    is_deprecated: false,
                })
                .collect();

            let synthetic = ClassInfo {
                name: "__object_shape".to_string(),
                properties,
                ..ClassInfo::default()
            };
            return vec![synthetic];
        }

        // self / static / $this always refer to the owning class.
        // In docblocks `@return $this` means "the instance the method is
        // called on" — identical to `static` for inheritance, but when the
        // method comes from a `@mixin` the return type is rewritten to the
        // mixin class name during merge (see `PHPDocProvider` in
        // `virtual_members/phpdoc.rs`).
        if hint == "self" || hint == "static" || hint == "$this" {
            return all_classes
                .iter()
                .find(|c| c.name == owning_class_name)
                .cloned()
                .or_else(|| class_loader(owning_class_name))
                .into_iter()
                .collect();
        }

        // ── Parse generic arguments (if any) ──
        // `Collection<int, User>` → base_hint = `Collection`, generic_args = ["int", "User"]
        // `Foo`                   → base_hint = `Foo`,        generic_args = []
        let (base_hint, raw_generic_args) = parse_generic_args(hint);

        // ── Resolve static/self/$this inside generic arguments ────────
        // When a method returns e.g. `Builder<static>`, the generic arg
        // `static` must be resolved to the owning class name so that
        // `Brand::with('english')->` resolves to `Builder<Brand>` and
        // scope injection (and other generic substitution) works correctly.
        let resolved_generic_args: Vec<String> = raw_generic_args
            .iter()
            .map(|arg| {
                let trimmed = arg.trim();
                if trimmed == "static" || trimmed == "self" || trimmed == "$this" {
                    owning_class_name.to_string()
                } else {
                    trimmed.to_string()
                }
            })
            .collect();
        let generic_args: Vec<&str> = resolved_generic_args.iter().map(|s| s.as_str()).collect();

        // For class lookup, strip any remaining generics from the base
        // (should already be clean, but defensive) and use the short name.
        let base_clean = strip_generics(base_hint.strip_prefix('\\').unwrap_or(base_hint));
        let short = short_name(&base_clean);

        // Try local (current-file) lookup by last segment.
        //
        // When the type hint is namespace-qualified (e.g.
        // `Illuminate\Database\Eloquent\Builder`), match against each
        // class's `file_namespace` so that we pick the right one when
        // multiple classes share the same short name but live in
        // different namespace blocks (e.g. `Demo\Builder` vs
        // `Illuminate\Database\Eloquent\Builder` in example.php).
        let found = find_class_by_name(all_classes, &base_clean)
            .cloned()
            .or_else(|| class_loader(base_hint));

        match found {
            Some(cls) => {
                // ── Custom Eloquent collection swapping ────────────────
                // When the resolved class is the standard Eloquent
                // Collection and one of the generic type args is a model
                // with a `custom_collection` declared (via
                // `#[CollectedBy]` or `@use HasCollection<X>`), swap to
                // the custom collection class so that its own methods
                // (e.g. `topRated()`) appear in completions.
                //
                // This handles the common chain pattern:
                //   Model::where(...)->get()
                // where Builder's `get()` returns
                //   `\Illuminate\Database\Eloquent\Collection<int, TModel>`
                // and TModel has been substituted to the concrete model.
                //
                // We compare against `base_clean` (the FQN extracted from
                // the type hint) rather than `cls.file_namespace` because
                // `file_namespace` is not always populated when classes are
                // loaded via PSR-4 / classmap.
                let is_eloquent_collection = {
                    let bc = base_clean.strip_prefix('\\').unwrap_or(&base_clean);
                    bc == crate::types::ELOQUENT_COLLECTION_FQN
                };
                let cls = if is_eloquent_collection && !generic_args.is_empty() {
                    // The last generic arg is typically the model type.
                    let model_arg = generic_args.last().unwrap();
                    let model_clean = model_arg.strip_prefix('\\').unwrap_or(model_arg);
                    let model_class = find_class_by_name(all_classes, model_clean)
                        .cloned()
                        .or_else(|| class_loader(model_clean));
                    if let Some(ref mc) = model_class
                        && let Some(ref coll_name) = mc.custom_collection
                    {
                        let coll_clean = coll_name.strip_prefix('\\').unwrap_or(coll_name);
                        find_class_by_name(all_classes, coll_clean)
                            .cloned()
                            .or_else(|| class_loader(coll_clean))
                            .unwrap_or(cls)
                    } else {
                        cls
                    }
                } else {
                    cls
                };

                // Apply generic substitution if the type hint carried
                // generic arguments and the class has template parameters.
                // Resolve the class fully first (including trait methods,
                // parent methods, and virtual members) so that methods
                // inherited from traits also receive the substitution.
                // Without this, a method like `first()` inherited from
                // `BuildsQueries` via `@use BuildsQueries<TModel>` would
                // keep its raw `TModel` return type instead of being
                // substituted to the concrete model class.
                if !generic_args.is_empty() && !cls.template_params.is_empty() {
                    let resolved = Self::resolve_class_fully(&cls, class_loader);
                    let mut result = apply_generic_args(&resolved, &generic_args);

                    // ── Eloquent Builder scope injection ───────────────
                    // When the resolved class is the Eloquent Builder
                    // and the first generic arg is a concrete model
                    // name, inject the model's scope methods as instance
                    // methods on the Builder so that
                    // `Brand::where(...)->isActive()` and
                    // `$query->active()` both resolve.
                    let is_eloquent_builder = {
                        let bc = base_clean.strip_prefix('\\').unwrap_or(&base_clean);
                        let cn = cls.name.strip_prefix('\\').unwrap_or(&cls.name);
                        // Also construct FQN from file_namespace + name
                        // for classes loaded via PSR-4 where `cls.name`
                        // is the short name only.
                        let fqn_from_ns = cls
                            .file_namespace
                            .as_ref()
                            .map(|ns| format!("{ns}\\{}", cls.name));
                        let fqn_clean = fqn_from_ns
                            .as_deref()
                            .map(|f| f.strip_prefix('\\').unwrap_or(f));
                        bc == ELOQUENT_BUILDER_FQN
                            || cn == ELOQUENT_BUILDER_FQN
                            || fqn_clean == Some(ELOQUENT_BUILDER_FQN)
                    };
                    if is_eloquent_builder {
                        // The first (or only) generic arg is the model type.
                        if let Some(model_arg) = generic_args.first() {
                            let model_clean = model_arg.strip_prefix('\\').unwrap_or(model_arg);
                            let scope_methods =
                                build_scope_methods_for_builder(model_clean, class_loader);
                            for method in scope_methods {
                                if !result.methods.iter().any(|m| {
                                    m.name == method.name && m.is_static == method.is_static
                                }) {
                                    result.methods.push(method);
                                }
                            }
                        }
                    }

                    vec![result]
                } else {
                    vec![cls]
                }
            }
            None => {
                // ── Template parameter bound fallback ──────────────────
                // When the type hint doesn't match any known class, check
                // whether it is a template parameter declared on the
                // owning class.  If it has an `of` bound (e.g.
                // `@template TNode of PDependNode`), resolve the bound
                // type so that completion and go-to-definition still work.
                let loaded;
                let owning = match all_classes.iter().find(|c| c.name == owning_class_name) {
                    Some(c) => Some(c),
                    None => {
                        loaded = class_loader(owning_class_name);
                        loaded.as_ref()
                    }
                };

                // Try class-level template param bounds on the owning class.
                if let Some(owner) = owning
                    && owner.template_params.contains(&short.to_string())
                    && let Some(bound) = owner.template_param_bounds.get(short)
                {
                    return Self::type_hint_to_classes_depth(
                        bound,
                        owning_class_name,
                        all_classes,
                        class_loader,
                        depth + 1,
                    );
                }

                vec![]
            }
        }
    }

    /// Look up a type alias by name and fully expand alias chains.
    ///
    /// Returns the fully expanded type definition string if `hint` is a
    /// known alias, or `None` if it is not. Follows up to 10 levels of
    /// alias indirection to handle aliases that reference other aliases.
    ///
    /// For imported aliases (`from:ClassName:OriginalName`), the source
    /// class is loaded and the original alias is resolved from its
    /// `type_aliases` map.
    ///
    /// Pass an empty `owning_class_name` to search all classes without
    /// priority (used by the array-key completion path).
    pub(crate) fn resolve_type_alias(
        hint: &str,
        owning_class_name: &str,
        all_classes: &[ClassInfo],
        class_loader: &dyn Fn(&str) -> Option<ClassInfo>,
    ) -> Option<String> {
        let mut current = hint.to_string();
        let mut resolved_any = false;

        for _ in 0..10 {
            // Only bare identifiers (no `<`, `{`, `|`, `&`, `?`, `\`) can be
            // type aliases.  Skip anything that looks like a complex type
            // expression to avoid false matches.
            if current.contains('<')
                || current.contains('{')
                || current.contains('|')
                || current.contains('&')
                || current.contains('?')
                || current.contains('\\')
                || current.contains('$')
            {
                break;
            }

            let expanded = Self::resolve_type_alias_once(
                &current,
                owning_class_name,
                all_classes,
                class_loader,
            );

            match expanded {
                Some(def) => {
                    current = def;
                    resolved_any = true;
                }
                None => break,
            }
        }

        if resolved_any { Some(current) } else { None }
    }

    /// Single-level alias lookup (no chaining).
    fn resolve_type_alias_once(
        hint: &str,
        owning_class_name: &str,
        all_classes: &[ClassInfo],
        class_loader: &dyn Fn(&str) -> Option<ClassInfo>,
    ) -> Option<String> {
        // Find the owning class to check its type_aliases.
        let owning_class = all_classes.iter().find(|c| c.name == owning_class_name);

        if let Some(cls) = owning_class
            && let Some(def) = cls.type_aliases.get(hint)
        {
            // Handle imported type aliases: `from:ClassName:OriginalName`
            if let Some(import_ref) = def.strip_prefix("from:") {
                return Self::resolve_imported_type_alias(import_ref, all_classes, class_loader);
            }
            return Some(def.clone());
        }

        // Also check all classes in the file — the type alias might be
        // referenced from a method inside a different class that uses the
        // owning class's return type.  This is rare but handles the case
        // where the owning class name is empty (top-level code) or when
        // the type is used in a context where the owning class is not the
        // declaring class.
        for cls in all_classes {
            if cls.name == owning_class_name {
                continue; // Already checked above.
            }
            if let Some(def) = cls.type_aliases.get(hint) {
                if let Some(import_ref) = def.strip_prefix("from:") {
                    return Self::resolve_imported_type_alias(
                        import_ref,
                        all_classes,
                        class_loader,
                    );
                }
                return Some(def.clone());
            }
        }

        None
    }

    /// Extract the first argument from a comma-separated argument text,
    /// respecting nested parentheses, brackets, and braces.
    fn extract_first_arg_text(args_text: &str) -> Option<String> {
        let trimmed = args_text.trim();
        if trimmed.is_empty() {
            return None;
        }
        let mut depth = 0i32;
        for (i, ch) in trimmed.char_indices() {
            match ch {
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => depth -= 1,
                ',' if depth == 0 => {
                    let arg = trimmed[..i].trim();
                    if !arg.is_empty() {
                        return Some(arg.to_string());
                    }
                    return None;
                }
                _ => {}
            }
        }
        // Single (or last) argument.
        let arg = trimmed.trim();
        if !arg.is_empty() {
            Some(arg.to_string())
        } else {
            None
        }
    }

    /// Resolve the raw return type string of an inline argument expression.
    ///
    /// Handles plain variables (`$customers`), call chains
    /// (`Customer::get()->all()`), and static calls (`ClassName::method()`).
    ///
    /// Returns the raw type string (e.g. `"array<int, Customer>"`) so
    /// that the caller can extract element types from it.
    fn resolve_inline_arg_raw_type(arg_text: &str, ctx: &ResolutionCtx<'_>) -> Option<String> {
        let current_class = ctx.current_class;
        let all_classes = ctx.all_classes;
        let class_loader = ctx.class_loader;

        // ── Plain variable: `$customers` ────────────────────────────────
        if arg_text.starts_with('$')
            && arg_text[1..]
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_')
        {
            // Try docblock annotation first (@var / @param).
            if let Some(raw) = docblock::find_iterable_raw_type_in_source(
                ctx.content,
                ctx.cursor_offset as usize,
                arg_text,
            ) {
                return Some(raw);
            }
            // Fall back to AST-based assignment scanning.
            return Self::resolve_variable_assignment_raw_type(
                arg_text,
                ctx.content,
                ctx.cursor_offset,
                current_class,
                all_classes,
                class_loader,
                ctx.function_loader,
            );
        }

        // ── Call expression ending with `)` ─────────────────────────────
        if arg_text.ends_with(')')
            && let Some((call_body, _args)) = split_call_subject(arg_text)
        {
            // Instance method chain: `expr->method()`
            if let Some(pos) = call_body.rfind("->") {
                // Strip trailing `?` from LHS when the operator was `?->`
                let lhs = call_body[..pos]
                    .strip_suffix('?')
                    .unwrap_or(&call_body[..pos]);
                let method_name = &call_body[pos + 2..];

                let lhs_classes = Self::resolve_target_classes(lhs, AccessKind::Arrow, ctx);
                for cls in &lhs_classes {
                    if let Some(rt) =
                        Self::resolve_method_return_type(cls, method_name, class_loader)
                    {
                        return Some(rt);
                    }
                }
            }

            // Static call: `ClassName::method()`
            if let Some(pos) = call_body.rfind("::") {
                let class_part = &call_body[..pos];
                let method_name = &call_body[pos + 2..];

                let owner = if class_part == "self" || class_part == "static" {
                    current_class.cloned()
                } else {
                    find_class_by_name(all_classes, class_part)
                        .cloned()
                        .or_else(|| class_loader(class_part))
                };
                if let Some(ref cls) = owner
                    && let Some(rt) =
                        Self::resolve_method_return_type(cls, method_name, class_loader)
                {
                    return Some(rt);
                }
            }
        }

        // ── Property access: `$this->prop` or `$var->prop` ──────────────
        if let Some(pos) = arg_text.rfind("->") {
            // Strip trailing `?` from LHS when the operator was `?->`
            let lhs = arg_text[..pos]
                .strip_suffix('?')
                .unwrap_or(&arg_text[..pos]);
            let prop_name = &arg_text[pos + 2..];
            if !prop_name.is_empty() && prop_name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                let lhs_classes = Self::resolve_target_classes(lhs, AccessKind::Arrow, ctx);
                for cls in &lhs_classes {
                    if let Some(rt) = Self::resolve_property_type_hint(cls, prop_name, class_loader)
                    {
                        return Some(rt);
                    }
                }
            }
        }

        None
    }

    pub(crate) fn resolve_imported_type_alias(
        import_ref: &str,
        all_classes: &[ClassInfo],
        class_loader: &dyn Fn(&str) -> Option<ClassInfo>,
    ) -> Option<String> {
        let (source_class_name, original_name) = import_ref.split_once(':')?;

        // Try to find the source class.
        let lookup = source_class_name
            .rsplit('\\')
            .next()
            .unwrap_or(source_class_name);
        let source_class = all_classes
            .iter()
            .find(|c| c.name == lookup)
            .cloned()
            .or_else(|| class_loader(source_class_name));

        let source_class = source_class?;
        let def = source_class.type_aliases.get(original_name)?;

        // Don't follow nested imports — just return the definition.
        if def.starts_with("from:") {
            return None;
        }

        Some(def.clone())
    }
}
