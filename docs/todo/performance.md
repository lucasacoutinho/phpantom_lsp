# PHPantom — Performance

Internal performance improvements that reduce latency, memory usage,
and lock contention on the hot paths. These items are sequenced so
that structural fixes land before features that would amplify the
underlying costs (parallel file processing, full background indexing).

Items are ordered by **impact** (descending), then **effort** (ascending)
within the same impact tier.

| Label | Scale |
|---|---|
| **Impact** | **Critical**, **High**, **Medium-High**, **Medium**, **Low-Medium**, **Low** |
| **Effort** | **Low** (≤ 1 day), **Medium** (2-5 days), **Medium-High** (1-2 weeks), **High** (2-4 weeks), **Very High** (> 1 month) |

---

## 1. FQN secondary index for `find_class_in_ast_map`
**Impact: High · Effort: Low (fixed)**

**Status:** Fixed. `Backend` now carries a `fqn_index`
(`Arc<RwLock<HashMap<String, ClassInfo>>>`) that maps fully-qualified
class names directly to their parsed `ClassInfo`.
`find_class_in_ast_map` performs an O(1) hash lookup against this
index before falling back to the linear `ast_map` scan (which now
only serves as a safety net for anonymous classes or race conditions
during initial indexing).

The index is maintained in both `update_ast_inner` (stale entries
removed via the `old_fqns` snapshot, new entries inserted alongside
`class_index`) and `parse_and_cache_content_versioned` (entries
inserted after the `ast_map` write).

---

## 2. Reference-counted `ClassInfo` (`Arc<ClassInfo>`)
**Impact: High · Effort: Medium**

`ClassInfo` is a large struct: 30+ fields including `Vec<MethodInfo>`,
`Vec<PropertyInfo>`, `Vec<ConstantInfo>`, multiple `HashMap`s, and
many `Vec<String>` fields. It is deep-cloned constantly:

- `find_class_in_ast_map` returns `Some(cls.clone())`
- `find_or_load_class` clones the result from the ast_map
- `resolve_class_with_inheritance` starts with `class.clone()` and
  clones every parent method/property during merging
- `resolve_class_fully_inner` calls `resolve_class_with_inheritance`
  (more clones), then caches the result with `.clone()`
- `resolve_target_classes` returns `Vec<ClassInfo>` (each a full clone)
- Cache retrieval clones on read: `map.get(&key) → cached.clone()`

A single completion on `$this->` in a class with a deep inheritance
chain can produce dozens of full `ClassInfo` clones, each involving
deep copies of all method signatures, parameter lists, and docblock
strings.

Under full background indexing (indexing.md Phase 5), the ast_map
holds thousands of `ClassInfo` values. Cloning them out on every
lookup produces significant allocation pressure.

### Fix

Store `Arc<ClassInfo>` in `ast_map` instead of owned `ClassInfo`.
Retrieval becomes a cheap reference-count increment instead of a
deep copy. The `resolved_class_cache` should also store
`Arc<ClassInfo>` so that cache hits are free.

### Mutation

Inheritance merging (`resolve_class_with_inheritance`) mutates the
merged class. Use `Arc::make_mut` (copy-on-write) at the start of
merging: the first mutation clones the inner value, but subsequent
mutations on the same `Arc` are free. Code that only reads a
`ClassInfo` (the majority of call sites) never pays for a clone.

### Scope

This is a pervasive change that touches every function returning or
accepting `ClassInfo`. It can be done incrementally:

1. Change `ast_map` to store `Arc<ClassInfo>`. Update
   `find_class_in_ast_map` and `parse_and_cache_content_versioned`.
2. Change `resolved_class_cache` to store `Arc<ClassInfo>`. Update
   `resolve_class_fully_inner`.
3. Update `resolve_target_classes` and downstream consumers to accept
   `Arc<ClassInfo>` where possible.

Each step compiles and passes tests independently.

---

## 3. `RwLock` for read-heavy maps
**Impact: Medium · Effort: Low (fixed)**

**Status:** Fixed. All read-heavy `Arc<Mutex<…>>` fields on `Backend`
have been replaced with `Arc<parking_lot::RwLock<…>>`:

- `ast_map`, `symbol_maps`, `use_map`, `namespace_map`
- `class_index`, `classmap`
- `global_functions`, `global_defines`
- `open_files`
- `workspace_root`, `psr4_mappings`

Fields that are rarely accessed or always written use
`parking_lot::Mutex` (no poisoning, no `Result` unwrapping):

- `resolved_class_cache` — frequently written (cache stores)
- `php_version`, `vendor_uri_prefix`, `vendor_dir_name`, `config` —
  written once during init, read rarely
- `diag_pending_uri` — tiny critical section

All `.lock().ok()?` / `.lock().map(…)` patterns were simplified to
direct `.read()` or `.write()` calls. `parking_lot` v0.12 added as a
dependency.

---

