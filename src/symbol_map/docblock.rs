//! Docblock symbol extraction helpers for the symbol map.
//!
//! This module contains functions that scan PHPDoc comment blocks for
//! type references in supported tags (`@param`, `@return`, `@var`,
//! `@template`, `@method`, etc.) and emit [`SymbolSpan`] entries with
//! correct file-level byte offsets.

use mago_span::HasSpan;
use mago_syntax::ast::*;

use crate::docblock::types::{split_intersection_depth0, split_type_token, split_union_depth0};
use crate::types::TemplateVariance;

use super::{SymbolKind, SymbolSpan};

// ─── Navigability filter ────────────────────────────────────────────────────

/// Non-navigable type names (scalars, pseudo-types, PHPStan utility types).
/// Types in this list are skipped when extracting docblock symbol spans.
const NON_NAVIGABLE: &[&str] = &[
    "int",
    "integer",
    "float",
    "double",
    "string",
    "bool",
    "boolean",
    "array",
    "object",
    "mixed",
    "void",
    "null",
    "true",
    "false",
    "never",
    "resource",
    "callable",
    "iterable",
    "static",
    "self",
    "parent",
    "class-string",
    "positive-int",
    "negative-int",
    "non-empty-string",
    "non-empty-array",
    "non-empty-list",
    "numeric-string",
    "numeric",
    "scalar",
    "list",
    "non-falsy-string",
    "literal-string",
    "callable-string",
    "array-key",
    "value-of",
    "key-of",
    "int-mask",
    "int-mask-of",
    "no-return",
    "empty",
    "number",
];

/// Returns `true` when a type name refers to a class/interface that the
/// user should be able to navigate to.
pub(crate) fn is_navigable_type(name: &str) -> bool {
    let base = name.split('<').next().unwrap_or(name);
    let base = base.split('{').next().unwrap_or(base);
    let lower = base.trim().to_ascii_lowercase();
    if lower.is_empty() {
        return false;
    }
    !NON_NAVIGABLE.contains(&lower.as_str())
}

// ─── Span construction helpers ──────────────────────────────────────────────

/// Construct a `ClassReference` `SymbolSpan` from a raw identifier string.
///
/// Detects whether the name is fully-qualified (leading `\`) and sets
/// `is_fqn` accordingly.  The leading `\` is stripped from the stored
/// `name` in all cases.
pub(super) fn class_ref_span(start: u32, end: u32, raw_name: &str) -> SymbolSpan {
    let is_fqn = raw_name.starts_with('\\');
    let name = raw_name.strip_prefix('\\').unwrap_or(raw_name).to_string();
    SymbolSpan {
        start,
        end,
        kind: SymbolKind::ClassReference { name, is_fqn },
    }
}

// ─── Docblock text retrieval ────────────────────────────────────────────────

/// Like [`crate::docblock::get_docblock_text_for_node`] but also returns
/// the byte offset of the `/**` opening within the file.
pub fn get_docblock_text_with_offset<'a>(
    trivia: &'a [Trivia<'a>],
    content: &str,
    node: &impl HasSpan,
) -> Option<(&'a str, u32)> {
    let node_start = node.span().start.offset;
    let candidate_idx = trivia.partition_point(|t| t.span.start.offset < node_start);
    if candidate_idx == 0 {
        return None;
    }

    let content_bytes = content.as_bytes();
    let mut covered_from = node_start;

    for i in (0..candidate_idx).rev() {
        let t = &trivia[i];
        let t_end = t.span.end.offset;

        let gap = content_bytes
            .get(t_end as usize..covered_from as usize)
            .unwrap_or(&[]);
        if !gap.iter().all(u8::is_ascii_whitespace) {
            return None;
        }

        match t.kind {
            TriviaKind::DocBlockComment => {
                return Some((t.value, t.span.start.offset));
            }
            TriviaKind::WhiteSpace
            | TriviaKind::SingleLineComment
            | TriviaKind::MultiLineComment
            | TriviaKind::HashComment => {
                covered_from = t.span.start.offset;
            }
        }
    }

    None
}

// ─── Docblock tag scanning ──────────────────────────────────────────────────

