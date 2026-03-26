//! "Generate constructor" code action.
//!
//! When the cursor is inside a class that has non-static properties but
//! no `__construct` method, this module offers a code action to generate
//! a constructor that accepts each qualifying property as a parameter
//! and assigns it in the body.
//!
//! **Code action kind:** `refactor.rewrite`.

use std::collections::HashMap;

use bumpalo::Bump;
use mago_span::HasSpan;
use mago_syntax::ast::class_like::member::ClassLikeMember;
use mago_syntax::ast::class_like::property::{Property, PropertyItem};
use mago_syntax::ast::modifier::Modifier;
use mago_syntax::ast::*;
use tower_lsp::lsp_types::*;

use super::cursor_context::{CursorContext, find_cursor_context};
use crate::Backend;
use crate::docblock::{extract_var_type, get_docblock_text_for_node};
use crate::parser::extract_hint_string;
use crate::util::offset_to_position;

// ── Data types ──────────────────────────────────────────────────────────────

/// A property that qualifies for inclusion in the generated constructor.
struct QualifyingProperty {
    /// Property name without the `$` prefix.
    name: String,
    /// Type hint string for the constructor parameter, if available.
    type_hint: Option<String>,
    /// Default value text (e.g. `'active'`, `[]`), if the property has one.
    default_value: Option<String>,
}

// ── Public API ──────────────────────────────────────────────────────────────