## 4. `HashSet` dedup in inheritance merging
**Impact: Medium · Effort: Low (fixed)**

**Status:** Fixed. All member deduplication during inheritance merging
now uses `HashSet<String>` lookups instead of linear scans.

A `MergeDedup` struct (with `methods`, `properties`, and `constants`
`HashSet`s) is built from the class's own members at the start of
`resolve_class_with_inheritance` and threaded through
`merge_traits_into` (including recursive calls) so that every
addition is checked in O(1).

The same pattern is applied in `merge_virtual_members`,
`merge_interface_members_into`, `PHPDocProvider::provide` (Phases 1,
1b, 1c, and mixin collection), `collect_mixin_members` (via a
`MixinDedup` struct passed through recursion), and
`LaravelModelProvider::provide`.

The `merged.methods.iter().any(...)` calls in
`definition/implementation.rs` were intentionally left as linear scans
because they run once per lookup, not per-member-per-level.

---

## 5. `Arc<String>` for file content in `open_files`
**Impact: Low-Medium · Effort: Low (fixed)**

**Status:** Fixed. `open_files` now stores `HashMap<String, Arc<String>>`.
`did_open` and `did_change` wrap the file text in `Arc::new()` once on
insertion. `get_file_content` (which returns an owned `String`) still
deep-clones for callers that need it, but the new `get_file_content_arc`
returns a cheap `Arc::clone` for hot paths like cross-file reference
scanning. The diagnostic worker's snapshot (`files.get(&uri).clone()`)
is now an atomic increment instead of a full string copy.

---

## 6. `Arc<SymbolMap>` to avoid snapshot cloning
**Impact: Low-Medium · Effort: Low (fixed)**

**Status:** Fixed. `symbol_maps` now stores
`HashMap<String, Arc<SymbolMap>>`. `update_ast_inner` wraps the
extracted symbol map in `Arc::new()` before insertion.
`user_file_symbol_maps` returns `Vec<(String, Arc<SymbolMap>)>` where
each entry is a cheap `Arc::clone`. All read sites (diagnostics,
code actions, completion, hover, definition, highlight) that previously
deep-cloned a `SymbolMap` now clone the `Arc` instead, and access
fields through `Deref`.

---

## 7. Recursive string substitution in `apply_substitution`
**Impact: Medium · Effort: High**

Generic type substitution (`apply_substitution`) does recursive
string parsing and re-building for every type string. It handles
nullable, union, intersection, generic, callable, and array types
by splitting, recursing, and re-joining strings. Each recursion
level allocates new `String` values.

This runs on every inherited method's return type, every parameter's
type hint, and every property's type hint when template substitution
is active. In a deeply-generic framework like Laravel (where
`Collection<TKey, TValue>` flows through multiple inheritance
levels), this function is called hundreds of times per resolution,
each time allocating new strings.

The resolved-class cache (type-inference.md §31) mitigates this by
caching the result, so substitution only runs on cache misses. But
cache misses still happen: first access, after edits that trigger
invalidation, and for generic classes with different type arguments.

### Fix (long-term)

Replace the string-based type representation with a parsed type AST
(an enum of `TypeNode` variants: `Named`, `Union`, `Intersection`,
`Generic`, `Nullable`, `Array`, `Callable`, etc.). Parse the type
string once during class extraction. Substitution becomes a tree
walk that swaps `Named` leaf nodes, avoiding all string allocation
and re-parsing.

This is a significant refactor that touches the parser, docblock
extraction, type resolution, and inheritance merging. It should be
evaluated after the lower-effort items are done and profiling
confirms that substitution remains a measurable cost.

### Fix (short-term)

Two targeted optimisations that reduce allocation without the full
refactor:

1. **Early exit.** Before recursing, check whether the type string
   contains any of the substitution map's keys. If no key appears
   as a substring, return the input unchanged (no allocation). This
   skips the majority of type strings that don't reference template
   parameters.

2. **Cow return type.** Change `apply_substitution` to return
   `Cow<'_, str>` instead of `String`. When no substitution occurs
   (the common case), return the borrowed input. Only allocate a new
   `String` when a replacement actually happens.

---

## 8. Incremental text sync
**Impact: Low-Medium · Effort: Medium**

PHPantom uses `TextDocumentSyncKind::FULL`, meaning every
`textDocument/didChange` notification sends the entire file content.
For large files (5000+ lines, common in legacy PHP), sending 200 KB
on every keystroke adds measurable IPC overhead.

The practical benefit is bounded: Mago requires a full re-parse
regardless of how the change was received. The saving is purely in
the data transferred over the IPC channel. For files under ~1000
lines this is negligible.

This item is already tracked in [lsp-features.md §17](lsp-features.md#17-incremental-text-sync)
and is included here for completeness. The effort and implementation
plan are unchanged. It is the lowest-priority performance item
because full-file sync is rarely the bottleneck in practice.