/// Scan a docblock for type references in supported tags and emit
/// `SymbolSpan` entries with file-level byte offsets.
///
/// Returns a list of `@template` parameter definitions found in the
/// docblock, each as `(name, byte_offset)`.
pub(super) fn extract_docblock_symbols(
    docblock: &str,
    base_offset: u32,
    spans: &mut Vec<SymbolSpan>,
) -> Vec<(String, u32, Option<String>, TemplateVariance)> {
    // Tags whose immediate next token is a type.
    const TYPE_FIRST_TAGS: &[&str] = &[
        "@param",
        "@return",
        "@throws",
        "@var",
        "@property",
        "@property-read",
        "@property-write",
        "@mixin",
        "@extends",
        "@implements",
        "@use",
        "@template-extends",
        "@template-implements",
        "@phpstan-return",
        "@phpstan-param",
        "@psalm-return",
        "@psalm-param",
        "@phpstan-var",
        "@psalm-var",
        "@phpstan-assert",
        "@phpstan-assert-if-true",
        "@phpstan-assert-if-false",
        "@psalm-assert",
        "@psalm-assert-if-true",
        "@psalm-assert-if-false",
    ];

    let mut line_start: usize = 0;
    let mut template_params: Vec<(String, u32, Option<String>, TemplateVariance)> = Vec::new();

    for line in docblock.split('\n') {
        if let Some(at_pos) = line.find('@') {
            let tag_start_in_line = at_pos;
            let after_at = &line[tag_start_in_line..];

            let tag_end = after_at
                .find(|c: char| c.is_whitespace())
                .unwrap_or(after_at.len());
            let tag = &after_at[..tag_end];
            let tag_lower = tag.to_ascii_lowercase();

            if tag_lower == "@method" {
                extract_method_tag_symbols(
                    line,
                    tag_start_in_line,
                    tag_end,
                    line_start,
                    base_offset,
                    spans,
                );
                line_start += line.len() + 1;
                continue;
            }

            // @template tags: `@template T of BoundType`
            // @template-covariant / @template-contravariant are variants.
            // The first token after the tag is the parameter name (skip it),
            // then if followed by `of`, the next token is the bound type.
            let template_variance = if tag_lower == "@template"
                || tag_lower == "@phpstan-template"
                || tag_lower == "@psalm-template"
            {
                Some(TemplateVariance::Invariant)
            } else if tag_lower == "@template-covariant"
                || tag_lower == "@phpstan-template-covariant"
                || tag_lower == "@psalm-template-covariant"
            {
                Some(TemplateVariance::Covariant)
            } else if tag_lower == "@template-contravariant"
                || tag_lower == "@phpstan-template-contravariant"
                || tag_lower == "@psalm-template-contravariant"
            {
                Some(TemplateVariance::Contravariant)
            } else {
                None
            };

            if let Some(variance) = template_variance {
                if let Some((name, offset, bound)) = extract_template_tag_symbols(
                    after_at,
                    tag_end,
                    tag_start_in_line,
                    line_start,
                    base_offset,
                    spans,
                ) {
                    template_params.push((name, offset, bound, variance));
                }
                line_start += line.len() + 1;
                continue;
            }

            let is_type_first = TYPE_FIRST_TAGS.iter().any(|t| tag_lower == *t);

            if is_type_first {
                let after_tag = &after_at[tag_end..];
                let after_tag_trimmed = after_tag.trim_start();
                if !after_tag_trimmed.is_empty() {
                    let type_start_in_line =
                        tag_start_in_line + tag_end + (after_tag.len() - after_tag_trimmed.len());
                    let type_start_in_docblock = line_start + type_start_in_line;

                    // Build a (possibly multiline) type string and its
                    // offset map so that `emit_type_spans` produces
                    // correct file-level offsets even when the type
                    // spans continuation lines.
                    let (joined, offset_map) =
                        join_multiline_type(docblock, type_start_in_docblock);

                    let (type_token, _remainder) = split_type_token(&joined);
                    if !type_token.is_empty() {
                        let mut local_spans: Vec<SymbolSpan> = Vec::new();
                        emit_type_spans(type_token, 0, &mut local_spans);
                        // Remap offsets from the joined string back to
                        // original docblock positions.
                        for mut sp in local_spans {
                            sp.start = base_offset
                                + offset_map
                                    .get(sp.start as usize)
                                    .copied()
                                    .unwrap_or(sp.start as usize)
                                    as u32;
                            sp.end = base_offset
                                + offset_map
                                    .get(sp.end as usize)
                                    .copied()
                                    .unwrap_or(sp.end as usize)
                                    as u32;
                            spans.push(sp);
                        }
                    }
                }
            }
        }

        line_start += line.len() + 1;
    }

    template_params
}

