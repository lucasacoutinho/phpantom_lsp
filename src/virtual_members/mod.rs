//! Virtual member provider abstraction.
//!
//! Virtual members are methods and properties that do not exist as real
//! PHP declarations but are surfaced by magic methods (`__call`, `__get`,
//! `__set`, etc.) or framework conventions.  Three providers produce
//! virtual members today:
//!
//! 1. **Laravel model provider** — synthesizes members from
//!    framework-specific patterns (relationship properties, scope methods,
//!    Builder-as-static forwarding, convention-based `factory()` method).
//! 2. **Laravel factory provider** — synthesizes `create()` and `make()`
//!    methods on factory classes that return the corresponding model type,
//!    using the naming convention when no `@extends Factory<Model>`
//!    annotation is present.
//! 3. **PHPDoc provider** (`@method`, `@property`, `@property-read`,
//!    `@property-write`, `@mixin`) — documents magic members on a class.
//!    Within this provider, explicit `@method` / `@property` tags take
//!    precedence over members inherited from `@mixin` classes.
//!
//! All are unified behind the [`VirtualMemberProvider`] trait.
//! Providers are queried in priority order after base resolution
//! (own members + traits + parent chain) is complete.  A member
//! contributed by a higher-priority provider is never overwritten by a
//! lower-priority one, and all virtual members lose to real declared
//! members.
//!
//! # Caching
//!
//! [`resolve_class_fully`] is called from many code paths (completion,
//! hover, go-to-definition, call resolution, etc.) and often for the
//! same class within a single request.  The full resolution (inheritance
//! walk + virtual member providers + interface merging) is expensive, so
//! [`resolve_class_fully_cached`] accepts a [`ResolvedClassCache`] that
//! stores results keyed by fully-qualified class name.  The cache is
//! stored on `Backend` and cleared whenever a file is re-parsed
//! (`update_ast` / `parse_and_cache_content`), so stale entries never
//! survive an edit.
//!
//! # Precedence model
//!
//! ```text
//! 1. Real declared members (in PHP source code)
//! 2. Trait members (real implementations)
//! 3. Parent chain members (real implementations)
//! 4. Virtual member providers (in priority order):
//!    a. Laravel model provider  — richest type info
//!    b. Laravel factory provider — convention-based factory methods
//!    c. PHPDoc provider          — @method, @property, @mixin
//! ```

pub mod laravel;
pub mod phpdoc;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::inheritance::resolve_class_with_inheritance;
use crate::types::{ClassInfo, ConstantInfo, MethodInfo, PropertyInfo};

/// Cache key for [`ResolvedClassCache`]: fully-qualified class name
/// paired with the concrete generic type arguments used at this
/// instantiation site.
///
/// For non-generic classes the argument list is empty, keeping the
/// common case cheap.  For generic classes like
/// `Illuminate\Database\Eloquent\Builder<App\Models\User>`, the key
/// would be `("Illuminate\\Database\\Eloquent\\Builder", vec!["App\\Models\\User"])`.
///
/// Generic args are stored normalized (fully qualified, sorted when
/// order-independent) to avoid near-miss cache entries.
pub type ResolvedClassCacheKey = (String, Vec<String>);

/// Thread-safe cache of fully-resolved classes, keyed by FQN + generic args.
///
/// Stored on [`Backend`](crate::Backend) and selectively invalidated
/// when a file is re-parsed (`update_ast` / `parse_and_cache_content`).
/// Within a single request cycle (completion, hover, etc.) the cache
/// eliminates redundant calls to [`resolve_class_fully`] for the same
/// class at the same generic instantiation.
pub type ResolvedClassCache = Arc<Mutex<HashMap<ResolvedClassCacheKey, ClassInfo>>>;

/// Create a new, empty [`ResolvedClassCache`].
pub fn new_resolved_class_cache() -> ResolvedClassCache {
    Arc::new(Mutex::new(HashMap::new()))
}

/// Evict all cache entries whose FQN matches the given name.
///
/// Because the cache is keyed by `(FQN, generic_args)`, a single FQN
/// may have multiple entries (one per distinct generic instantiation).
/// This helper removes all of them, which is used during targeted
/// cache invalidation when a class definition changes.
pub fn evict_fqn(cache: &mut HashMap<ResolvedClassCacheKey, ClassInfo>, fqn: &str) {
    cache.retain(|(k, _), _| k != fqn);
}

