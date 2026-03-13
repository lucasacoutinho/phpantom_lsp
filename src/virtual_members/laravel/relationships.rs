//! Eloquent relationship classification, property type synthesis, and
//! body-text inference.
//!
//! This module handles the mapping from Eloquent relationship method
//! return types (e.g. `HasMany<Post, $this>`) to virtual property types
//! (e.g. `Illuminate\Database\Eloquent\Collection<Post>`), as well as
//! inferring relationship types from method body text when no `@return`
//! annotation is present.

use crate::docblock::types::parse_generic_args;
use crate::types::{ClassInfo, ELOQUENT_COLLECTION_FQN};
use crate::util::short_name;

use super::helpers::{camel_to_snake, snake_to_camel};

/// Fully-qualified relationship class names used by
/// [`infer_relationship_from_body`].
const RELATIONSHIP_METHOD_FQN_MAP: &[(&str, &str)] = &[
    (
        "hasOne",
        "Illuminate\\Database\\Eloquent\\Relations\\HasOne",
    ),
    (
        "hasMany",
        "Illuminate\\Database\\Eloquent\\Relations\\HasMany",
    ),
    (
        "belongsTo",
        "Illuminate\\Database\\Eloquent\\Relations\\BelongsTo",
    ),
    (
        "belongsToMany",
        "Illuminate\\Database\\Eloquent\\Relations\\BelongsToMany",
    ),
    (
        "morphOne",
        "Illuminate\\Database\\Eloquent\\Relations\\MorphOne",
    ),
    (
        "morphMany",
        "Illuminate\\Database\\Eloquent\\Relations\\MorphMany",
    ),
    (
        "morphTo",
        "Illuminate\\Database\\Eloquent\\Relations\\MorphTo",
    ),
    (
        "morphToMany",
        "Illuminate\\Database\\Eloquent\\Relations\\MorphToMany",
    ),
    (
        "hasManyThrough",
        "Illuminate\\Database\\Eloquent\\Relations\\HasManyThrough",
    ),
    (
        "hasOneThrough",
        "Illuminate\\Database\\Eloquent\\Relations\\HasOneThrough",
    ),
];

/// Known Eloquent relationship class short names that yield a single
/// (nullable) related model instance when accessed as a property.
const SINGULAR_RELATIONSHIPS: &[&str] = &["HasOne", "MorphOne", "BelongsTo", "HasOneThrough"];

/// Known Eloquent relationship class short names that yield a
/// `Collection<TRelated>` when accessed as a property.
const COLLECTION_RELATIONSHIPS: &[&str] = &[
    "HasMany",
    "MorphMany",
    "BelongsToMany",
    "HasManyThrough",
    "MorphToMany",
];

/// The `MorphTo` relationship resolves to the generic `Model` base class
/// because the concrete related type is determined at runtime.
const MORPH_TO: &str = "MorphTo";

/// The FQN namespace prefix for Eloquent relationship classes.
///
/// When a return type is fully-qualified, we verify it lives under this
/// namespace before classifying it as a relationship.  This prevents
/// false positives from user classes that happen to share short names
/// with Eloquent relationships (e.g. `App\Relations\HasMany`).
const ELOQUENT_RELATIONS_NS: &str = "Illuminate\\Database\\Eloquent\\Relations\\";

/// The category of a relationship return type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RelationshipKind {
    /// HasOne, MorphOne, BelongsTo — singular nullable model.
    Singular,
    /// HasMany, MorphMany, BelongsToMany, HasManyThrough, MorphToMany — Collection.
    Collection,
    /// MorphTo — generic Model.
    MorphTo,
}

/// Try to classify a return type string as a known Eloquent relationship.
///
/// Accepts both short names (`HasMany`) and fully-qualified names
/// (`\Illuminate\Database\Eloquent\Relations\HasMany`).  Generic
/// parameters are stripped before matching.
///
/// When the base type is namespace-qualified (contains `\`), the
/// function verifies that it lives under
/// `Illuminate\Database\Eloquent\Relations\` before classifying.
/// This prevents false positives from user classes whose short name
/// collides with an Eloquent relationship class (e.g. a custom
/// `App\Relations\HasMany` that does not extend Eloquent's).
///
/// Unqualified names (no `\`) are matched by short name only, which
/// is the common case for body-inferred types and docblock annotations
/// that use `use` imports.
pub(super) fn classify_relationship(return_type: &str) -> Option<RelationshipKind> {
    let (base, _) = parse_generic_args(return_type);
    let sname = short_name(base);

    // When the base type is qualified (contains `\`), verify it belongs
    // to the Eloquent Relations namespace.
    if base.contains('\\') && !base.starts_with(ELOQUENT_RELATIONS_NS) {
        return None;
    }

    if SINGULAR_RELATIONSHIPS.contains(&sname) {
        return Some(RelationshipKind::Singular);
    }
    if COLLECTION_RELATIONSHIPS.contains(&sname) {
        return Some(RelationshipKind::Collection);
    }
    if sname == MORPH_TO {
        return Some(RelationshipKind::MorphTo);
    }

    None
}

/// Extract the `TRelated` type from a relationship return type's
/// generic parameters.
///
/// Given `"HasMany<Post, $this>"`, returns `Some("Post")`.
/// Given `"HasOne<\\App\\Models\\Post, $this>"`, returns
/// `Some("\\App\\Models\\Post")`.
///
/// Returns `None` if no generic parameters are present.
pub(super) fn extract_related_type(return_type: &str) -> Option<String> {
    let (_, args) = parse_generic_args(return_type);
    let first = args.first()?;
    let trimmed = first.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.to_string())
}