// ─── Type span emission ─────────────────────────────────────────────────────

/// Emit `SymbolSpan` entries for a type token, splitting unions and
/// intersections and skipping scalars.
/// Build a contiguous type string from a potentially multiline docblock
/// region, starting at `start_in_docblock` (byte offset within the
/// docblock text).
///
/// Returns `(joined_text, offset_map)` where `offset_map[i]` is the byte
/// offset in the original `docblock` that corresponds to byte `i` in
/// `joined_text`.  Continuation-line prefixes (`* `) are stripped so that
/// `split_type_token` / `emit_type_spans` see a clean type string.
fn join_multiline_type(docblock: &str, start_in_docblock: usize) -> (String, Vec<usize>) {
    let mut joined = String::new();
    // offset_map[i] = byte offset in `docblock` for byte `i` in `joined`.
    // We only add the one-past-end sentinel at the very end so that
    // continuation chunks don't shift indices.
    let mut offset_map: Vec<usize> = Vec::new();

    let first_line_rest = &docblock[start_in_docblock..];
    // Take text up to (but not including) the newline on the first line.
    let first_nl = first_line_rest.find('\n').unwrap_or(first_line_rest.len());
    let first_chunk = &first_line_rest[..first_nl];
    for (i, _) in first_chunk.char_indices() {
        offset_map.push(start_in_docblock + i);
    }
    joined.push_str(first_chunk);

    // Check whether the first chunk has unclosed `<`, `(`, or `{`.
    if !has_unclosed_delimiters(&joined) {
        // Push one-past-end sentinel.
        offset_map.push(start_in_docblock + first_chunk.len());
        return (joined, offset_map);
    }

    // Consume continuation lines.
    let mut pos = start_in_docblock + first_nl;
    while pos < docblock.len() {
        // Skip the `\n`.
        if docblock.as_bytes().get(pos) == Some(&b'\n') {
            pos += 1;
        }
        if pos >= docblock.len() {
            break;
        }

        let line_end = docblock[pos..]
            .find('\n')
            .map_or(docblock.len(), |p| pos + p);
        let raw_line = &docblock[pos..line_end];

        // Strip the leading `* ` (with optional whitespace before `*`).
        let stripped = raw_line.trim_start();
        if stripped.starts_with("*/") {
            // End of docblock.
            break;
        }
        let content_after_star = if let Some(rest) = stripped.strip_prefix('*') {
            // Skip one optional space after `*`.
            rest.strip_prefix(' ').unwrap_or(rest)
        } else {
            stripped
        };

        // If the continuation line starts with `@`, it's a new tag — stop.
        if content_after_star.trim_start().starts_with('@') {
            break;
        }

        let content_start_in_docblock = pos + (raw_line.len() - content_after_star.len());

        // Append a space to represent the line break in the joined string,
        // mapped to the newline position.
        offset_map.push(pos.saturating_sub(1));
        joined.push(' ');

        for (i, _) in content_after_star.char_indices() {
            offset_map.push(content_start_in_docblock + i);
        }
        joined.push_str(content_after_star);

        pos = line_end;

        if !has_unclosed_delimiters(&joined) {
            break;
        }
    }

    // One-past-end sentinel so that `sp.end` lookups work.
    let last_mapped = offset_map.last().copied().unwrap_or(start_in_docblock);
    offset_map.push(last_mapped + 1);

    (joined, offset_map)
}

/// Returns `true` when `s` has more opening `<`, `(`, or `{` than closing.
fn has_unclosed_delimiters(s: &str) -> bool {
    let mut angle = 0i32;
    let mut paren = 0i32;
    let mut brace = 0i32;
    for b in s.bytes() {
        match b {
            b'<' => angle += 1,
            b'>' => angle -= 1,
            b'(' => paren += 1,
            b')' => paren -= 1,
            b'{' => brace += 1,
            b'}' => brace -= 1,
            _ => {}
        }
    }
    angle > 0 || paren > 0 || brace > 0
}

