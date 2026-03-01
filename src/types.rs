//! Data types used throughout the PHPantom server.
//!
//! This module contains all the "model" structs and enums that represent
//! extracted PHP information (classes, methods, properties, constants,
//! standalone functions) as well as completion-related types
//! (AccessKind, CompletionTarget, SubjectExpr), PHPStan conditional
//! return type representations, and PHPStan/Psalm array shape types.

use std::collections::HashMap;

/// The return type of `Backend::extract_class_like_members`.
///
/// Contains `(methods, properties, constants, used_traits, trait_precedences, trait_aliases)`
/// extracted from the members of a class-like declaration.
/// Extracted class-like members from a class body.
///
/// Fields: methods, properties, constants, used_traits, trait_precedences,
/// trait_aliases, inline_use_generics.
///
/// The last element holds `@use` generics extracted from docblocks on trait
/// `use` statements inside the class body (e.g. `/** @use BuildsQueries<TModel> */`).
pub type ExtractedMembers = (
    Vec<MethodInfo>,
    Vec<PropertyInfo>,
    Vec<ConstantInfo>,
    Vec<String>,
    Vec<TraitPrecedence>,
    Vec<TraitAlias>,
    Vec<(String, Vec<String>)>,
);

// ─── Array Shape Types ──────────────────────────────────────────────────────

/// A single entry in a PHPStan/Psalm array shape type.
///
/// Array shapes describe the exact structure of an array, including
/// named or positional keys and their value types.
///
/// # Examples
///
/// ```text
/// array{name: string, age: int}       → two entries with keys "name" and "age"
/// array{0: User, 1: Address}          → two entries with numeric keys
/// array{name: string, age?: int}      → "age" is optional
/// array{string, int}                  → implicit keys "0" and "1"
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArrayShapeEntry {
    /// The key name (e.g. `"name"`, `"0"`, `"1"`).
    /// For positional entries without explicit keys, this is the
    /// stringified index (`"0"`, `"1"`, …).
    pub key: String,
    /// The value type string (e.g. `"string"`, `"int"`, `"User"`).
    pub value_type: String,
    /// Whether this key is optional (declared with `?` suffix, e.g. `age?: int`).
    pub optional: bool,
}

/// Visibility of a class member (method, property, or constant).
///
/// In PHP, members without an explicit visibility modifier default to `Public`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    Public,
    Protected,
    Private,
}

/// Stores extracted parameter information from a parsed PHP method.
#[derive(Debug, Clone)]
pub struct ParameterInfo {
    /// The parameter name including the `$` prefix (e.g. "$text").
    pub name: String,
    /// Whether this parameter is required (no default value and not variadic).
    pub is_required: bool,
    /// Optional type hint string (e.g. "string", "int", "?Foo").
    pub type_hint: Option<String>,
    /// Whether this parameter is variadic (has `...`).
    pub is_variadic: bool,
    /// Whether this parameter is passed by reference (has `&`).
    pub is_reference: bool,
}

/// Stores extracted method information from a parsed PHP class.
#[derive(Debug, Clone)]
pub struct MethodInfo {
    /// The method name (e.g. "updateText").
    pub name: String,
    /// Byte offset of the method's name token in the source file.
    ///
    /// Set to the `span.start.offset` of the name `LocalIdentifier` during
    /// parsing.  A value of `0` means "not available" (e.g. for stubs and
    /// synthetic members) — callers should fall back to text search.
    pub name_offset: u32,
    /// The parameters of the method.
    pub parameters: Vec<ParameterInfo>,
    /// Optional return type hint string (e.g. "void", "string", "?int").
    pub return_type: Option<String>,
    /// Whether the method is static.
    pub is_static: bool,
    /// Visibility of the method (public, protected, or private).
    pub visibility: Visibility,
    /// Optional PHPStan conditional return type parsed from the docblock.
    ///
    /// When present, the resolver should use this instead of `return_type`
    /// and resolve the concrete type based on call-site arguments.
    ///
    /// Example docblock:
    /// ```text
    /// @return ($abstract is class-string<TClass> ? TClass : mixed)
    /// ```
    pub conditional_return: Option<ConditionalReturnType>,
    /// Whether this method is marked `@deprecated` in its PHPDoc.
    pub is_deprecated: bool,
    /// Template parameter names declared via `@template` tags in the
    /// method-level docblock.
    ///
    /// For example, a method with `@template T of Model` would have
    /// `template_params: vec!["T".into()]`.
    ///
    /// These are distinct from class-level template parameters
    /// (`ClassInfo::template_params`) and are used for general
    /// method-level generic type substitution at call sites.
    pub template_params: Vec<String>,
    /// Mappings from method-level template parameter names to the method
    /// parameter names (with `$` prefix) that directly bind them via
    /// `@param` annotations.
    ///
    /// For example, `@template T` + `@param T $model` produces
    /// `[("T", "$model")]`.  At call sites the resolver uses these
    /// bindings to infer concrete types for each template parameter
    /// from the actual argument expressions.
    pub template_bindings: Vec<(String, String)>,
    /// Whether this method has the `#[Scope]` attribute (Laravel 11+).
    ///
    /// Methods decorated with `#[\Illuminate\Database\Eloquent\Attributes\Scope]`
    /// are treated as Eloquent scope methods without needing the `scopeX`
    /// naming convention.  The method's own name is used directly as the
    /// public-facing scope name (e.g. `#[Scope] protected function active()`
    /// becomes `User::active()`).
    pub has_scope_attribute: bool,
}

/// Stores extracted property information from a parsed PHP class.
#[derive(Debug, Clone)]
pub struct PropertyInfo {
    /// The property name WITHOUT the `$` prefix (e.g. "name", "age").
    /// This matches PHP access syntax: `$this->name` not `$this->$name`.
    pub name: String,
    /// Byte offset of the property's variable token (`$name`) in the source file.
    ///
    /// Set to the `span.start.offset` of the `DirectVariable` during parsing.
    /// A value of `0` means "not available" — callers should fall back to
    /// text search.
    pub name_offset: u32,
    /// Optional type hint string (e.g. "string", "int").
    pub type_hint: Option<String>,
    /// Whether the property is static.
    pub is_static: bool,
    /// Visibility of the property (public, protected, or private).
    pub visibility: Visibility,
    /// Whether this property is marked `@deprecated` in its PHPDoc.
    pub is_deprecated: bool,
}

/// Stores extracted constant information from a parsed PHP class.
#[derive(Debug, Clone)]
pub struct ConstantInfo {
    /// The constant name (e.g. "MAX_SIZE", "STATUS_ACTIVE").
    pub name: String,
    /// Byte offset of the constant's name token in the source file.
    ///
    /// Set to the `span.start.offset` of the name `LocalIdentifier` during
    /// parsing.  A value of `0` means "not available" — callers should fall
    /// back to text search.
    pub name_offset: u32,
    /// Optional type hint string (e.g. "string", "int").
    pub type_hint: Option<String>,
    /// Visibility of the constant (public, protected, or private).
    pub visibility: Visibility,
    /// Whether this constant is marked `@deprecated` in its PHPDoc.
    pub is_deprecated: bool,
}

/// Describes the access operator that triggered completion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessKind {
    /// Completion triggered after `->` (instance access).
    Arrow,
    /// Completion triggered after `::` (static access).
    DoubleColon,
    /// Completion triggered after `parent::`, `self::`, or `static::`.
    ///
    /// All three keywords use `::` syntax but differ from external static
    /// access (`ClassName::`): they show both static **and** instance
    /// methods (PHP allows `self::nonStaticMethod()`,
    /// `static::nonStaticMethod()`, and `parent::nonStaticMethod()` from
    /// an instance context), plus constants and static properties.
    /// Visibility filtering (e.g. excluding private members for `parent::`)
    /// is handled separately via `current_class_name`.
    ParentDoubleColon,
    /// No specific access operator detected (e.g. inside class body).
    Other,
}

/// The result of analysing what is to the left of `->` or `::`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionTarget {
    /// Whether `->` or `::` was used.
    pub access_kind: AccessKind,
    /// The textual subject before the operator, e.g. `"$this"`, `"self"`,
    /// `"$var"`, `"$this->prop"`, `"ClassName"`.
    pub subject: String,
}