/// Members synthesized by a provider.
///
/// Merged below real declared members, traits, and the parent chain.
/// Each provider returns a `VirtualMembers` value from its
/// [`provide`](VirtualMemberProvider::provide) method.
pub struct VirtualMembers {
    /// Virtual methods to add to the class.
    pub methods: Vec<MethodInfo>,
    /// Virtual properties to add to the class.
    pub properties: Vec<PropertyInfo>,
    /// Virtual constants to add to the class.
    pub constants: Vec<ConstantInfo>,
}

impl VirtualMembers {
    /// Whether this value contains no methods, properties, or constants.
    pub fn is_empty(&self) -> bool {
        self.methods.is_empty() && self.properties.is_empty() && self.constants.is_empty()
    }
}

/// A provider that contributes virtual members to a class.
///
/// Receives the class with traits and parents already merged (via
/// [`resolve_class_with_inheritance`](crate::inheritance::resolve_class_with_inheritance)),
/// but **without** other providers' contributions.  This prevents
/// circular loading when one provider's output would trigger another
/// provider.
///
/// Implementations must be cheap to construct and stateless.  All
/// contextual information is passed through the `class` and
/// `class_loader` arguments.
pub trait VirtualMemberProvider {
    /// Whether this provider has anything to say about this class.
    ///
    /// This is a cheap pre-check so the resolver can skip providers
    /// early without calling [`provide`](Self::provide).  Returning
    /// `false` means [`provide`](Self::provide) will not be called.
    fn applies_to(
        &self,
        class: &ClassInfo,
        class_loader: &dyn Fn(&str) -> Option<ClassInfo>,
    ) -> bool;

    /// Produce virtual members for this class.
    ///
    /// Only called when [`applies_to`](Self::applies_to) returned `true`.
    /// The returned members are merged into the class below all real
    /// declared members (own, trait, and parent chain).
    ///
    /// `cache` is the shared resolved-class cache.  Providers that need
    /// to fully resolve helper classes (e.g. the Laravel model provider
    /// resolving the Eloquent Builder) should use
    /// [`resolve_class_fully_cached`] via this cache to avoid redundant
    /// work across requests.
    fn provide(
        &self,
        class: &ClassInfo,
        class_loader: &dyn Fn(&str) -> Option<ClassInfo>,
        cache: Option<&ResolvedClassCache>,
    ) -> VirtualMembers;
}

/// Merge virtual members into a resolved `ClassInfo`.
///
/// For each method in `virtual.methods`, adds it to `class.methods` only
/// if no method with the same name and same staticness already exists.
/// This allows a provider to contribute both a static and an instance
/// variant of the same method (e.g. Laravel scope methods that are
/// accessible via both `User::active()` and `$user->active()`).
///
/// **Exception:** when the existing method has `has_scope_attribute: true`,
/// the virtual method **replaces** it.  `#[Scope]`-attributed methods
/// share their name with the synthesized scope method, but the original
/// is a `protected` implementation detail that should not appear in
/// completion results.  The virtual replacement is `public` with the
/// first `$query` parameter stripped, which is what callers actually see.
///
/// Properties and constants are deduplicated by name only.
///
/// This ensures that real declared members (and contributions from
/// higher-priority providers that were merged earlier) are never
/// overwritten.
pub fn merge_virtual_members(class: &mut ClassInfo, virtual_members: VirtualMembers) {
    for method in virtual_members.methods {
        let existing = class
            .methods
            .iter()
            .position(|m| m.name == method.name && m.is_static == method.is_static);
        match existing {
            Some(idx) if class.methods[idx].has_scope_attribute => {
                // Replace the #[Scope]-attributed original with the
                // synthesized virtual scope method.
                class.methods[idx] = method;
            }
            Some(_) => {
                // Real declared member — keep the original.
            }
            None => {
                class.methods.push(method);
            }
        }
    }
    for property in virtual_members.properties {
        if !class.properties.iter().any(|p| p.name == property.name) {
            class.properties.push(property);
        }
    }
    for constant in virtual_members.constants {
        if !class.constants.iter().any(|c| c.name == constant.name) {
            class.constants.push(constant);
        }
    }
}