pub(super) fn emit_type_spans(
    type_token: &str,
    token_file_offset: u32,
    spans: &mut Vec<SymbolSpan>,
) {
    // Split on union `|` at depth 0.
    let union_parts = split_union_depth0(type_token);
    if union_parts.len() > 1 {
        let mut offset = 0usize;
        for part in &union_parts {
            if let Some(pos) = type_token[offset..].find(part) {
                let part_offset = offset + pos;
                emit_type_spans(part.trim(), token_file_offset + part_offset as u32, spans);
                offset = part_offset + part.len();
            }
        }
        return;
    }

    // Split on intersection `&` at depth 0.
    let intersection_parts = split_intersection_depth0(type_token);
    if intersection_parts.len() > 1 {
        let mut offset = 0usize;
        for part in &intersection_parts {
            if let Some(pos) = type_token[offset..].find(part) {
                let part_offset = offset + pos;
                emit_type_spans(part.trim(), token_file_offset + part_offset as u32, spans);
                offset = part_offset + part.len();
            }
        }
        return;
    }

    // Handle PHPStan conditional return types:
    //   ($paramName is Type ? TrueType : FalseType)
    //   ($paramName is not Type ? TrueType : FalseType)
    if type_token.starts_with('(') && type_token.ends_with(')') {
        let inner = &type_token[1..type_token.len() - 1];
        // Look for ` is ` at depth 0 to identify a conditional type.
        if let Some(is_pos) = find_keyword_depth0(inner, " is ") {
            let after_is = &inner[is_pos + 4..];
            // Skip optional `not ` keyword.
            let (after_keyword, keyword_extra) = if let Some(rest) = after_is.strip_prefix("not ") {
                (rest, 4usize)
            } else {
                (after_is, 0usize)
            };
            // Find ` ? ` at depth 0 to separate condition type from true branch.
            if let Some(q_pos) = find_keyword_depth0(after_keyword, " ? ") {
                let condition_type = after_keyword[..q_pos].trim();
                let after_q = &after_keyword[q_pos + 3..];
                // Find ` : ` at depth 0 to separate true branch from false branch.
                if let Some(c_pos) = find_keyword_depth0(after_q, " : ") {
                    let true_type = after_q[..c_pos].trim();
                    let false_type = after_q[c_pos + 3..].trim();

                    // Byte offset of the condition type within the original token.
                    // token_file_offset points at `(`, +1 for `(`, +is_pos for `$param`,
                    // +4 for ` is `, +keyword_extra for optional `not `.
                    let cond_offset_in_inner = is_pos + 4 + keyword_extra;
                    let cond_leading =
                        after_keyword[..q_pos].len() - after_keyword[..q_pos].trim_start().len();
                    let cond_file_offset =
                        token_file_offset + 1 + (cond_offset_in_inner + cond_leading) as u32;
                    if !condition_type.is_empty() {
                        emit_type_spans(condition_type, cond_file_offset, spans);
                    }

                    // True type offset.
                    let true_region = &after_q[..c_pos];
                    let true_leading = true_region.len() - true_region.trim_start().len();
                    let true_offset_in_inner = cond_offset_in_inner + q_pos + 3;
                    let true_file_offset =
                        token_file_offset + 1 + (true_offset_in_inner + true_leading) as u32;
                    if !true_type.is_empty() {
                        emit_type_spans(true_type, true_file_offset, spans);
                    }

                    // False type offset.
                    let false_region = &after_q[c_pos + 3..];
                    let false_leading = false_region.len() - false_region.trim_start().len();
                    let false_offset_in_inner = true_offset_in_inner + c_pos + 3;
                    let false_file_offset =
                        token_file_offset + 1 + (false_offset_in_inner + false_leading) as u32;
                    if !false_type.is_empty() {
                        emit_type_spans(false_type, false_file_offset, spans);
                    }
                    return;
                }
            }
        }

        // Not a conditional type — this is a parenthesized group used for
        // grouping in union/intersection/DNF types, e.g. `(\Closure(static): mixed)`
        // or `(A&B)`.  Strip the outer parens and recurse into the inner content.
        emit_type_spans(inner, token_file_offset + 1, spans);
        return;
    }

    // Single type — strip nullable prefix.
    let (type_name, extra_offset) = if let Some(rest) = type_token.strip_prefix('?') {
        (rest, 1u32)
    } else {
        (type_token, 0u32)
    };

    if type_name.is_empty() {
        return;
    }

    // Handle `$this` as a self-reference (equivalent to `static`).
    if type_name == "$this" {
        let start = token_file_offset + extra_offset;
        let end = start + type_name.len() as u32;
        spans.push(SymbolSpan {
            start,
            end,
            kind: SymbolKind::SelfStaticParent {
                keyword: "static".to_string(),
            },
        });
        return;
    }

    // Handle `static`, `self`, and `parent` keywords in docblock types.
    // These are in NON_NAVIGABLE so they won't be emitted as ClassReference
    // spans, but they should still produce SelfStaticParent spans so that
    // hover works when they appear inside generic args (e.g. `Builder<static>`).
    if type_name == "static" || type_name == "self" || type_name == "parent" {
        let start = token_file_offset + extra_offset;
        let end = start + type_name.len() as u32;
        spans.push(SymbolSpan {
            start,
            end,
            kind: SymbolKind::SelfStaticParent {
                keyword: type_name.to_string(),
            },
        });
        return;
    }

    // Handle callable types: `Closure(ParamType): ReturnType`,
    // `callable(A, B): C`, `\Closure(): Pencil`, etc.
    // Detect by finding `(` at depth 0 (angle/brace) that is *not* at
    // position 0 (position-0 parens are the conditional-type case
    // handled above).
    if let Some(paren_pos) = find_callable_paren(type_name) {
        let base_name = &type_name[..paren_pos];

        // Emit span for the callable base name (e.g. `Closure`, `\Closure`).
        let base_trimmed = base_name
            .split('<')
            .next()
            .unwrap_or(base_name)
            .split('{')
            .next()
            .unwrap_or(base_name);
        let name_for_check = base_trimmed
            .strip_prefix('\\')
            .unwrap_or(base_trimmed)
            .trim();
        if is_navigable_type(name_for_check) {
            let is_fqn = base_trimmed.starts_with('\\');
            let name = base_trimmed
                .strip_prefix('\\')
                .unwrap_or(base_trimmed)
                .trim()
                .to_string();
            let start = token_file_offset + extra_offset;
            let end = start + base_trimmed.len() as u32;
            spans.push(SymbolSpan {
                start,
                end,
                kind: SymbolKind::ClassReference { name, is_fqn },
            });
        }

        // Find matching `)` respecting nesting.
        let inner_start = paren_pos + 1;
        let bytes = type_name.as_bytes();
        let mut depth = 1u32;
        let mut close_paren = inner_start;
        while close_paren < bytes.len() && depth > 0 {
            match bytes[close_paren] {
                b'(' => depth += 1,
                b')' => depth -= 1,
                _ => {}
            }
            if depth > 0 {
                close_paren += 1;
            }
        }

        if depth == 0 {
            // Recurse into parameter types inside `(...)`.
            let inner = &type_name[inner_start..close_paren];
            if !inner.trim().is_empty() {
                let mut d = 0u32;
                let mut arg_start_idx = 0usize;
                let inner_bytes = inner.as_bytes();
                for i in 0..=inner_bytes.len() {
                    let at_end = i == inner_bytes.len();
                    let is_comma = !at_end && inner_bytes[i] == b',' && d == 0;
                    if (at_end && d == 0) || is_comma {
                        let arg = &inner[arg_start_idx..i];
                        let trimmed = arg.trim();
                        if !trimmed.is_empty() {
                            let leading_ws = arg.len() - arg.trim_start().len();
                            let arg_file_offset = token_file_offset
                                + extra_offset
                                + (inner_start + arg_start_idx + leading_ws) as u32;
                            emit_type_spans(trimmed, arg_file_offset, spans);
                        }
                        arg_start_idx = i + 1;
                    } else if !at_end {
                        match inner_bytes[i] {
                            b'<' | b'(' | b'{' => d += 1,
                            b'>' | b')' | b'}' if d > 0 => d -= 1,
                            _ => {}
                        }
                    }
                }
            }

            // Recurse into the return type after `): `.
            let after_close = &type_name[close_paren + 1..];
            let after_trimmed = after_close.trim_start();
            if let Some(after_colon) = after_trimmed.strip_prefix(':') {
                let ret_trimmed = after_colon.trim_start();
                if !ret_trimmed.is_empty() {
                    let ret_offset_in_type = type_name.len() - ret_trimmed.len();
                    let ret_file_offset =
                        token_file_offset + extra_offset + ret_offset_in_type as u32;
                    emit_type_spans(ret_trimmed, ret_file_offset, spans);
                }
            }
        }

        return;
    }

    // Strip generic suffix and array suffix to get the base type name.
    let base = type_name.split('<').next().unwrap_or(type_name);
    let base = base.split('{').next().unwrap_or(base);
    let base = base.strip_suffix("[]").unwrap_or(base);

    let name_for_check = base.strip_prefix('\\').unwrap_or(base).trim();

    if is_navigable_type(name_for_check) {
        let is_fqn = base.starts_with('\\');
        let name = base.strip_prefix('\\').unwrap_or(base).trim().to_string();
        let start = token_file_offset + extra_offset;
        let end = start + base.len() as u32;

        spans.push(SymbolSpan {
            start,
            end,
            kind: SymbolKind::ClassReference { name, is_fqn },
        });
    }

    // Recurse into generic type arguments: `Foo<Bar, Baz>` → process `Bar, Baz`.
    // Also recurse into array/object shape bodies: `array{key: Cls, other: int}`.
    let brace_start = type_name.find('{');
    if let Some(gen_start) = type_name.find('<') {
        // Find the matching closing `>` (respecting nesting depth).
        let inner_start = gen_start + 1;
        let bytes = type_name.as_bytes();
        let mut depth = 1u32;
        let mut gen_end = inner_start;
        while gen_end < bytes.len() && depth > 0 {
            match bytes[gen_end] {
                b'<' => depth += 1,
                b'>' => depth -= 1,
                _ => {}
            }
            if depth > 0 {
                gen_end += 1;
            }
        }
        if depth == 0 {
            let inner = &type_name[inner_start..gen_end];
            // Split on `,` at depth 0 to get individual type arguments.
            let mut d = 0u32;
            let mut arg_start_idx = 0usize;
            let inner_bytes = inner.as_bytes();
            for i in 0..=inner_bytes.len() {
                let at_end = i == inner_bytes.len();
                let is_comma = !at_end && inner_bytes[i] == b',' && d == 0;
                if at_end && d == 0 || is_comma {
                    let arg = &inner[arg_start_idx..i];
                    let trimmed = arg.trim();
                    if !trimmed.is_empty() {
                        // Compute the offset of the trimmed arg within inner.
                        let leading_ws = arg.len() - arg.trim_start().len();
                        let arg_file_offset = token_file_offset
                            + extra_offset
                            + (inner_start + arg_start_idx + leading_ws) as u32;
                        emit_type_spans(trimmed, arg_file_offset, spans);
                    }
                    arg_start_idx = i + 1;
                } else if !at_end {
                    match inner_bytes[i] {
                        b'<' | b'(' | b'{' => d += 1,
                        b'>' | b')' | b'}' if d > 0 => d -= 1,
                        _ => {}
                    }
                }
            }
        }
    }

    // Recurse into array/object shape bodies: `array{key: Pen, debug: bool}`
    // or `object{name: string, user: User}`.
    // Each entry has the form `key: Type` or `key?: Type`.  We split on
    // `,` at depth 0, then for each entry skip past the `:` to get the
    // value type and recurse into it.
    if let Some(brace_pos) = brace_start {
        let inner_start = brace_pos + 1;
        let bytes = type_name.as_bytes();
        let mut depth = 1u32;
        let mut brace_end = inner_start;
        while brace_end < bytes.len() && depth > 0 {
            match bytes[brace_end] {
                b'{' => depth += 1,
                b'}' => depth -= 1,
                _ => {}
            }
            if depth > 0 {
                brace_end += 1;
            }
        }
        if depth == 0 {
            let inner = &type_name[inner_start..brace_end];
            // Split entries on `,` at depth 0.
            let mut d = 0u32;
            let mut entry_start = 0usize;
            let inner_bytes = inner.as_bytes();
            for i in 0..=inner_bytes.len() {
                let at_end = i == inner_bytes.len();
                let is_comma = !at_end && inner_bytes[i] == b',' && d == 0;
                if (at_end && d == 0) || is_comma {
                    let entry = &inner[entry_start..i];
                    // Find the `:` separator (at depth 0) between key and value type.
                    // The key may contain `?` (optional marker) but not `<`, `{`, etc.
                    let mut colon_pos = None;
                    let mut ed = 0u32;
                    for (j, &b) in entry.as_bytes().iter().enumerate() {
                        match b {
                            b'<' | b'(' | b'{' => ed += 1,
                            b'>' | b')' | b'}' if ed > 0 => ed -= 1,
                            b':' if ed == 0 => {
                                colon_pos = Some(j);
                                break;
                            }
                            _ => {}
                        }
                    }
                    if let Some(cp) = colon_pos {
                        let value_part = &entry[cp + 1..];
                        let value_trimmed = value_part.trim();
                        if !value_trimmed.is_empty() {
                            let leading_ws = value_part.len() - value_part.trim_start().len();
                            let value_file_offset = token_file_offset
                                + extra_offset
                                + (inner_start + entry_start + cp + 1 + leading_ws) as u32;
                            emit_type_spans(value_trimmed, value_file_offset, spans);
                        }
                    }
                    entry_start = i + 1;
                } else if !at_end {
                    match inner_bytes[i] {
                        b'<' | b'(' | b'{' => d += 1,
                        b'>' | b')' | b'}' if d > 0 => d -= 1,
                        _ => {}
                    }
                }
            }
        }
    }
}