// ─── Resolved Callable Target ───────────────────────────────────────────────

/// The result of resolving a call expression to its callable target.
///
/// Shared between signature help (`resolve_callable`) and named-argument
/// completion (`resolve_named_arg_params`).  Each caller projects the
/// fields it needs: signature help uses all three to build a
/// `SignatureHelp` response; named-arg completion only reads `parameters`.
#[derive(Debug, Clone)]
pub(crate) struct ResolvedCallableTarget {
    /// Human-readable label prefix (e.g. `"App\\Service::process"`,
    /// `"array_map"`).  Used by signature help for the signature label.
    pub label_prefix: String,
    /// The parameters of the callable.
    pub parameters: Vec<ParameterInfo>,
    /// Optional return type string.
    pub return_type: Option<String>,
}

// ─── Structured Subject Expression ──────────────────────────────────────────

/// Structured representation of a completion subject expression.
///
/// Replaces the string-shape dispatch (checking `starts_with('$')`,
/// `contains("->")`, `ends_with(')')`, etc.) with a typed enum so that
/// `resolve_target_classes` and `resolve_call_return_types_expr` can use
/// exhaustive `match` instead of fragile if-else chains.
///
/// Constructed via [`SubjectExpr::parse`] from the raw subject string
/// that the symbol map or text scanner produces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubjectExpr {
    /// `$this` keyword.
    This,
    /// `self` keyword (may appear before `::` or as a subject).
    SelfKw,
    /// `static` keyword.
    StaticKw,
    /// `parent` keyword.
    Parent,
    /// A bare `$variable` (no chain, no brackets).
    Variable(String),
    /// A property chain: `base->property` or `base?->property`.
    ///
    /// The `base` is itself a `SubjectExpr` (e.g. `$this`, `$var`,
    /// or another `PropertyChain`), and `property` is the trailing
    /// identifier after the last `->`.
    PropertyChain {
        /// The expression to the left of the last `->`.
        base: Box<SubjectExpr>,
        /// The property name to the right of the last `->`.
        property: String,
    },
    /// A method/function call expression: `base(args)`.
    ///
    /// `callee` is the structured expression for the call target
    /// (which may be an instance method chain, a static method, or a
    /// bare function name) and `args_text` is the raw text between
    /// the parentheses (preserved for conditional return type
    /// resolution and template substitution).
    CallExpr {
        /// The structured callee expression (e.g. `MethodCall`,
        /// `StaticMethodCall`, `FunctionCall`, or a nested `CallExpr`).
        callee: Box<SubjectExpr>,
        /// Raw text of the arguments between `(` and `)`.
        args_text: String,
    },
    /// Instance method call target: `base->method`.
    ///
    /// This variant represents the *callee* of a call expression
    /// (i.e. what appears to the left of `(…)`), not the full call.
    /// The full call is wrapped in [`CallExpr`](SubjectExpr::CallExpr).
    MethodCall {
        /// The expression to the left of `->`.
        base: Box<SubjectExpr>,
        /// The method name to the right of `->`.
        method: String,
    },
    /// Static method call target: `ClassName::method`.
    ///
    /// Like `MethodCall`, this is the callee portion; the full call
    /// with arguments is wrapped in `CallExpr`.
    StaticMethodCall {
        /// The class name (or keyword) to the left of `::`.
        class: String,
        /// The method name to the right of `::`.
        method: String,
    },
    /// Static member access (enum case or constant): `ClassName::MEMBER`.
    ///
    /// Used when the RHS of `::` is a non-call identifier (e.g.
    /// `Status::Active`, `MyClass::SOME_CONST`).
    StaticAccess {
        /// The class name to the left of `::`.
        class: String,
        /// The member name to the right of `::`.
        member: String,
    },
    /// Constructor call target: `new ClassName`.
    ///
    /// The wrapping `CallExpr` (if any) carries the constructor
    /// arguments.
    NewExpr {
        /// The class name being instantiated.
        class_name: String,
    },
    /// A bare class name used as a subject (e.g. after `new` or before `::`).
    ClassName(String),
    /// A bare function name used as a call target.
    FunctionCall(String),
    /// Array index access: `base['key']` or `base[]`.
    ArrayAccess {
        /// The base expression being indexed.
        base: Box<SubjectExpr>,
        /// The bracket segments in left-to-right order.
        segments: Vec<BracketSegment>,
    },
    /// Inline array literal with index access: `[expr1, expr2][0]`.
    InlineArray {
        /// The raw element expressions inside the `[…]` literal.
        elements: Vec<String>,
        /// The bracket segments after the literal.
        index_segments: Vec<BracketSegment>,
    },
}

/// A single bracket segment in an array access chain.
///
/// Used by [`SubjectExpr::ArrayAccess`] and [`SubjectExpr::InlineArray`]
/// to represent each `[…]` dereference in a chain like `$var['a'][0][]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BracketSegment {
    /// A string-key access, e.g. `['items']`.
    StringKey(String),
    /// A numeric or variable index access, e.g. `[0]` or `[$i]` or `[]`.
    ElementAccess,
}

impl SubjectExpr {
    /// Parse a raw subject string into a structured `SubjectExpr`.
    ///
    /// This is the bridge between the text-based world (symbol map
    /// `subject_text`, text scanner output) and the structured enum.
    /// The parser handles the same patterns that `resolve_target_classes`
    /// and `resolve_call_return_types_expr` previously checked with
    /// `starts_with`, `contains`, `rfind`, etc.
    pub fn parse(subject: &str) -> Self {
        let subject = subject.trim();
        if subject.is_empty() {
            return SubjectExpr::ClassName(String::new());
        }

        // ── Keywords ────────────────────────────────────────────────
        match subject {
            "$this" => return SubjectExpr::This,
            "self" => return SubjectExpr::SelfKw,
            "static" => return SubjectExpr::StaticKw,
            "parent" => return SubjectExpr::Parent,
            _ => {}
        }

        // ── `new ClassName(…)` or `(new ClassName(…))` ──────────────
        if let Some(class_name) = parse_new_expression_class(subject) {
            return SubjectExpr::NewExpr { class_name };
        }

        // ── Inline array literal with index: `[expr][0]` ───────────
        if subject.starts_with('[')
            && subject.contains("][")
            && let Some(result) = parse_inline_array(subject)
        {
            return result;
        }

        // ── Call expression: ends with `)` ──────────────────────────
        // Must be checked before property chains so that
        // `$this->getFactory()` is parsed as a call, not a property.
        if subject.ends_with(')')
            && let Some((call_body, args_text)) = split_call_subject_raw(subject)
        {
            let callee = parse_callee(call_body);
            return SubjectExpr::CallExpr {
                callee: Box::new(callee),
                args_text: args_text.to_string(),
            };
        }

        // ── `$var::member` — class-string variable static access ────
        // When a variable is followed by `::`, it holds a class-string
        // (e.g. `$cls = Pen::class; $cls::make()`).  Parse as
        // `StaticMethodCall` so that callable resolution can route
        // through `resolve_target_classes` with `DoubleColon` access.
        if subject.starts_with('$')
            && subject.contains("::")
            && !subject.ends_with(')')
            && let Some((var_part, member)) = subject.split_once("::")
            && !member.contains("->")
        {
            return SubjectExpr::StaticMethodCall {
                class: var_part.to_string(),
                method: member.to_string(),
            };
        }

        // ── Enum case / static access: `ClassName::Member` ─────────
        // Only match when there is no `->` after `::` (that would be a
        // chain like `ClassName::make()->prop`).
        if !subject.starts_with('$')
            && subject.contains("::")
            && !subject.ends_with(')')
            && let Some((class_part, member)) = subject.split_once("::")
            && !member.contains("->")
        {
            return SubjectExpr::StaticAccess {
                class: class_part.to_string(),
                member: member.to_string(),
            };
        }

        // ── Property chain (split at last depth-0 arrow) ───────────
        if subject.contains("->")
            && let Some((base_str, prop)) = split_last_arrow_raw(subject)
        {
            let base = SubjectExpr::parse(base_str);
            return SubjectExpr::PropertyChain {
                base: Box::new(base),
                property: prop.to_string(),
            };
        }

        // ── Variable with bracket access: `$var['key']` ────────────
        if subject.starts_with('$')
            && subject.contains('[')
            && let Some(result) = parse_variable_array_access(subject)
        {
            return result;
        }

        // ── Bare variable: `$var` ──────────────────────────────────
        if subject.starts_with('$') {
            return SubjectExpr::Variable(subject.to_string());
        }

        // ── Bare class name ────────────────────────────────────────
        SubjectExpr::ClassName(subject.to_string())
    }