impl Backend {
    /// Collect "Generate constructor" code actions for the cursor position.
    ///
    /// When the cursor is inside a class body that has at least one
    /// non-static property and no existing `__construct` method, this
    /// produces a single code action that inserts a constructor.
    pub(crate) fn collect_generate_constructor_actions(
        &self,
        uri: &str,
        content: &str,
        params: &CodeActionParams,
        out: &mut Vec<CodeActionOrCommand>,
    ) {
        let doc_uri: Url = match uri.parse() {
            Ok(u) => u,
            Err(_) => return,
        };

        let cursor_offset = crate::util::position_to_offset(content, params.range.start);

        let arena = Bump::new();
        let file_id = mago_database::file::FileId::new("input.php");
        let program = mago_syntax::parser::parse_file_content(&arena, file_id, content);

        let ctx = find_cursor_context(&program.statements, cursor_offset);

        let all_members = match &ctx {
            CursorContext::InClassLike { all_members, .. } => *all_members,
            _ => return,
        };

        // If a __construct already exists, do not offer the action.
        if has_constructor(all_members) {
            return;
        }

        let trivia = program.trivia.as_slice();

        // Collect qualifying properties (non-static).
        let props = collect_qualifying_properties(all_members, content, trivia);

        if props.is_empty() {
            return;
        }

        // Detect indentation from existing class members.
        let indent = detect_indent_from_members(all_members, content);

        // Build the constructor text.
        let constructor_text = build_constructor(&props, &indent);

        // Find the insertion point: after the last property declaration,
        // before any methods or other members.
        let insert_offset = find_insertion_offset(all_members, content);
        let insert_pos = offset_to_position(content, insert_offset);

        let title = "Generate constructor".to_string();

        let edit = TextEdit {
            range: Range {
                start: insert_pos,
                end: insert_pos,
            },
            new_text: constructor_text,
        };

        let mut changes = HashMap::new();
        changes.insert(doc_uri, vec![edit]);

        out.push(CodeActionOrCommand::CodeAction(CodeAction {
            title,
            kind: Some(CodeActionKind::REFACTOR_REWRITE),
            diagnostics: None,
            edit: Some(WorkspaceEdit {
                changes: Some(changes),
                document_changes: None,
                change_annotations: None,
            }),
            command: None,
            is_preferred: Some(false),
            disabled: None,
            data: None,
        }));
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Check whether the class already has a `__construct` method.
fn has_constructor<'a>(members: &Sequence<'a, ClassLikeMember<'a>>) -> bool {
    members.iter().any(|m| {
        if let ClassLikeMember::Method(method) = m {
            method.name.value.eq_ignore_ascii_case("__construct")
        } else {
            false
        }
    })
}

/// Collect all non-static properties from the class members,
/// in declaration order.  Readonly properties are included because they
/// *must* be initialized in the constructor.
fn collect_qualifying_properties<'a>(
    members: &Sequence<'a, ClassLikeMember<'a>>,
    content: &str,
    trivia: &[Trivia<'a>],
) -> Vec<QualifyingProperty> {
    let mut result = Vec::new();

    for member in members.iter() {
        let plain = match member {
            ClassLikeMember::Property(Property::Plain(p)) => p,
            _ => continue,
        };

        // Skip static properties.
        if is_static(plain.modifiers.iter()) {
            continue;
        }

        // Extract the native type hint for the property.
        let native_hint = plain.hint.as_ref().map(|h| extract_hint_string(h));

        // Try to get a docblock @var type if there's no native hint
        // or if we want to use it as a fallback.
        let docblock_type =
            get_docblock_text_for_node(trivia, content, plain).and_then(extract_var_type);

        for item in plain.items.iter() {
            let var_name = item.variable().name;
            let bare_name = var_name.strip_prefix('$').unwrap_or(var_name);

            // Determine the type hint for the parameter.
            let type_hint = if let Some(ref hint) = native_hint {
                Some(hint.clone())
            } else if let Some(ref doc_type) = docblock_type {
                // Only use docblock type if it's a single, non-compound type.
                // Skip complex types like `array{key: value}` or `int|string`.
                if is_simple_type(doc_type) {
                    Some(doc_type.clone())
                } else {
                    None::<String>
                }
            } else {
                None
            };

            // Extract default value if the property has one.
            let default_value = if let PropertyItem::Concrete(concrete) = item {
                let span = concrete.value.span();
                let start = span.start.offset as usize;
                let end = span.end.offset as usize;
                content.get(start..end).map(|s| s.trim().to_string())
            } else {
                None
            };

            result.push(QualifyingProperty {
                name: bare_name.to_string(),
                type_hint,
                default_value,
            });
        }
    }

    result
}

/// Check whether a type string is a simple (non-compound) type suitable
/// for use as a parameter type hint.
///
/// Returns `false` for union types (`int|string`), intersection types,
/// array shapes, generic syntax, etc.
fn is_simple_type(type_str: &str) -> bool {
    // Reject union and intersection types.
    if type_str.contains('|') || type_str.contains('&') {
        return false;
    }
    // Reject array shapes and generic syntax.
    if type_str.contains('{') || type_str.contains('<') {
        return false;
    }
    // Allow nullable types like `?string`.
    let inner = type_str.strip_prefix('?').unwrap_or(type_str);
    // Must be a simple identifier (possibly with namespace separators).
    !inner.is_empty()
        && inner
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '\\')
}

/// Find the byte offset where the constructor should be inserted.
///
/// The constructor is inserted after the last property declaration and
/// before any methods or other non-property members.  If there are no
/// properties before any methods, it's inserted after the class opening
/// brace.
fn find_insertion_offset<'a>(members: &Sequence<'a, ClassLikeMember<'a>>, content: &str) -> usize {
    // Find the end of the last property declaration.
    let mut last_property_end: Option<u32> = None;
    let mut first_non_property_start: Option<u32> = None;

    for member in members.iter() {
        match member {
            ClassLikeMember::Property(_) => {
                let span = member.span();
                last_property_end = Some(span.end.offset);
            }
            ClassLikeMember::Method(_)
            | ClassLikeMember::Constant(_)
            | ClassLikeMember::TraitUse(_)
            | ClassLikeMember::EnumCase(_) => {
                if first_non_property_start.is_none() && last_property_end.is_some() {
                    first_non_property_start = Some(member.span().start.offset);
                }
                // If we haven't seen any properties yet, record this as
                // the first non-property so we know where to insert
                // relative to the class opening brace.
                if last_property_end.is_none() && first_non_property_start.is_none() {
                    first_non_property_start = Some(member.span().start.offset);
                }
            }
        }
    }

    if let Some(end) = last_property_end {
        // Insert after the last property.  Find the end of the line
        // containing the property's semicolon.
        find_line_end(content, end as usize)
    } else {
        // No properties at all — shouldn't happen because we check for
        // qualifying properties, but handle gracefully.
        0
    }
}