/// Apply all registered providers to a base-resolved class.
///
/// Iterates over `providers` in order (highest priority first) and
/// merges each provider's virtual members into `class`.  Because
/// [`merge_virtual_members`] skips members that already exist,
/// higher-priority providers' contributions shadow lower-priority ones.
pub fn apply_virtual_members(
    class: &mut ClassInfo,
    class_loader: &dyn Fn(&str) -> Option<ClassInfo>,
    providers: &[Box<dyn VirtualMemberProvider>],
    cache: Option<&ResolvedClassCache>,
) {
    for provider in providers {
        if provider.applies_to(class, class_loader) {
            let virtual_members = provider.provide(class, class_loader, cache);
            if !virtual_members.is_empty() {
                merge_virtual_members(class, virtual_members);
            }
        }
    }
}

/// Return the default set of virtual member providers in priority order.
///
/// Providers are queried in order; a member contributed by an earlier
/// provider is never overwritten by a later one.
///
/// 1. Laravel model provider (highest priority — richest type info)
/// 2. Laravel factory provider (convention-based create/make methods)
/// 3. PHPDoc provider (`@method` / `@property` / `@mixin` tags)
pub fn default_providers() -> Vec<Box<dyn VirtualMemberProvider>> {
    vec![
        // Laravel model provider — relationship properties, scopes, Builder
        // forwarding, convention-based factory() method.
        Box::new(laravel::LaravelModelProvider),
        // Laravel factory provider — convention-based create()/make() methods
        // for factory classes extending Illuminate\Database\Eloquent\Factories\Factory.
        Box::new(laravel::LaravelFactoryProvider),
        // PHPDoc provider — @method / @property / @mixin tags.
        Box::new(phpdoc::PHPDocProvider),
    ]
}

// ─── Full class resolution ──────────────────────────────────────────────────

/// Resolve a class with full inheritance and virtual member providers.
///
/// This is the primary entry point for completion, go-to-definition,
/// and any other feature that needs the complete set of members
/// visible on a class instance or static access.
///
/// The resolution proceeds in two phases:
///
/// 1. **Base resolution** via
///    [`resolve_class_with_inheritance`]: merges own members, trait
///    members, and parent chain members, applying generic type
///    substitution along the way.
///
/// 2. **Virtual member providers**: queries each registered provider
///    in priority order and merges their contributions.  Virtual
///    members never overwrite real declared members or contributions
///    from higher-priority providers.
///
/// Code that needs only the base resolution (e.g. providers
/// themselves, to avoid circular loading) should call
/// [`resolve_class_with_inheritance`] directly.
pub fn resolve_class_fully(
    class: &ClassInfo,
    class_loader: &dyn Fn(&str) -> Option<ClassInfo>,
) -> ClassInfo {
    resolve_class_fully_inner(class, class_loader, None, &[])
}

/// Cached variant of [`resolve_class_fully`].
///
/// Identical semantics, but stores and retrieves results from `cache`
/// so that repeated resolutions of the same class within a single
/// request cycle (or across requests between edits) are free.
///
/// The cache is keyed by the class's fully-qualified name
/// (`namespace\ClassName` or just `ClassName` for the global namespace).
/// Callers that apply post-resolution transforms (e.g.
/// [`apply_generic_args`](crate::inheritance::apply_generic_args)) should
/// still call this function for the base resolution and apply the
/// transform to the returned value.
pub fn resolve_class_fully_cached(
    class: &ClassInfo,
    class_loader: &dyn Fn(&str) -> Option<ClassInfo>,
    cache: &ResolvedClassCache,
) -> ClassInfo {
    resolve_class_fully_inner(class, class_loader, Some(cache), &[])
}

/// Resolve a class fully, using the cache when available.
///
/// This is the preferred entry point for code paths that may or may
/// not have access to a [`ResolvedClassCache`] (e.g. context structs
/// where the cache field is `Option<&ResolvedClassCache>`).  When
/// `cache` is `Some`, behaves like [`resolve_class_fully_cached`];
/// when `None`, behaves like [`resolve_class_fully`].
pub fn resolve_class_fully_maybe_cached(
    class: &ClassInfo,
    class_loader: &dyn Fn(&str) -> Option<ClassInfo>,
    cache: Option<&ResolvedClassCache>,
) -> ClassInfo {
    resolve_class_fully_inner(class, class_loader, cache, &[])
}