/// Build the property type string for a relationship.
///
/// - Singular relationships → the related type as-is (nullable).
/// - Collection relationships → the custom collection class (if set) or
///   `Illuminate\Database\Eloquent\Collection`, parameterised with `<TRelated>`.
/// - MorphTo → `Illuminate\Database\Eloquent\Model`.
pub(super) fn build_property_type(
    kind: RelationshipKind,
    related_type: Option<&str>,
    custom_collection: Option<&str>,
) -> Option<String> {
    match kind {
        RelationshipKind::Singular => related_type.map(|t| t.to_string()),
        RelationshipKind::Collection => {
            let inner = related_type.unwrap_or("Illuminate\\Database\\Eloquent\\Model");
            let collection_class = custom_collection
                .map(|c| c.to_string())
                .unwrap_or_else(|| ELOQUENT_COLLECTION_FQN.to_string());
            Some(format!("{collection_class}<{inner}>"))
        }
        RelationshipKind::MorphTo => Some("Illuminate\\Database\\Eloquent\\Model".to_string()),
    }
}

/// Map a `*_count` virtual property name back to the relationship method
/// name that produced it.
///
/// Returns `Some(method_name)` when `property_name` ends with `_count`
/// and the stripped/camelCased remainder is a relationship method on
/// `class`.  Go-to-definition uses this so that clicking on
/// `posts_count` jumps to the `posts()` method, and
/// `master_recipe_count` jumps to `masterRecipe()`.
pub(crate) fn count_property_to_relationship_method(
    class: &ClassInfo,
    property_name: &str,
) -> Option<String> {
    let base = property_name.strip_suffix("_count")?;
    if base.is_empty() {
        return None;
    }
    let method_name = snake_to_camel(base);
    let method = class.methods.iter().find(|m| m.name == method_name)?;
    let return_type = method.return_type.as_deref()?;
    if classify_relationship(return_type).is_some() {
        Some(method_name)
    } else {
        None
    }
}

/// Infer a relationship return type from a method's body text.
///
/// When a relationship method has no `@return` annotation, this function
/// scans the body for patterns like `$this->hasMany(Post::class)` and
/// synthesizes a fully-qualified return type string (e.g.
/// `Illuminate\Database\Eloquent\Relations\HasMany<Post>`).
///
/// Supports all standard Eloquent relationship builder methods:
/// `hasOne`, `hasMany`, `belongsTo`, `belongsToMany`, `morphOne`,
/// `morphMany`, `morphTo`, `morphToMany`, `hasManyThrough`, and
/// `hasOneThrough`.
///
/// Returns `None` if no recognisable pattern is found.
pub fn infer_relationship_from_body(body_text: &str) -> Option<String> {
    for &(method_name, fqn) in RELATIONSHIP_METHOD_FQN_MAP {
        // Look for `$this->methodName(` in the body text.
        let needle = format!("$this->{method_name}(");
        let Some(call_pos) = body_text.find(&needle) else {
            continue;
        };

        // `morphTo` never carries a related-model generic parameter;
        // the concrete type is determined at runtime.
        //
        // The FQN is prefixed with `\` so that `resolve_type_string`
        // in `ast_update.rs` recognises it as already-qualified and
        // does not prepend the declaring file's namespace.
        // `resolve_name` will strip the leading `\` back to canonical
        // form during the resolution pass.
        if method_name == "morphTo" {
            return Some(format!("\\{fqn}"));
        }

        // Extract the first argument from the call.  We look for
        // `SomeName::class` as the first positional argument.
        let args_start = call_pos + needle.len();
        let after_paren = &body_text[args_start..];

        if let Some(class_arg) = extract_class_argument(after_paren) {
            return Some(format!("\\{fqn}<{class_arg}>"));
        }

        // No `::class` argument found — return the bare relationship
        // name without generics.  The provider will handle it the same
        // way it handles annotated relationships without generics.
        return Some(format!("\\{fqn}"));
    }

    None
}

/// Extract a class name from the first `X::class` argument in a
/// parenthesised argument list.
///
/// Given the text after the opening `(`, e.g. `Post::class, 'user_id')`,
/// returns `Some("Post")`.  Also handles fully-qualified names like
/// `\App\Models\Post::class` and `self::class` / `static::class`.
///
/// Returns `None` if no `::class` token is found before the closing `)`.
fn extract_class_argument(after_paren: &str) -> Option<String> {
    // Find the closing paren to bound our search.
    let end = after_paren.find(')')?;
    let args_region = &after_paren[..end];

    // Isolate the first argument (before the first comma) and look for
    // `X::class` within it.
    let first_arg = args_region.split(',').next().unwrap_or(args_region);
    let class_pos = first_arg.find("::class")?;
    let before = first_arg[..class_pos].trim();

    if before.is_empty() {
        return None;
    }

    // Strip leading backslash for FQNs and extract the short name.
    let name = before.strip_prefix('\\').unwrap_or(before);
    let short_name = short_name(name);

    if short_name.is_empty() {
        return None;
    }

    Some(short_name.to_string())
}

/// Build a `{snake_name}_count` property name for a relationship method.
///
/// Used by the provider to synthesize `*_count` properties for each
/// relationship.
pub(super) fn count_property_name(method_name: &str) -> String {
    format!("{}_count", camel_to_snake(method_name))
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "relationships_tests.rs"]
mod tests;