    /// Return the raw text representation of this expression.
    ///
    /// This is used as a bridge while callers are migrated: they can
    /// parse a string into `SubjectExpr`, match on it, and still pass
    /// the original text to functions that haven't been converted yet.
    pub fn to_subject_text(&self) -> String {
        match self {
            SubjectExpr::This => "$this".to_string(),
            SubjectExpr::SelfKw => "self".to_string(),
            SubjectExpr::StaticKw => "static".to_string(),
            SubjectExpr::Parent => "parent".to_string(),
            SubjectExpr::Variable(v) => v.clone(),
            SubjectExpr::PropertyChain { base, property } => {
                format!("{}->{}", base.to_subject_text(), property)
            }
            SubjectExpr::CallExpr { callee, args_text } => {
                format!("{}({})", callee.to_subject_text(), args_text)
            }
            SubjectExpr::MethodCall { base, method } => {
                format!("{}->{}", base.to_subject_text(), method)
            }
            SubjectExpr::StaticMethodCall { class, method } => {
                format!("{}::{}", class, method)
            }
            SubjectExpr::StaticAccess { class, member } => {
                format!("{}::{}", class, member)
            }
            SubjectExpr::NewExpr { class_name } => {
                format!("new {}", class_name)
            }
            SubjectExpr::ClassName(name) => name.clone(),
            SubjectExpr::FunctionCall(name) => name.clone(),
            SubjectExpr::ArrayAccess { base, segments } => {
                let mut s = base.to_subject_text();
                for seg in segments {
                    match seg {
                        BracketSegment::StringKey(k) => {
                            s.push_str(&format!("['{}']", k));
                        }
                        BracketSegment::ElementAccess => {
                            s.push_str("[]");
                        }
                    }
                }
                s
            }
            SubjectExpr::InlineArray {
                elements,
                index_segments,
            } => {
                let mut s = format!("[{}]", elements.join(", "));
                for seg in index_segments {
                    match seg {
                        BracketSegment::StringKey(k) => {
                            s.push_str(&format!("['{}']", k));
                        }
                        BracketSegment::ElementAccess => {
                            s.push_str("[]");
                        }
                    }
                }
                s
            }
        }
    }

    /// Returns `true` if this expression is one of the "current class"
    /// keywords (`$this`, `self`, `static`).
    pub fn is_self_like(&self) -> bool {
        matches!(
            self,
            SubjectExpr::This | SubjectExpr::SelfKw | SubjectExpr::StaticKw
        )
    }

    /// Parse the callee portion of a call expression (everything before
    /// the opening `(`).
    ///
    /// This distinguishes instance method calls (`base->method`), static
    /// method calls (`Class::method`), constructor calls (`new Class`),
    /// and bare function names.
    pub fn parse_callee(call_body: &str) -> SubjectExpr {
        parse_callee(call_body)
    }
}

// ─── SubjectExpr parsing helpers ────────────────────────────────────────────

/// Parse the callee portion of a call expression (everything before the
/// opening `(`).
///
/// This distinguishes instance method calls (`base->method`), static
/// method calls (`Class::method`), constructor calls (`new Class`),
/// and bare function names.
fn parse_callee(call_body: &str) -> SubjectExpr {
    let call_body = call_body.trim();

    // ── `new ClassName` ─────────────────────────────────────────
    if let Some(class_name) = call_body
        .strip_prefix("new ")
        .map(|s| s.trim().trim_start_matches('\\'))
        .filter(|s| !s.is_empty())
    {
        // Strip trailing parens content if any (e.g. from `(new Foo(…))`)
        let clean = class_name
            .find(|c: char| c == '(' || c.is_whitespace())
            .map_or(class_name, |pos| &class_name[..pos]);
        return SubjectExpr::NewExpr {
            class_name: clean.to_string(),
        };
    }

    // ── Instance method: `base->method` ─────────────────────────
    // Use rfind to find the last `->` at depth 0 (outside parens).
    if let Some((base_str, method)) = split_last_arrow_raw(call_body) {
        let base = SubjectExpr::parse(base_str);
        return SubjectExpr::MethodCall {
            base: Box::new(base),
            method: method.to_string(),
        };
    }

    // ── Static method: `Class::method` ──────────────────────────
    if let Some(pos) = call_body.rfind("::") {
        let class_part = &call_body[..pos];
        let method_name = &call_body[pos + 2..];
        return SubjectExpr::StaticMethodCall {
            class: class_part.to_string(),
            method: method_name.to_string(),
        };
    }

    // ── Bare variable: `$fn` ────────────────────────────────────
    if call_body.starts_with('$') {
        return SubjectExpr::Variable(call_body.to_string());
    }

    // ── Bare function name ──────────────────────────────────────
    SubjectExpr::FunctionCall(call_body.to_string())
}

/// Split a subject at the **last** `->` or `?->` at depth 0.
///
/// Returns `(base, property)` or `None` if no arrow is found.
/// Arrows inside balanced parentheses are ignored.
fn split_last_arrow_raw(subject: &str) -> Option<(&str, &str)> {
    let bytes = subject.as_bytes();
    let mut depth = 0i32;
    let mut last_arrow: Option<(usize, usize)> = None;

    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'(' => depth += 1,
            b')' => depth -= 1,
            b'-' if depth == 0 && i + 1 < bytes.len() && bytes[i + 1] == b'>' => {
                let arrow_start = if i > 0 && bytes[i - 1] == b'?' {
                    i - 1
                } else {
                    i
                };
                let prop_start = i + 2;
                last_arrow = Some((arrow_start, prop_start));
                i += 2;
                continue;
            }
            _ => {}
        }
        i += 1;
    }

    let (arrow_start, prop_start) = last_arrow?;
    if prop_start >= subject.len() {
        return None;
    }
    let base = &subject[..arrow_start];
    let prop = &subject[prop_start..];
    if base.is_empty() || prop.is_empty() {
        return None;
    }
    Some((base, prop))
}