/// Find the end of the line at or after the given offset (past the newline).
fn find_line_end(content: &str, offset: usize) -> usize {
    if let Some(nl) = content[offset..].find('\n') {
        offset + nl + 1
    } else {
        content.len()
    }
}

/// Detect indentation from the first class member's position in the source.
///
/// Looks at the line containing the first member to determine the
/// indent string.  Falls back to four spaces.
fn detect_indent_from_members<'a>(
    members: &Sequence<'a, ClassLikeMember<'a>>,
    content: &str,
) -> String {
    // Find the first member and look at its line's leading whitespace.
    if let Some(first) = members.first() {
        let offset = first.span().start.offset as usize;
        // Walk backwards from the member's start to find the beginning of the line.
        let line_start = content[..offset]
            .rfind('\n')
            .map(|pos| pos + 1)
            .unwrap_or(0);
        let line_prefix = &content[line_start..offset];
        let indent: String = line_prefix
            .chars()
            .take_while(|c| c.is_whitespace())
            .collect();
        if !indent.is_empty() {
            return indent;
        }
    }

    // Fallback: four spaces.
    "    ".to_string()
}

/// Build the constructor source text from the qualifying properties.
fn build_constructor(props: &[QualifyingProperty], indent: &str) -> String {
    let mut result = String::new();

    result.push('\n');
    result.push_str(indent);
    result.push_str("public function __construct(");

    // Build parameter list.
    // Parameters with default values must come after required parameters.
    // We preserve declaration order but PHP requires defaults at the end,
    // so we separate them.
    let mut required_params = Vec::new();
    let mut optional_params = Vec::new();

    for prop in props {
        let mut param = String::new();

        if let Some(ref hint) = prop.type_hint {
            param.push_str(hint);
            param.push(' ');
        }

        param.push('$');
        param.push_str(&prop.name);

        if let Some(ref default) = prop.default_value {
            param.push_str(" = ");
            param.push_str(default);
            optional_params.push(param);
        } else {
            required_params.push(param);
        }
    }

    let all_params: Vec<&str> = required_params
        .iter()
        .chain(optional_params.iter())
        .map(|s| s.as_str())
        .collect();

    result.push_str(&all_params.join(", "));
    result.push_str(")\n");
    result.push_str(indent);
    result.push_str("{\n");

    // Build assignment body — use declaration order for assignments,
    // not the reordered parameter order.
    for prop in props {
        result.push_str(indent);
        result.push_str(indent);
        result.push_str("$this->");
        result.push_str(&prop.name);
        result.push_str(" = $");
        result.push_str(&prop.name);
        result.push_str(";\n");
    }

    result.push_str(indent);
    result.push_str("}\n");

    result
}