// ─── Callable / keyword helpers ─────────────────────────────────────────────

/// Find the byte position of a `(` that starts a callable parameter list
/// within a type string.  Returns `None` when there is no `(` at
/// angle-bracket / brace depth 0 or when `(` is at position 0 (which
/// indicates a conditional / grouped type, not a callable).
fn find_callable_paren(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut depth_angle = 0i32;
    let mut depth_brace = 0i32;
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'<' => depth_angle += 1,
            b'>' if depth_angle > 0 => depth_angle -= 1,
            b'{' => depth_brace += 1,
            b'}' if depth_brace > 0 => depth_brace -= 1,
            b'(' if depth_angle == 0 && depth_brace == 0 && i > 0 => return Some(i),
            _ => {}
        }
    }
    None
}

/// Find the byte position of `keyword` (e.g. `" is "`, `" ? "`, `" : "`)
/// within `s` at parenthesis/angle-bracket depth 0.  Returns `None` when
/// the keyword only appears inside nested delimiters.
fn find_keyword_depth0(s: &str, keyword: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let kw_bytes = keyword.as_bytes();
    let kw_len = kw_bytes.len();
    if bytes.len() < kw_len {
        return None;
    }
    let mut depth = 0i32;
    for i in 0..=bytes.len() - kw_len {
        match bytes[i] {
            b'<' | b'(' | b'{' => depth += 1,
            b'>' | b')' | b'}' => {
                if depth > 0 {
                    depth -= 1;
                }
            }
            _ => {}
        }
        if depth == 0 && &bytes[i..i + kw_len] == kw_bytes {
            return Some(i);
        }
    }
    None
}