/// Split a call expression at the matching `(` for the trailing `)`.
///
/// Returns `(call_body, args_text)` where `call_body` is the expression
/// before `(` and `args_text` is the trimmed content between `(` and `)`.
fn split_call_subject_raw(subject: &str) -> Option<(&str, &str)> {
    let inner = subject.strip_suffix(')')?;
    let bytes = inner.as_bytes();
    let mut depth: u32 = 0;
    let mut open = None;
    for i in (0..bytes.len()).rev() {
        match bytes[i] {
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
    let call_body = &inner[..open];
    let args_text = inner[open + 1..].trim();
    if call_body.is_empty() {
        return None;
    }
    Some((call_body, args_text))
}

/// Parse a `new ClassName` or `(new ClassName(…))` expression and extract
/// the class name.
fn parse_new_expression_class(s: &str) -> Option<String> {
    // Strip balanced outer parentheses.
    let inner = if s.starts_with('(') && s.ends_with(')') {
        &s[1..s.len() - 1]
    } else {
        s
    };
    let rest = inner.trim().strip_prefix("new ")?;
    let rest = rest.trim_start();
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

/// Parse a variable with bracket access like `$var['key'][0]`.
fn parse_variable_array_access(subject: &str) -> Option<SubjectExpr> {
    let first_bracket = subject.find('[')?;
    let base_var = &subject[..first_bracket];
    if base_var.len() < 2 {
        return None;
    }

    let mut segments = Vec::new();
    let mut rest = &subject[first_bracket..];

    while rest.starts_with('[') {
        let close = rest.find(']')?;
        let inner = rest[1..close].trim();

        if let Some(key) = inner
            .strip_prefix('\'')
            .and_then(|s| s.strip_suffix('\''))
            .or_else(|| inner.strip_prefix('"').and_then(|s| s.strip_suffix('"')))
        {
            segments.push(BracketSegment::StringKey(key.to_string()));
        } else {
            segments.push(BracketSegment::ElementAccess);
        }

        rest = &rest[close + 1..];
    }

    if segments.is_empty() {
        return None;
    }

    Some(SubjectExpr::ArrayAccess {
        base: Box::new(SubjectExpr::parse(base_var)),
        segments,
    })
}

/// Parse an inline array literal with index access: `[expr1, expr2][0]`.
fn parse_inline_array(subject: &str) -> Option<SubjectExpr> {
    let split_pos = subject.find("][")?;
    let literal_text = &subject[..split_pos + 1];
    if !literal_text.starts_with('[') || !literal_text.ends_with(']') {
        return None;
    }
    let inner = literal_text[1..literal_text.len() - 1].trim();
    let elements: Vec<String> = inner.split(',').map(|e| e.trim().to_string()).collect();

    // Parse the bracket segments after the literal.
    let index_part = &subject[split_pos + 1..];
    let mut index_segments = Vec::new();
    let mut rest = index_part;
    while rest.starts_with('[') {
        let close = rest.find(']')?;
        let idx_inner = rest[1..close].trim();
        if let Some(key) = idx_inner
            .strip_prefix('\'')
            .and_then(|s| s.strip_suffix('\''))
            .or_else(|| {
                idx_inner
                    .strip_prefix('"')
                    .and_then(|s| s.strip_suffix('"'))
            })
        {
            index_segments.push(BracketSegment::StringKey(key.to_string()));
        } else {
            index_segments.push(BracketSegment::ElementAccess);
        }
        rest = &rest[close + 1..];
    }

    Some(SubjectExpr::InlineArray {
        elements,
        index_segments,
    })
}

/// Stores extracted information about a standalone PHP function.
///
/// This is used for global / namespaced functions defined outside of classes,
/// typically found in files listed by Composer's `autoload_files.php`.
#[derive(Debug, Clone)]
pub struct FunctionInfo {
    /// The function name (e.g. "array_map", "myHelper").
    pub name: String,
    /// Byte offset of the function's name token in the source file.
    ///
    /// Set to the `span.start.offset` of the name `LocalIdentifier` during
    /// parsing.  A value of `0` means "not available" (e.g. for stubs and
    /// synthetic entries) — callers should fall back to text search.
    pub name_offset: u32,
    /// The parameters of the function.
    pub parameters: Vec<ParameterInfo>,
    /// Optional return type hint string (e.g. "void", "string", "?int").
    pub return_type: Option<String>,
    /// The namespace this function is declared in, if any.
    /// For example, `Amp\delay` would have namespace `Some("Amp")`.
    pub namespace: Option<String>,
    /// Optional PHPStan conditional return type parsed from the docblock.
    ///
    /// When present, the resolver should use this instead of `return_type`
    /// and resolve the concrete type based on call-site arguments.
    ///
    /// Example docblock:
    /// ```text
    /// @return ($abstract is class-string<TClass> ? TClass : \Illuminate\Foundation\Application)
    /// ```
    pub conditional_return: Option<ConditionalReturnType>,
    /// Type assertions parsed from `@phpstan-assert` / `@psalm-assert`
    /// annotations in the function's docblock.
    ///
    /// These allow user-defined functions to act as custom type guards,
    /// narrowing the type of a parameter after the call (or conditionally
    /// when used in an `if` condition).
    ///
    /// Example docblocks:
    /// ```text
    /// @phpstan-assert User $value           — unconditional assertion
    /// @phpstan-assert !User $value          — negated assertion
    /// @phpstan-assert-if-true User $value   — assertion when return is true
    /// @phpstan-assert-if-false User $value  — assertion when return is false
    /// ```
    pub type_assertions: Vec<TypeAssertion>,
    /// Whether this function is marked `@deprecated` in its PHPDoc.
    pub is_deprecated: bool,
}

// ─── PHPStan Type Assertions ────────────────────────────────────────────────

/// A type assertion annotation parsed from `@phpstan-assert` /
/// `@psalm-assert` (and their `-if-true` / `-if-false` variants).
///
/// These annotations let any function or method act as a custom type
/// guard, telling the analyser that a parameter has been narrowed to
/// a specific type after the call succeeds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeAssertion {
    /// When the assertion applies.
    pub kind: AssertionKind,
    /// The parameter name **with** the `$` prefix (e.g. `"$value"`).
    pub param_name: String,
    /// The asserted type (e.g. `"User"`, `"AdminUser"`).
    pub asserted_type: String,
    /// Whether the assertion is negated (`!Type`), meaning the parameter
    /// is guaranteed to *not* be this type.
    pub negated: bool,
}

/// When a `@phpstan-assert` annotation takes effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssertionKind {
    /// `@phpstan-assert` — unconditional: after the function returns
    /// (without throwing), the assertion holds for all subsequent code.
    Always,
    /// `@phpstan-assert-if-true` — the assertion holds when the function
    /// returns `true` (i.e. inside the `if` body).
    IfTrue,
    /// `@phpstan-assert-if-false` — the assertion holds when the function
    /// returns `false` (i.e. inside the `else` body, or the `if` body of
    /// a negated condition).
    IfFalse,
}

// ─── PHPStan Conditional Return Types ───────────────────────────────────────

/// A parsed PHPStan conditional return type expression.
///
/// PHPStan allows `@return` annotations that conditionally resolve to
/// different types based on the value/type of a parameter.  For example:
///
/// ```text
/// @return ($abstract is class-string<TClass> ? TClass
///           : ($abstract is null ? \Illuminate\Foundation\Application : mixed))
/// ```
///
/// This enum represents the recursive structure of such expressions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConditionalReturnType {
    /// A concrete (terminal) type, e.g. `\Illuminate\Foundation\Application`
    /// or `mixed`.
    Concrete(String),

    /// A conditional branch:
    /// `($param is Condition ? ThenType : ElseType)`
    Conditional {
        /// The parameter name **without** the `$` prefix (e.g. `"abstract"`).
        param_name: String,
        /// The condition being checked.
        condition: ParamCondition,
        /// The type when the condition is satisfied.
        then_type: Box<ConditionalReturnType>,
        /// The type when the condition is not satisfied.
        else_type: Box<ConditionalReturnType>,
    },
}

/// The kind of condition in a PHPStan conditional return type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParamCondition {
    /// `$param is class-string<T>` — when the argument is a `::class` constant,
    /// the return type is the class itself.
    ClassString,

    /// `$param is null` — typically used for parameters with `= null` defaults
    /// to return a known concrete type when no argument is provided.
    IsNull,

    /// `$param is \SomeType` — a general type check (e.g. `\Closure`, `string`).
    IsType(String),
}

/// A trait `insteadof` adaptation.
///
/// When a class uses multiple traits that define the same method, PHP
/// requires an explicit `insteadof` declaration to resolve the conflict.
///
/// # Example
///
/// ```php
/// use TraitA, TraitB {
///     TraitA::method insteadof TraitB;
/// }
/// ```
///
/// This means TraitA's version of `method` wins and TraitB's is excluded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraitPrecedence {
    /// The trait that provides the winning method (e.g. `"TraitA"`).
    pub trait_name: String,
    /// The method name being resolved (e.g. `"method"`).
    pub method_name: String,
    /// The traits whose versions of the method are excluded
    /// (e.g. `["TraitB"]`).
    pub insteadof: Vec<String>,
}

/// A trait `as` alias adaptation.
///
/// Creates an alias for a trait method, optionally changing its visibility.
///
/// # Examples
///
/// ```php
/// use TraitA, TraitB {
///     TraitB::method as traitBMethod;          // rename
///     TraitA::method as protected;             // visibility-only change
///     TraitB::method as private altMethod;     // rename + visibility change
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraitAlias {
    /// The trait that provides the method (e.g. `Some("TraitB")`).
    /// `None` when the method reference is unqualified (e.g. `method as …`).
    pub trait_name: Option<String>,
    /// The original method name (e.g. `"method"`).
    pub method_name: String,
    /// The alias name, if any (e.g. `Some("traitBMethod")`).
    /// `None` when only the visibility is changed (e.g. `method as protected`).
    pub alias: Option<String>,
    /// Optional visibility override (e.g. `Some(Visibility::Protected)`).
    pub visibility: Option<Visibility>,
}