/// Check if any modifier is `static`.
fn is_static<'a>(modifiers: impl Iterator<Item = &'a Modifier<'a>>) -> bool {
    modifiers
        .into_iter()
        .any(|m| matches!(m, Modifier::Static(_)))
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── is_simple_type ──────────────────────────────────────────────────

    #[test]
    fn simple_type_accepts_basic() {
        assert!(is_simple_type("string"));
        assert!(is_simple_type("int"));
        assert!(is_simple_type("array"));
        assert!(is_simple_type("bool"));
    }

    #[test]
    fn simple_type_accepts_nullable() {
        assert!(is_simple_type("?string"));
        assert!(is_simple_type("?Foo"));
    }

    #[test]
    fn simple_type_accepts_fqn() {
        assert!(is_simple_type("App\\Models\\User"));
        assert!(is_simple_type("?App\\Models\\User"));
    }

    #[test]
    fn simple_type_rejects_union() {
        assert!(!is_simple_type("int|string"));
    }

    #[test]
    fn simple_type_rejects_intersection() {
        assert!(!is_simple_type("Foo&Bar"));
    }

    #[test]
    fn simple_type_rejects_array_shape() {
        assert!(!is_simple_type("array{name: string}"));
    }

    #[test]
    fn simple_type_rejects_generic() {
        assert!(!is_simple_type("Collection<User>"));
    }

    #[test]
    fn simple_type_rejects_empty() {
        assert!(!is_simple_type(""));
    }

    // ── build_constructor ───────────────────────────────────────────────

    #[test]
    fn builds_basic_constructor() {
        let props = vec![
            QualifyingProperty {
                name: "name".to_string(),
                type_hint: Some("string".to_string()),
                default_value: None,
            },
            QualifyingProperty {
                name: "age".to_string(),
                type_hint: Some("int".to_string()),
                default_value: None,
            },
        ];

        let result = build_constructor(&props, "    ");
        assert!(result.contains("public function __construct(string $name, int $age)"));
        assert!(result.contains("$this->name = $name;"));
        assert!(result.contains("$this->age = $age;"));
    }

    #[test]
    fn builds_constructor_with_defaults() {
        let props = vec![
            QualifyingProperty {
                name: "name".to_string(),
                type_hint: Some("string".to_string()),
                default_value: None,
            },
            QualifyingProperty {
                name: "status".to_string(),
                type_hint: Some("string".to_string()),
                default_value: Some("'active'".to_string()),
            },
        ];

        let result = build_constructor(&props, "    ");
        assert!(
            result.contains("string $name, string $status = 'active'"),
            "required params before optional: {result}"
        );
    }

    #[test]
    fn defaults_reordered_before_required() {
        let props = vec![
            QualifyingProperty {
                name: "status".to_string(),
                type_hint: Some("string".to_string()),
                default_value: Some("'draft'".to_string()),
            },
            QualifyingProperty {
                name: "name".to_string(),
                type_hint: Some("string".to_string()),
                default_value: None,
            },
        ];

        let result = build_constructor(&props, "    ");
        // Required parameter $name should come before optional $status.
        let name_pos = result.find("$name").unwrap();
        let status_pos = result.find("$status").unwrap();
        assert!(
            name_pos < status_pos,
            "required params should come first: {result}"
        );
    }

    #[test]
    fn builds_constructor_without_type_hints() {
        let props = vec![QualifyingProperty {
            name: "data".to_string(),
            type_hint: None,
            default_value: None,
        }];

        let result = build_constructor(&props, "    ");
        assert!(
            result.contains("($data)"),
            "untyped param should not have type: {result}"
        );
    }

    #[test]
    fn builds_constructor_with_nullable_type() {
        let props = vec![QualifyingProperty {
            name: "label".to_string(),
            type_hint: Some("?string".to_string()),
            default_value: None,
        }];

        let result = build_constructor(&props, "    ");
        assert!(
            result.contains("?string $label"),
            "nullable type preserved: {result}"
        );
    }

    #[test]
    fn builds_constructor_with_union_type() {
        let props = vec![QualifyingProperty {
            name: "id".to_string(),
            type_hint: Some("int|string".to_string()),
            default_value: None,
        }];

        let result = build_constructor(&props, "    ");
        assert!(
            result.contains("int|string $id"),
            "union type preserved: {result}"
        );
    }

    #[test]
    fn respects_tab_indentation() {
        let props = vec![QualifyingProperty {
            name: "name".to_string(),
            type_hint: Some("string".to_string()),
            default_value: None,
        }];

        let result = build_constructor(&props, "\t");
        assert!(
            result.contains("\tpublic function __construct("),
            "should use tab indent: {result}"
        );
        assert!(
            result.contains("\t\t$this->name = $name;"),
            "body should use double tab: {result}"
        );
    }

    // ── has_constructor ─────────────────────────────────────────────────

    #[test]
    fn detects_existing_constructor() {
        let arena = Box::leak(Box::new(Bump::new()));
        let file_id = mago_database::file::FileId::new("input.php");
        let php = "<?php\nclass Foo {\n    public function __construct() {}\n}";
        let program = mago_syntax::parser::parse_file_content(arena, file_id, php);

        // Find the class and check for constructor.
        let ctx = find_cursor_context(&program.statements, 20);
        if let CursorContext::InClassLike { all_members, .. } = &ctx {
            assert!(has_constructor(all_members));
        } else {
            panic!("should find class");
        }
    }

    #[test]
    fn detects_no_constructor() {
        let arena = Box::leak(Box::new(Bump::new()));
        let file_id = mago_database::file::FileId::new("input.php");
        let php = "<?php\nclass Foo {\n    public string $name;\n}";
        let program = mago_syntax::parser::parse_file_content(arena, file_id, php);

        let ctx = find_cursor_context(&program.statements, 20);
        if let CursorContext::InClassLike { all_members, .. } = &ctx {
            assert!(!has_constructor(all_members));
        } else {
            panic!("should find class");
        }
    }

    #[test]
    fn detects_constructor_case_insensitive() {
        let arena = Box::leak(Box::new(Bump::new()));
        let file_id = mago_database::file::FileId::new("input.php");
        let php = "<?php\nclass Foo {\n    public function __CONSTRUCT() {}\n}";
        let program = mago_syntax::parser::parse_file_content(arena, file_id, php);

        let ctx = find_cursor_context(&program.statements, 20);
        if let CursorContext::InClassLike { all_members, .. } = &ctx {
            assert!(has_constructor(all_members));
        } else {
            panic!("should find class");
        }
    }

    // ── collect_qualifying_properties ───────────────────────────────────

    #[test]
    fn collects_non_static() {
        let arena = Box::leak(Box::new(Bump::new()));
        let file_id = mago_database::file::FileId::new("input.php");
        let php = "<?php\nclass Foo {\n    public string $name;\n    private int $age;\n}";
        let program = mago_syntax::parser::parse_file_content(arena, file_id, php);

        let ctx = find_cursor_context(&program.statements, 20);
        if let CursorContext::InClassLike { all_members, .. } = &ctx {
            let props = collect_qualifying_properties(all_members, php, program.trivia.as_slice());
            assert_eq!(props.len(), 2);
            assert_eq!(props[0].name, "name");
            assert_eq!(props[0].type_hint.as_deref(), Some("string"));
            assert_eq!(props[1].name, "age");
            assert_eq!(props[1].type_hint.as_deref(), Some("int"));
        } else {
            panic!("should find class");
        }
    }

    #[test]
    fn skips_static_properties() {
        let arena = Box::leak(Box::new(Bump::new()));
        let file_id = mago_database::file::FileId::new("input.php");
        let php = "<?php\nclass Foo {\n    public string $name;\n    public static int $count;\n}";
        let program = mago_syntax::parser::parse_file_content(arena, file_id, php);

        let ctx = find_cursor_context(&program.statements, 20);
        if let CursorContext::InClassLike { all_members, .. } = &ctx {
            let props = collect_qualifying_properties(all_members, php, program.trivia.as_slice());
            assert_eq!(props.len(), 1);
            assert_eq!(props[0].name, "name");
        } else {
            panic!("should find class");
        }
    }

    #[test]
    fn includes_readonly_properties() {
        let arena = Box::leak(Box::new(Bump::new()));
        let file_id = mago_database::file::FileId::new("input.php");
        let php = "<?php\nclass Foo {\n    public string $name;\n    public readonly int $id;\n}";
        let program = mago_syntax::parser::parse_file_content(arena, file_id, php);

        let ctx = find_cursor_context(&program.statements, 20);
        if let CursorContext::InClassLike { all_members, .. } = &ctx {
            let props = collect_qualifying_properties(all_members, php, program.trivia.as_slice());
            assert_eq!(props.len(), 2);
            assert_eq!(props[0].name, "name");
            assert_eq!(props[1].name, "id");
            assert_eq!(props[1].type_hint.as_deref(), Some("int"));
        } else {
            panic!("should find class");
        }
    }

    #[test]
    fn extracts_default_values() {
        let arena = Box::leak(Box::new(Bump::new()));
        let file_id = mago_database::file::FileId::new("input.php");
        let php = "<?php\nclass Foo {\n    public string $status = 'active';\n}";
        let program = mago_syntax::parser::parse_file_content(arena, file_id, php);

        let ctx = find_cursor_context(&program.statements, 20);
        if let CursorContext::InClassLike { all_members, .. } = &ctx {
            let props = collect_qualifying_properties(all_members, php, program.trivia.as_slice());
            assert_eq!(props.len(), 1);
            assert_eq!(props[0].name, "status");
            assert_eq!(props[0].default_value.as_deref(), Some("'active'"));
        } else {
            panic!("should find class");
        }
    }

    #[test]
    fn extracts_docblock_type_when_no_native_hint() {
        let arena = Box::leak(Box::new(Bump::new()));
        let file_id = mago_database::file::FileId::new("input.php");
        let php = "<?php\nclass Foo {\n    /** @var string */\n    public $name;\n}";
        let program = mago_syntax::parser::parse_file_content(arena, file_id, php);

        let ctx = find_cursor_context(&program.statements, 20);
        if let CursorContext::InClassLike { all_members, .. } = &ctx {
            let props = collect_qualifying_properties(all_members, php, program.trivia.as_slice());
            assert_eq!(props.len(), 1);
            assert_eq!(props[0].name, "name");
            assert_eq!(props[0].type_hint.as_deref(), Some("string"));
        } else {
            panic!("should find class");
        }
    }

    #[test]
    fn skips_compound_docblock_type() {
        let arena = Box::leak(Box::new(Bump::new()));
        let file_id = mago_database::file::FileId::new("input.php");
        let php = "<?php\nclass Foo {\n    /** @var int|string */\n    public $id;\n}";
        let program = mago_syntax::parser::parse_file_content(arena, file_id, php);

        let ctx = find_cursor_context(&program.statements, 20);
        if let CursorContext::InClassLike { all_members, .. } = &ctx {
            let props = collect_qualifying_properties(all_members, php, program.trivia.as_slice());
            assert_eq!(props.len(), 1);
            assert_eq!(props[0].name, "id");
            assert!(
                props[0].type_hint.is_none(),
                "compound docblock type should be skipped"
            );
        } else {
            panic!("should find class");
        }
    }

    #[test]
    fn preserves_nullable_native_type() {
        let arena = Box::leak(Box::new(Bump::new()));
        let file_id = mago_database::file::FileId::new("input.php");
        let php = "<?php\nclass Foo {\n    public ?string $name;\n}";
        let program = mago_syntax::parser::parse_file_content(arena, file_id, php);

        let ctx = find_cursor_context(&program.statements, 20);
        if let CursorContext::InClassLike { all_members, .. } = &ctx {
            let props = collect_qualifying_properties(all_members, php, program.trivia.as_slice());
            assert_eq!(props.len(), 1);
            assert_eq!(props[0].type_hint.as_deref(), Some("?string"));
        } else {
            panic!("should find class");
        }
    }

    #[test]
    fn preserves_union_native_type() {
        let arena = Box::leak(Box::new(Bump::new()));
        let file_id = mago_database::file::FileId::new("input.php");
        let php = "<?php\nclass Foo {\n    public int|string $id;\n}";
        let program = mago_syntax::parser::parse_file_content(arena, file_id, php);

        let ctx = find_cursor_context(&program.statements, 20);
        if let CursorContext::InClassLike { all_members, .. } = &ctx {
            let props = collect_qualifying_properties(all_members, php, program.trivia.as_slice());
            assert_eq!(props.len(), 1);
            assert_eq!(props[0].type_hint.as_deref(), Some("int|string"));
        } else {
            panic!("should find class");
        }
    }
}