// ─── @template tag extraction ───────────────────────────────────────────────

/// Handle `@template` (and variants) tags which have the form:
/// `@template T of BoundType`
///
/// The first token after the tag is the template parameter name — its
/// `(name, byte_offset)` pair is returned so the caller can record a
/// [`super::TemplateParamDef`].  If followed by the keyword `of`, the next
/// token is the bound type which is emitted as a `ClassReference`.
fn extract_template_tag_symbols(
    after_at: &str,
    tag_end: usize,
    tag_start_in_line: usize,
    line_start: usize,
    base_offset: u32,
    spans: &mut Vec<SymbolSpan>,
) -> Option<(String, u32, Option<String>)> {
    // Skip the tag itself to get to the template parameter name.
    let after_tag = &after_at[tag_end..];
    let after_tag_trimmed = after_tag.trim_start();
    if after_tag_trimmed.is_empty() {
        return None;
    }

    // The first non-whitespace token is the parameter name (e.g. `T`, `TNode`).
    let param_end = after_tag_trimmed
        .find(|c: char| c.is_whitespace())
        .unwrap_or(after_tag_trimmed.len());

    let param_name = &after_tag_trimmed[..param_end];
    // Compute the byte offset of the param name within the file.
    let param_offset_in_after_at = after_at.len() - after_tag_trimmed.len();
    let param_file_offset =
        base_offset + (line_start + tag_start_in_line + param_offset_in_after_at) as u32;

    let after_param = &after_tag_trimmed[param_end..];
    let after_param_trimmed = after_param.trim_start();

    // Check for `of` keyword.
    if !after_param_trimmed.starts_with("of ") && !after_param_trimmed.starts_with("of\t") {
        return Some((param_name.to_string(), param_file_offset, None));
    }

    // Skip `of` and whitespace to get to the bound type.
    let after_of = &after_param_trimmed[2..]; // skip "of"
    let after_of_trimmed = after_of.trim_start();
    if after_of_trimmed.is_empty() {
        return Some((param_name.to_string(), param_file_offset, None));
    }

    // Compute the offset of the bound type within the original line.
    // after_at starts at tag_start_in_line within the line.
    // after_of_trimmed starts at:
    //   tag_start_in_line + tag_end + (whitespace before param)
    //   + param_end + (whitespace before "of") + 2 + (whitespace after "of")
    let bound_offset_in_after_at = after_at.len() - after_of_trimmed.len();
    let bound_start_in_line = tag_start_in_line + bound_offset_in_after_at;

    let (type_token, _remainder) = split_type_token(after_of_trimmed);
    let bound = if !type_token.is_empty() {
        emit_type_spans(
            type_token,
            base_offset + (line_start + bound_start_in_line) as u32,
            spans,
        );
        Some(type_token.to_string())
    } else {
        None
    };

    Some((param_name.to_string(), param_file_offset, bound))
}