/// The syntactic kind of a class-like declaration.
///
/// PHP has four class-like constructs that share the same `ClassInfo`
/// representation.  This enum lets callers distinguish them when the
/// difference matters (e.g. `throw new` completion should only offer
/// concrete classes, not interfaces or traits).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ClassLikeKind {
    /// A regular `class` declaration (the default).
    #[default]
    Class,
    /// An `interface` declaration.
    Interface,
    /// A `trait` declaration.
    Trait,
    /// An `enum` declaration.
    Enum,
}

/// Stores extracted class information from a parsed PHP file.
/// All data is owned so we don't depend on the parser's arena lifetime.
#[derive(Debug, Clone, Default)]
pub struct ClassInfo {
    /// The syntactic kind of this class-like declaration.
    pub kind: ClassLikeKind,
    /// The name of the class (e.g. "User").
    pub name: String,
    /// The methods defined directly in this class.
    pub methods: Vec<MethodInfo>,
    /// The properties defined directly in this class.
    pub properties: Vec<PropertyInfo>,
    /// The constants defined directly in this class.
    pub constants: Vec<ConstantInfo>,
    /// Byte offset where the class body starts (left brace).
    pub start_offset: u32,
    /// Byte offset where the class body ends (right brace).
    pub end_offset: u32,
    /// Byte offset of the `class` / `interface` / `trait` / `enum` keyword
    /// token in the source file.
    ///
    /// Used with `offset_to_position` to convert directly to an LSP
    /// `Position`.  A value of `0` means "not available" (e.g. for
    /// synthetic classes or anonymous classes) — callers return `None`.
    pub keyword_offset: u32,
    /// The parent class name from the `extends` clause, if any.
    /// This is the raw name as written in source (e.g. "BaseClass", "Foo\\Bar").
    pub parent_class: Option<String>,
    /// Interface names from the `implements` clause (classes and enums only).
    ///
    /// These are resolved to fully-qualified names during post-processing
    /// (see `resolve_parent_class_names` in `parser/ast_update.rs`).
    /// Used by "Go to Implementation" to find classes that implement a
    /// given interface.
    pub interfaces: Vec<String>,
    /// Trait names used by this class via `use TraitName;` statements.
    /// These are resolved to fully-qualified names during post-processing.
    pub used_traits: Vec<String>,
    /// Class names from `@mixin` docblock tags.
    /// These declare that this class exposes public members from the listed
    /// classes via magic methods (`__call`, `__get`, `__set`, etc.).
    /// Resolved to fully-qualified names during post-processing.
    pub mixins: Vec<String>,
    /// Whether the class is declared `final`.
    ///
    /// Final classes cannot be extended, so `static::` is equivalent to
    /// `self::` and need not be offered as a separate completion subject.
    pub is_final: bool,
    /// Whether the class is declared `abstract`.
    ///
    /// Abstract classes cannot be instantiated directly, so they should
    /// be excluded from contexts like `throw new` or `new` completion
    /// where only concrete classes are valid.
    pub is_abstract: bool,
    /// Whether this class is marked `@deprecated` in its PHPDoc.
    pub is_deprecated: bool,
    /// Template parameter names declared via `@template` / `@template-covariant`
    /// / `@template-contravariant` tags in the class-level docblock.
    ///
    /// For example, `Collection` with `@template TKey` and `@template TValue`
    /// would have `template_params: vec!["TKey".into(), "TValue".into()]`.
    pub template_params: Vec<String>,
    /// Upper bounds for template parameters, keyed by parameter name.
    ///
    /// Populated from the `of` clause in `@template` tags. For example,
    /// `@template TNode of PDependNode` produces `("TNode", "PDependNode")`.
    ///
    /// When a type hint resolves to a template parameter name that cannot be
    /// concretely substituted, the resolver falls back to this bound so that
    /// completion and go-to-definition still work against the bound type.
    pub template_param_bounds: HashMap<String, String>,
    /// Generic type arguments from `@extends` / `@phpstan-extends` tags.
    ///
    /// Each entry is `(ClassName, [TypeArg1, TypeArg2, …])`.
    /// For example, `@extends Collection<int, Language>` produces
    /// `("Collection", ["int", "Language"])`.
    pub extends_generics: Vec<(String, Vec<String>)>,
    /// Generic type arguments from `@implements` / `@phpstan-implements` tags.
    ///
    /// Each entry is `(InterfaceName, [TypeArg1, TypeArg2, …])`.
    /// For example, `@implements ArrayAccess<int, User>` produces
    /// `("ArrayAccess", ["int", "User"])`.
    pub implements_generics: Vec<(String, Vec<String>)>,
    /// Generic type arguments from `@use` / `@phpstan-use` tags.
    ///
    /// Each entry is `(TraitName, [TypeArg1, TypeArg2, …])`.
    /// For example, `@use HasFactory<UserFactory>` produces
    /// `("HasFactory", ["UserFactory"])`.
    ///
    /// When a trait declares `@template T` and a class uses it with
    /// `@use SomeTrait<ConcreteType>`, the trait's template parameter `T`
    /// is substituted with `ConcreteType` in all inherited methods and
    /// properties.
    pub use_generics: Vec<(String, Vec<String>)>,
    /// Type aliases defined via `@phpstan-type` / `@psalm-type` tags in the
    /// class-level docblock, and imported via `@phpstan-import-type` /
    /// `@psalm-import-type`.
    ///
    /// Maps alias name → type definition string.
    /// For example, `@phpstan-type UserData array{name: string, email: string}`
    /// produces `("UserData", "array{name: string, email: string}")`.
    ///
    /// These are consulted during type resolution so that a method returning
    /// `UserData` resolves to the underlying `array{name: string, email: string}`.
    pub type_aliases: HashMap<String, String>,
    /// Trait `insteadof` precedence adaptations.
    ///
    /// When a class uses multiple traits with conflicting method names,
    /// `insteadof` declarations specify which trait's version wins.
    /// For example, `TraitA::method insteadof TraitB` means TraitA's
    /// `method` is used and TraitB's is excluded.
    pub trait_precedences: Vec<TraitPrecedence>,
    /// Trait `as` alias adaptations.
    ///
    /// Creates aliases for trait methods, optionally with visibility changes.
    /// For example, `TraitB::method as traitBMethod` adds a new method
    /// `traitBMethod` that is a copy of TraitB's `method`.
    pub trait_aliases: Vec<TraitAlias>,
    /// Raw class-level docblock text, preserved for deferred parsing.
    ///
    /// `@method` and `@property` / `@property-read` / `@property-write`
    /// tags are **not** parsed eagerly into `methods` / `properties`.
    /// Instead, the raw docblock string is stored here and parsed lazily
    /// by the `PHPDocProvider` virtual member provider when completion or
    /// go-to-definition actually needs virtual members.
    ///
    /// Other docblock tags (`@template`, `@extends`, `@deprecated`, etc.)
    /// are still parsed eagerly because they affect class metadata that is
    /// needed during indexing and inheritance resolution.
    pub class_docblock: Option<String>,
    /// The namespace this class was declared in.
    ///
    /// Populated during parsing from the enclosing `namespace { }` block.
    /// For files with a single namespace (the common PSR-4 case) this
    /// matches the file-level namespace.  For files with multiple
    /// namespace blocks (e.g. `example.php` with inline stubs) each class
    /// carries its own namespace so that `find_class_in_ast_map` can
    /// distinguish two classes with the same short name in different
    /// namespace blocks (e.g. `Illuminate\Database\Eloquent\Builder` vs
    /// `Illuminate\Database\Query\Builder`).
    pub file_namespace: Option<String>,
    /// Custom collection class for Eloquent models.
    ///
    /// Detected from two Laravel mechanisms:
    ///
    /// 1. The `#[CollectedBy(CustomCollection::class)]` attribute on the
    ///    model class.
    /// 2. The `/** @use HasCollection<CustomCollection> */` docblock
    ///    annotation on a `use HasCollection;` trait usage.
    ///
    /// When set, the `LaravelModelProvider` replaces
    /// `\Illuminate\Database\Eloquent\Collection` with this class in
    /// relationship property types and Builder-forwarded return types
    /// (e.g. `get()`, `all()`).
    pub custom_collection: Option<String>,
    /// Eloquent cast definitions extracted from the `$casts` property
    /// initializer or the `casts()` method body.
    ///
    /// Each entry maps a column name to a cast type string (e.g.
    /// `("created_at", "datetime")`, `("is_admin", "boolean")`).
    /// The `LaravelModelProvider` uses these to synthesize typed virtual
    /// properties, mapping cast type strings to PHP types (e.g.
    /// `datetime` to `Carbon\Carbon`, `boolean` to `bool`).
    pub casts_definitions: Vec<(String, String)>,
    /// Eloquent attribute defaults extracted from the `$attributes`
    /// property initializer.
    ///
    /// Each entry maps a column name to a PHP type string inferred from
    /// the literal default value (e.g. `("role", "string")`,
    /// `("is_active", "bool")`, `("login_count", "int")`).
    /// The `LaravelModelProvider` uses these as a fallback when no
    /// `$casts` entry exists for the same column.
    pub attributes_definitions: Vec<(String, String)>,
    /// Column names extracted from `$fillable`, `$guarded`, and
    /// `$hidden` property arrays.
    ///
    /// These are simple string lists (no type information), so the
    /// `LaravelModelProvider` synthesizes `mixed`-typed virtual
    /// properties as a last-resort fallback when a column is not
    /// already covered by `$casts` or `$attributes`.
    pub column_names: Vec<String>,
}