/// Compute the fully-qualified name used as the cache key.
///
/// Mirrors the FQN construction in `update_ast_inner` and
/// `parse_and_cache_content`: `namespace\ClassName` when a namespace
/// is present, or just the short name otherwise.
fn class_fqn(class: &ClassInfo) -> String {
    match &class.file_namespace {
        Some(ns) if !ns.is_empty() => format!("{}\\{}", ns, class.name),
        _ => class.name.clone(),
    }
}

/// Shared implementation behind [`resolve_class_fully`] and
/// [`resolve_class_fully_cached`].
fn resolve_class_fully_inner(
    class: &ClassInfo,
    class_loader: &dyn Fn(&str) -> Option<ClassInfo>,
    cache: Option<&ResolvedClassCache>,
    generic_args: &[String],
) -> ClassInfo {
    let fqn = class_fqn(class);
    let cache_key: ResolvedClassCacheKey = (fqn.clone(), generic_args.to_vec());

    // ── Cache lookup ────────────────────────────────────────────────
    if let Some(cache) = cache
        && let Ok(map) = cache.lock()
        && let Some(cached) = map.get(&cache_key)
    {
        return cached.clone();
    }

    // ── Uncached resolution ─────────────────────────────────────────
    let mut merged = resolve_class_with_inheritance(class, class_loader);
    let providers = default_providers();
    if !providers.is_empty() {
        apply_virtual_members(&mut merged, class_loader, &providers, cache);
    }

    // 3. Merge members from implemented interfaces.
    //    Interfaces can declare `@method` / `@property` / `@property-read`
    //    tags that should be visible on implementing classes.  We collect
    //    interfaces from the class itself and from every parent in the
    //    extends chain, then fully resolve each interface (which applies
    //    its own virtual member providers) and merge any members that
    //    don't already exist.
    let mut all_iface_names: Vec<String> = class.interfaces.clone();
    {
        let mut current = class.clone();
        let mut depth = 0u32;
        while let Some(ref parent_name) = current.parent_class {
            depth += 1;
            if depth > 20 {
                break;
            }
            if let Some(parent) = class_loader(parent_name) {
                for iface in &parent.interfaces {
                    if !all_iface_names.contains(iface) {
                        all_iface_names.push(iface.clone());
                    }
                }
                current = parent;
            } else {
                break;
            }
        }
    }
    for iface_name in &all_iface_names {
        if let Some(iface) = class_loader(iface_name) {
            // Use the cache for interface resolution too — many classes
            // implement the same interfaces (e.g. Countable, Iterator)
            // and this avoids re-resolving them every time.
            let iface_key: ResolvedClassCacheKey = (class_fqn(&iface), Vec::new());
            if let Some(c) = cache
                && let Ok(map) = c.lock()
                && let Some(cached) = map.get(&iface_key)
            {
                let resolved_iface = cached.clone();
                drop(map);
                for method in &resolved_iface.methods {
                    if !merged.methods.iter().any(|m| m.name == method.name) {
                        merged.methods.push(method.clone());
                    }
                }
                for property in &resolved_iface.properties {
                    if !merged.properties.iter().any(|p| p.name == property.name) {
                        merged.properties.push(property.clone());
                    }
                }
                for constant in &resolved_iface.constants {
                    if !merged.constants.iter().any(|c| c.name == constant.name) {
                        merged.constants.push(constant.clone());
                    }
                }
                continue;
            }

            let mut resolved_iface = resolve_class_with_inheritance(&iface, class_loader);
            if !providers.is_empty() {
                apply_virtual_members(&mut resolved_iface, class_loader, &providers, cache);
            }
            for method in resolved_iface.methods {
                if !merged.methods.iter().any(|m| m.name == method.name) {
                    merged.methods.push(method);
                }
            }
            for property in resolved_iface.properties {
                if !merged.properties.iter().any(|p| p.name == property.name) {
                    merged.properties.push(property);
                }
            }
            for constant in resolved_iface.constants {
                if !merged.constants.iter().any(|c| c.name == constant.name) {
                    merged.constants.push(constant);
                }
            }
        }
    }

    // ── Cache store ─────────────────────────────────────────────────
    if let Some(cache) = cache
        && let Ok(mut map) = cache.lock()
    {
        map.insert(cache_key, merged.clone());
    }

    merged
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests;