// ─── @method tag extraction ─────────────────────────────────────────────────

/// Handle `@method` tags which have the form:
/// `@method [static] ReturnType methodName(ParamType $p, ...)`
fn extract_method_tag_symbols(
    line: &str,
    tag_start_in_line: usize,
    tag_end_in_tag: usize,
    line_start: usize,
    base_offset: u32,
    spans: &mut Vec<SymbolSpan>,
) {
    let after_tag = &line[tag_start_in_line + tag_end_in_tag..];
    let after_tag_trimmed = after_tag.trim_start();
    if after_tag_trimmed.is_empty() {
        return;
    }

    let mut rest = after_tag_trimmed;
    let mut rest_offset =
        tag_start_in_line + tag_end_in_tag + (after_tag.len() - after_tag_trimmed.len());

    // Skip optional `static` keyword.
    if rest.starts_with("static ") || rest.starts_with("static\t") {
        let skip = "static".len();
        let after_static = rest[skip..].trim_start();
        let whitespace_len = rest.len() - skip - after_static.len();
        rest_offset += skip + whitespace_len;
        rest = after_static;
    }

    if rest.is_empty() {
        return;
    }

    // Extract return type.
    let (return_type, remainder) = split_type_token(rest);
    if !return_type.is_empty() {
        emit_type_spans(
            return_type,
            base_offset + (line_start + rest_offset) as u32,
            spans,
        );
    }

    // After the return type, find the `(` for parameter list.
    if let Some(paren_pos) = remainder.find('(') {
        let close = remainder[paren_pos..].find(')');
        if let Some(close_pos) = close {
            let inner = &remainder[paren_pos + 1..paren_pos + close_pos];
            let inner_offset_in_line = rest_offset
                + return_type.len()
                + (remainder.len() - rest[return_type.len()..].len())
                + paren_pos
                + 1;

            // Simple comma-split at depth 0 for parameters.
            let mut depth = 0i32;
            let mut param_start = 0usize;

            for (i, ch) in inner.char_indices() {
                match ch {
                    '<' | '(' | '{' => depth += 1,
                    '>' | ')' | '}' => depth -= 1,
                    ',' if depth == 0 => {
                        let param = inner[param_start..i].trim();
                        emit_method_param_type(
                            param,
                            line_start,
                            inner_offset_in_line,
                            param_start,
                            base_offset,
                            spans,
                        );
                        param_start = i + 1;
                    }
                    _ => {}
                }
            }
            // Last parameter.
            let param = inner[param_start..].trim();
            emit_method_param_type(
                param,
                line_start,
                inner_offset_in_line,
                param_start,
                base_offset,
                spans,
            );
        }
    }
}

/// Emit a type span for a single parameter in a `@method` tag's parameter list.
fn emit_method_param_type(
    param: &str,
    line_start: usize,
    inner_offset_in_line: usize,
    param_start_in_inner: usize,
    base_offset: u32,
    spans: &mut Vec<SymbolSpan>,
) {
    if param.is_empty() {
        return;
    }
    // A parameter looks like `TypeHint $varName` or just `$varName`.
    if let Some(dollar_pos) = param.find('$') {
        let type_part = param[..dollar_pos].trim();
        if !type_part.is_empty() {
            let type_start_in_param = param.find(type_part).unwrap_or(0);
            let (type_token, _) = split_type_token(type_part);
            if !type_token.is_empty() {
                let file_offset = base_offset
                    + (line_start
                        + inner_offset_in_line
                        + param_start_in_inner
                        + type_start_in_param) as u32;
                emit_type_spans(type_token, file_offset, spans);
            }
        }
    }
}