// ─── ClassInfo helpers ──────────────────────────────────────────────────────

impl ClassInfo {
    /// Look up the stored `name_offset` for a member by name and kind.
    ///
    /// Returns `Some(offset)` when the member exists and has a non-zero
    /// offset, or `None` otherwise.  The `kind` string should be one of
    /// `"method"`, `"property"`, or `"constant"`.
    pub(crate) fn member_name_offset(&self, name: &str, kind: &str) -> Option<u32> {
        let off = match kind {
            "method" => self
                .methods
                .iter()
                .find(|m| m.name == name)
                .map(|m| m.name_offset),
            "property" => self
                .properties
                .iter()
                .find(|p| p.name == name)
                .map(|p| p.name_offset),
            "constant" => self
                .constants
                .iter()
                .find(|c| c.name == name)
                .map(|c| c.name_offset),
            _ => None,
        };
        off.filter(|&o| o > 0)
    }

    /// Push a `ClassInfo` into `results` only if no existing entry shares
    /// the same class name.  This is the single place where completion /
    /// resolution code deduplicates candidate classes.
    pub(crate) fn push_unique(results: &mut Vec<ClassInfo>, cls: ClassInfo) {
        if !results.iter().any(|c| c.name == cls.name) {
            results.push(cls);
        }
    }

    /// Extend `results` with entries from `new_classes`, skipping any whose
    /// name already appears in `results`.
    pub(crate) fn extend_unique(results: &mut Vec<ClassInfo>, new_classes: Vec<ClassInfo>) {
        for cls in new_classes {
            Self::push_unique(results, cls);
        }
    }
}

// ─── File Context ───────────────────────────────────────────────────────────

/// Cached per-file context retrieved from the `Backend` maps.
///
/// Bundles the three pieces of file-level metadata that almost every
/// handler needs: the parsed classes, the `use` statement import table,
/// and the declared namespace.  Constructed by
/// [`Backend::file_context`](crate::Backend) to replace the repeated
/// lock-and-unwrap boilerplate that was duplicated across completion,
/// definition, and implementation handlers.
pub(crate) struct FileContext {
    /// Classes extracted from the file's AST (from `ast_map`).
    pub classes: Vec<ClassInfo>,
    /// Import table mapping short names to fully-qualified names
    /// (from `use_map`).
    pub use_map: HashMap<String, String>,
    /// The file's declared namespace, if any (from `namespace_map`).
    pub namespace: Option<String>,
}

// ─── Eloquent Constants ─────────────────────────────────────────────────────

/// The fully-qualified name of the Eloquent Collection class.
///
/// Used by the `LaravelModelProvider` to detect and replace collection
/// return types when a model declares a custom collection class.
pub const ELOQUENT_COLLECTION_FQN: &str = "Illuminate\\Database\\Eloquent\\Collection";

// ─── Recursion Depth Limits ─────────────────────────────────────────────────
//
// Centralised constants for the maximum recursion depth allowed when
// walking inheritance chains, trait hierarchies, mixin graphs, and type
// alias resolution.  Defining them in one place ensures that the same
// limit is used consistently across the inheritance, definition, and
// completion modules.

/// Maximum depth when walking the `extends` parent chain
/// (class → parent → grandparent → …).
pub(crate) const MAX_INHERITANCE_DEPTH: u32 = 20;

/// Maximum depth when recursing into `use Trait` hierarchies
/// (a trait can itself `use` other traits).
pub(crate) const MAX_TRAIT_DEPTH: u32 = 20;

/// Maximum depth when recursing into `@mixin` class graphs.
pub(crate) const MAX_MIXIN_DEPTH: u32 = 10;

/// Maximum depth when resolving `@phpstan-type` / `@psalm-type` aliases
/// (an alias can reference another alias).
pub(crate) const MAX_ALIAS_DEPTH: u8 = 10;

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Keywords ────────────────────────────────────────────────────────

    #[test]
    fn parse_this() {
        assert_eq!(SubjectExpr::parse("$this"), SubjectExpr::This);
    }

    #[test]
    fn parse_self() {
        assert_eq!(SubjectExpr::parse("self"), SubjectExpr::SelfKw);
    }

    #[test]
    fn parse_static() {
        assert_eq!(SubjectExpr::parse("static"), SubjectExpr::StaticKw);
    }

    #[test]
    fn parse_parent() {
        assert_eq!(SubjectExpr::parse("parent"), SubjectExpr::Parent);
    }

    // ── Bare variable ───────────────────────────────────────────────────

    #[test]
    fn parse_bare_variable() {
        assert_eq!(
            SubjectExpr::parse("$user"),
            SubjectExpr::Variable("$user".to_string())
        );
    }

    #[test]
    fn parse_bare_variable_underscore() {
        assert_eq!(
            SubjectExpr::parse("$my_var"),
            SubjectExpr::Variable("$my_var".to_string())
        );
    }

    // ── Bare class name ─────────────────────────────────────────────────

    #[test]
    fn parse_bare_class_name() {
        assert_eq!(
            SubjectExpr::parse("User"),
            SubjectExpr::ClassName("User".to_string())
        );
    }

    #[test]
    fn parse_fqn_class_name() {
        assert_eq!(
            SubjectExpr::parse("App\\Models\\User"),
            SubjectExpr::ClassName("App\\Models\\User".to_string())
        );
    }

    // ── Property chains ─────────────────────────────────────────────────

    #[test]
    fn parse_this_property() {
        assert_eq!(
            SubjectExpr::parse("$this->name"),
            SubjectExpr::PropertyChain {
                base: Box::new(SubjectExpr::This),
                property: "name".to_string(),
            }
        );
    }

    #[test]
    fn parse_nullsafe_this_property() {
        assert_eq!(
            SubjectExpr::parse("$this?->name"),
            SubjectExpr::PropertyChain {
                base: Box::new(SubjectExpr::This),
                property: "name".to_string(),
            }
        );
    }

    #[test]
    fn parse_var_property() {
        assert_eq!(
            SubjectExpr::parse("$user->address"),
            SubjectExpr::PropertyChain {
                base: Box::new(SubjectExpr::Variable("$user".to_string())),
                property: "address".to_string(),
            }
        );
    }

    #[test]
    fn parse_nested_property_chain() {
        assert_eq!(
            SubjectExpr::parse("$user->address->city"),
            SubjectExpr::PropertyChain {
                base: Box::new(SubjectExpr::PropertyChain {
                    base: Box::new(SubjectExpr::Variable("$user".to_string())),
                    property: "address".to_string(),
                }),
                property: "city".to_string(),
            }
        );
    }

    #[test]
    fn parse_nullsafe_var_property() {
        assert_eq!(
            SubjectExpr::parse("$user?->address"),
            SubjectExpr::PropertyChain {
                base: Box::new(SubjectExpr::Variable("$user".to_string())),
                property: "address".to_string(),
            }
        );
    }

    // ── Static access (enum case / constant) ────────────────────────────

    #[test]
    fn parse_static_access_enum_case() {
        assert_eq!(
            SubjectExpr::parse("Status::Active"),
            SubjectExpr::StaticAccess {
                class: "Status".to_string(),
                member: "Active".to_string(),
            }
        );
    }

    #[test]
    fn parse_static_access_constant() {
        assert_eq!(
            SubjectExpr::parse("MyClass::SOME_CONST"),
            SubjectExpr::StaticAccess {
                class: "MyClass".to_string(),
                member: "SOME_CONST".to_string(),
            }
        );
    }

    // ── Call expressions ────────────────────────────────────────────────

    #[test]
    fn parse_function_call_no_args() {
        assert_eq!(
            SubjectExpr::parse("app()"),
            SubjectExpr::CallExpr {
                callee: Box::new(SubjectExpr::FunctionCall("app".to_string())),
                args_text: "".to_string(),
            }
        );
    }

    #[test]
    fn parse_function_call_with_args() {
        assert_eq!(
            SubjectExpr::parse("app(User::class)"),
            SubjectExpr::CallExpr {
                callee: Box::new(SubjectExpr::FunctionCall("app".to_string())),
                args_text: "User::class".to_string(),
            }
        );
    }

    #[test]
    fn parse_method_call() {
        assert_eq!(
            SubjectExpr::parse("$this->getUser()"),
            SubjectExpr::CallExpr {
                callee: Box::new(SubjectExpr::MethodCall {
                    base: Box::new(SubjectExpr::This),
                    method: "getUser".to_string(),
                }),
                args_text: "".to_string(),
            }
        );
    }

    #[test]
    fn parse_var_method_call() {
        assert_eq!(
            SubjectExpr::parse("$service->process()"),
            SubjectExpr::CallExpr {
                callee: Box::new(SubjectExpr::MethodCall {
                    base: Box::new(SubjectExpr::Variable("$service".to_string())),
                    method: "process".to_string(),
                }),
                args_text: "".to_string(),
            }
        );
    }

    #[test]
    fn parse_static_method_call() {
        assert_eq!(
            SubjectExpr::parse("User::find(1)"),
            SubjectExpr::CallExpr {
                callee: Box::new(SubjectExpr::StaticMethodCall {
                    class: "User".to_string(),
                    method: "find".to_string(),
                }),
                args_text: "1".to_string(),
            }
        );
    }

    #[test]
    fn parse_self_static_method_call() {
        assert_eq!(
            SubjectExpr::parse("self::create()"),
            SubjectExpr::CallExpr {
                callee: Box::new(SubjectExpr::StaticMethodCall {
                    class: "self".to_string(),
                    method: "create".to_string(),
                }),
                args_text: "".to_string(),
            }
        );
    }

    #[test]
    fn parse_parent_method_call() {
        assert_eq!(
            SubjectExpr::parse("parent::build()"),
            SubjectExpr::CallExpr {
                callee: Box::new(SubjectExpr::StaticMethodCall {
                    class: "parent".to_string(),
                    method: "build".to_string(),
                }),
                args_text: "".to_string(),
            }
        );
    }

    #[test]
    fn parse_chained_method_call() {
        // $this->getFactory()->create()
        let parsed = SubjectExpr::parse("$this->getFactory()->create()");
        assert_eq!(
            parsed,
            SubjectExpr::CallExpr {
                callee: Box::new(SubjectExpr::MethodCall {
                    base: Box::new(SubjectExpr::CallExpr {
                        callee: Box::new(SubjectExpr::MethodCall {
                            base: Box::new(SubjectExpr::This),
                            method: "getFactory".to_string(),
                        }),
                        args_text: "".to_string(),
                    }),
                    method: "create".to_string(),
                }),
                args_text: "".to_string(),
            }
        );
    }

    #[test]
    fn parse_chained_method_then_property() {
        // $this->getFactory()->config
        assert_eq!(
            SubjectExpr::parse("$this->getFactory()->config"),
            SubjectExpr::PropertyChain {
                base: Box::new(SubjectExpr::CallExpr {
                    callee: Box::new(SubjectExpr::MethodCall {
                        base: Box::new(SubjectExpr::This),
                        method: "getFactory".to_string(),
                    }),
                    args_text: "".to_string(),
                }),
                property: "config".to_string(),
            }
        );
    }

    #[test]
    fn parse_static_call_chain_then_property() {
        // BlogAuthor::whereIn(…)->first()->posts
        let parsed = SubjectExpr::parse("BlogAuthor::whereIn('id', [1])->first()->posts");
        assert_eq!(
            parsed,
            SubjectExpr::PropertyChain {
                base: Box::new(SubjectExpr::CallExpr {
                    callee: Box::new(SubjectExpr::MethodCall {
                        base: Box::new(SubjectExpr::CallExpr {
                            callee: Box::new(SubjectExpr::StaticMethodCall {
                                class: "BlogAuthor".to_string(),
                                method: "whereIn".to_string(),
                            }),
                            args_text: "'id', [1]".to_string(),
                        }),
                        method: "first".to_string(),
                    }),
                    args_text: "".to_string(),
                }),
                property: "posts".to_string(),
            }
        );
    }

    #[test]
    fn parse_nested_call_args() {
        // Environment::get(self::country())
        assert_eq!(
            SubjectExpr::parse("Environment::get(self::country())"),
            SubjectExpr::CallExpr {
                callee: Box::new(SubjectExpr::StaticMethodCall {
                    class: "Environment".to_string(),
                    method: "get".to_string(),
                }),
                args_text: "self::country()".to_string(),
            }
        );
    }

    // ── new expressions ─────────────────────────────────────────────────

    #[test]
    fn parse_new_expression_bare() {
        // `new Builder()` is recognised by the `new` handler before the
        // call-expression handler runs, so it collapses to `NewExpr`.
        assert_eq!(
            SubjectExpr::parse("new Builder()"),
            SubjectExpr::NewExpr {
                class_name: "Builder".to_string(),
            }
        );
    }

    #[test]
    fn parse_new_expression_parenthesized() {
        assert_eq!(
            SubjectExpr::parse("(new Builder())"),
            SubjectExpr::NewExpr {
                class_name: "Builder".to_string(),
            }
        );
    }

    #[test]
    fn parse_new_expression_no_parens() {
        assert_eq!(
            SubjectExpr::parse("(new Builder)"),
            SubjectExpr::NewExpr {
                class_name: "Builder".to_string(),
            }
        );
    }

    #[test]
    fn parse_new_expression_fqn() {
        assert_eq!(
            SubjectExpr::parse("(new \\App\\Builder())"),
            SubjectExpr::NewExpr {
                class_name: "App\\Builder".to_string(),
            }
        );
    }

    // ── Variable callable: `$fn()` ──────────────────────────────────────

    #[test]
    fn parse_variable_call() {
        assert_eq!(
            SubjectExpr::parse("$fn()"),
            SubjectExpr::CallExpr {
                callee: Box::new(SubjectExpr::Variable("$fn".to_string())),
                args_text: "".to_string(),
            }
        );
    }

    #[test]
    fn parse_variable_call_with_args() {
        assert_eq!(
            SubjectExpr::parse("$fn(42, 'hello')"),
            SubjectExpr::CallExpr {
                callee: Box::new(SubjectExpr::Variable("$fn".to_string())),
                args_text: "42, 'hello'".to_string(),
            }
        );
    }

    // ── Array access ────────────────────────────────────────────────────

    #[test]
    fn parse_variable_string_key_access() {
        assert_eq!(
            SubjectExpr::parse("$response['items']"),
            SubjectExpr::ArrayAccess {
                base: Box::new(SubjectExpr::Variable("$response".to_string())),
                segments: vec![BracketSegment::StringKey("items".to_string())],
            }
        );
    }

    #[test]
    fn parse_variable_element_access() {
        assert_eq!(
            SubjectExpr::parse("$list[0]"),
            SubjectExpr::ArrayAccess {
                base: Box::new(SubjectExpr::Variable("$list".to_string())),
                segments: vec![BracketSegment::ElementAccess],
            }
        );
    }

    #[test]
    fn parse_variable_chained_bracket_access() {
        assert_eq!(
            SubjectExpr::parse("$response['items'][0]"),
            SubjectExpr::ArrayAccess {
                base: Box::new(SubjectExpr::Variable("$response".to_string())),
                segments: vec![
                    BracketSegment::StringKey("items".to_string()),
                    BracketSegment::ElementAccess,
                ],
            }
        );
    }

    #[test]
    fn parse_variable_empty_bracket() {
        assert_eq!(
            SubjectExpr::parse("$arr[]"),
            SubjectExpr::ArrayAccess {
                base: Box::new(SubjectExpr::Variable("$arr".to_string())),
                segments: vec![BracketSegment::ElementAccess],
            }
        );
    }

    // ── Inline array literal ────────────────────────────────────────────

    #[test]
    fn parse_inline_array_literal() {
        assert_eq!(
            SubjectExpr::parse("[Customer::first()][0]"),
            SubjectExpr::InlineArray {
                elements: vec!["Customer::first()".to_string()],
                index_segments: vec![BracketSegment::ElementAccess],
            }
        );
    }

    #[test]
    fn parse_inline_array_literal_multiple_elements() {
        assert_eq!(
            SubjectExpr::parse("[$a, $b][0]"),
            SubjectExpr::InlineArray {
                elements: vec!["$a".to_string(), "$b".to_string()],
                index_segments: vec![BracketSegment::ElementAccess],
            }
        );
    }

    // ── is_self_like helper ─────────────────────────────────────────────

    #[test]
    fn is_self_like_keywords() {
        assert!(SubjectExpr::This.is_self_like());
        assert!(SubjectExpr::SelfKw.is_self_like());
        assert!(SubjectExpr::StaticKw.is_self_like());
    }

    #[test]
    fn is_self_like_non_keywords() {
        assert!(!SubjectExpr::Parent.is_self_like());
        assert!(!SubjectExpr::Variable("$x".to_string()).is_self_like());
        assert!(!SubjectExpr::ClassName("User".to_string()).is_self_like());
    }

    // ── to_subject_text round-trip ──────────────────────────────────────

    #[test]
    fn round_trip_this() {
        assert_eq!(SubjectExpr::parse("$this").to_subject_text(), "$this");
    }

    #[test]
    fn round_trip_self() {
        assert_eq!(SubjectExpr::parse("self").to_subject_text(), "self");
    }

    #[test]
    fn round_trip_variable() {
        assert_eq!(SubjectExpr::parse("$user").to_subject_text(), "$user");
    }

    #[test]
    fn round_trip_property_chain() {
        assert_eq!(
            SubjectExpr::parse("$this->name").to_subject_text(),
            "$this->name"
        );
    }

    #[test]
    fn round_trip_nested_property_chain() {
        assert_eq!(
            SubjectExpr::parse("$user->address->city").to_subject_text(),
            "$user->address->city"
        );
    }

    #[test]
    fn round_trip_function_call() {
        assert_eq!(SubjectExpr::parse("app()").to_subject_text(), "app()");
    }

    #[test]
    fn round_trip_method_call() {
        assert_eq!(
            SubjectExpr::parse("$this->getUser()").to_subject_text(),
            "$this->getUser()"
        );
    }

    #[test]
    fn round_trip_static_method_call() {
        assert_eq!(
            SubjectExpr::parse("User::find(1)").to_subject_text(),
            "User::find(1)"
        );
    }

    #[test]
    fn round_trip_static_access() {
        assert_eq!(
            SubjectExpr::parse("Status::Active").to_subject_text(),
            "Status::Active"
        );
    }

    #[test]
    fn round_trip_class_name() {
        assert_eq!(SubjectExpr::parse("User").to_subject_text(), "User");
    }

    #[test]
    fn round_trip_chained_call_then_property() {
        assert_eq!(
            SubjectExpr::parse("$this->getFactory()->config").to_subject_text(),
            "$this->getFactory()->config"
        );
    }

    #[test]
    fn round_trip_chained_method_calls() {
        assert_eq!(
            SubjectExpr::parse("$this->getFactory()->create()").to_subject_text(),
            "$this->getFactory()->create()"
        );
    }

    #[test]
    fn round_trip_array_access() {
        // Numeric index `[0]` is parsed as `ElementAccess` and
        // round-trips to `[]` (the index value is not preserved).
        assert_eq!(
            SubjectExpr::parse("$response['items'][0]").to_subject_text(),
            "$response['items'][]"
        );
    }

    // ── Whitespace trimming ─────────────────────────────────────────────

    #[test]
    fn parse_trims_whitespace() {
        assert_eq!(SubjectExpr::parse("  $this  "), SubjectExpr::This);
    }

    // ── Edge: empty string ──────────────────────────────────────────────

    #[test]
    fn parse_empty_string() {
        assert_eq!(
            SubjectExpr::parse(""),
            SubjectExpr::ClassName(String::new())
        );
    }

    // ── Complex chain: static → method → method → property ─────────────

    #[test]
    fn parse_complex_chain() {
        // ClassName::make()->process()->result
        let parsed = SubjectExpr::parse("ClassName::make()->process()->result");
        assert_eq!(
            parsed,
            SubjectExpr::PropertyChain {
                base: Box::new(SubjectExpr::CallExpr {
                    callee: Box::new(SubjectExpr::MethodCall {
                        base: Box::new(SubjectExpr::CallExpr {
                            callee: Box::new(SubjectExpr::StaticMethodCall {
                                class: "ClassName".to_string(),
                                method: "make".to_string(),
                            }),
                            args_text: "".to_string(),
                        }),
                        method: "process".to_string(),
                    }),
                    args_text: "".to_string(),
                }),
                property: "result".to_string(),
            }
        );
    }

    // ── Method call with nested parens in args ──────────────────────────

    #[test]
    fn parse_call_with_nested_parens_in_args() {
        // app(config('key'))
        let parsed = SubjectExpr::parse("app(config('key'))");
        assert_eq!(
            parsed,
            SubjectExpr::CallExpr {
                callee: Box::new(SubjectExpr::FunctionCall("app".to_string())),
                args_text: "config('key')".to_string(),
            }
        );
    }

    // ── Double-quoted string key in array access ────────────────────────

    #[test]
    fn parse_double_quoted_array_key() {
        assert_eq!(
            SubjectExpr::parse("$data[\"name\"]"),
            SubjectExpr::ArrayAccess {
                base: Box::new(SubjectExpr::Variable("$data".to_string())),
                segments: vec![BracketSegment::StringKey("name".to_string())],
            }
        );
    }

    // ── Call expression as callee base: `app()->make()` ─────────────────

    #[test]
    fn parse_function_call_then_method() {
        let parsed = SubjectExpr::parse("app()->make()");
        assert_eq!(
            parsed,
            SubjectExpr::CallExpr {
                callee: Box::new(SubjectExpr::MethodCall {
                    base: Box::new(SubjectExpr::CallExpr {
                        callee: Box::new(SubjectExpr::FunctionCall("app".to_string())),
                        args_text: "".to_string(),
                    }),
                    method: "make".to_string(),
                }),
                args_text: "".to_string(),
            }
        );
    }

    // ── `$this->prop` call vs property disambiguation ───────────────────

    #[test]
    fn parse_this_method_vs_property() {
        // `$this->getFactory()` should be a call, not a property chain
        let parsed = SubjectExpr::parse("$this->getFactory()");
        assert!(matches!(parsed, SubjectExpr::CallExpr { .. }));

        // `$this->factory` should be a property chain
        let parsed = SubjectExpr::parse("$this->factory");
        assert!(matches!(parsed, SubjectExpr::PropertyChain { .. }));
    }

    // ── Static access vs static call disambiguation ─────────────────────

    #[test]
    fn parse_static_access_vs_call() {
        // `Status::Active` → StaticAccess
        assert!(matches!(
            SubjectExpr::parse("Status::Active"),
            SubjectExpr::StaticAccess { .. }
        ));

        // `User::find(1)` → CallExpr wrapping StaticMethodCall
        assert!(matches!(
            SubjectExpr::parse("User::find(1)"),
            SubjectExpr::CallExpr { .. }
        ));
    }

    // ── Static call chain with `->` after `::` ─────────────────────────

    #[test]
    fn parse_static_then_arrow_chain() {
        // `ClassName::make()->config` should be PropertyChain, not StaticAccess
        let parsed = SubjectExpr::parse("ClassName::make()->config");
        assert!(matches!(parsed, SubjectExpr::PropertyChain { .. }));
    }
}
