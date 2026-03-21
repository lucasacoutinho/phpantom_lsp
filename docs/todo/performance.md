# PHPantom — Performance

Internal performance improvements that reduce latency, memory usage,
and lock contention on the hot paths. These items are sequenced so
that structural fixes land before features that would amplify the
underlying costs (parallel file processing, full background indexing).

Items are ordered by **impact** (descending), then **effort** (ascending)
within the same impact tier.

| Label      | Scale                                                                                                                  |
| ---------- | ---------------------------------------------------------------------------------------------------------------------- |
| **Impact** | **Critical**, **High**, **Medium-High**, **Medium**, **Low-Medium**, **Low**                                           |
| **Effort** | **Low** (≤ 1 day), **Medium** (2-5 days), **Medium-High** (1-2 weeks), **High** (2-4 weeks), **Very High** (> 1 month) |

---

## P1. Reference-counted `ClassInfo` (`Arc<ClassInfo>`)

**Impact: High · Effort: Medium**

### Completed

The three main data stores hold `Arc<ClassInfo>`:

- `ast_map`: `HashMap<String, Vec<Arc<ClassInfo>>>`
- `fqn_index`: `HashMap<String, Arc<ClassInfo>>`
- `resolved_class_cache`: `HashMap<ResolvedClassCacheKey, Arc<ClassInfo>>`

Insertion sites (`parse_and_cache_content_versioned`, `update_ast_inner`)
wrap `ClassInfo` in `Arc` before storing.

The retrieval and resolution pipeline now returns `Arc<ClassInfo>`
from the storage layer through to the class-loader boundary:

- `find_class_in_ast_map` returns `Option<Arc<ClassInfo>>`.
- `find_or_load_class` returns `Option<Arc<ClassInfo>>`.
- `FileContext::classes` is `Vec<Arc<ClassInfo>>`.
- `file_context()` clones `Arc`s instead of deep-cloning.
- `ResolutionCtx::all_classes` and `VarResolutionCtx::all_classes`
  are `&[Arc<ClassInfo>]`.
- All downstream functions that accept `all_classes` or
  `local_classes` parameters (`resolve_variable_types`,
  `type_hint_to_classes`, `resolve_type_alias`, helpers in
  `source/helpers.rs`, `hover/variable_type.rs`, diagnostics,
  definition, references, etc.) accept `&[Arc<ClassInfo>]`.
- `find_class_at_offset` accepts `&[Arc<ClassInfo>]` and returns
  `&ClassInfo` via `Arc::Deref`.
- `find_class_by_name` accepts `&[Arc<ClassInfo>]` and returns
  `Option<&Arc<ClassInfo>>`, enabling callers to `Arc::clone`
  instead of deep-cloning when they need ownership.

The `class_loader` closures and `resolve_class_name` now return
`Option<Arc<ClassInfo>>`, eliminating the deep clone at the
class-loader boundary. All ~32 files that accept a `class_loader`
parameter use the updated signature
`&dyn Fn(&str) -> Option<Arc<ClassInfo>>`.

Consumers that only read the `ClassInfo` (checking methods,
properties, parent chains, etc.) access it through `Arc`'s `Deref`
at zero cost. Mutation sites (inheritance merging in
`resolve_class_with_inheritance`, parent chain walks, virtual member
providers) call `Arc::unwrap_or_clone()` to obtain an owned
`ClassInfo` only when they actually need to mutate.

The full-resolution functions (`resolve_class_fully`,
`resolve_class_fully_cached`, `resolve_class_fully_maybe_cached`)
return `Arc<ClassInfo>`. Cache hits are a cheap `Arc::clone`
instead of a deep copy, and cache misses allocate the `Arc` once
(shared between the cache store and the return value).

`resolve_static_owner_class` returns `Option<Arc<ClassInfo>>`,
passing through the `Arc` from the class loader and
`find_class_by_name` without deep-cloning. Callers that only read
the result access it through `Deref` at zero cost.

The subject-resolution pipeline returns `Vec<Arc<ClassInfo>>`:

- `resolve_target_classes` and `resolve_target_classes_expr` return
  `Vec<Arc<ClassInfo>>`. Class-loader hits and `find_class_by_name`
  hits pass the `Arc` through without deep-cloning. The ~37 callers
  that iterate by reference work via `Deref` with no code changes.
- `resolve_call_return_types_expr` and
  `resolve_method_return_types_with_args` return
  `Vec<Arc<ClassInfo>>`, eliminating the `Arc::new` wrapping that
  was previously needed at the `CallExpr` boundary.
- `build_union_completion_items` accepts `&[Arc<ClassInfo>]`.
- `push_unique_arc` and `extend_unique_arc` helpers deduplicate
  `Vec<Arc<ClassInfo>>` collections without unwrapping.

### Remaining work

**`type_hint_to_classes` → `Vec<Arc<ClassInfo>>`.**
`type_hint_to_classes` is the main bridge between the type-string
world and the class-object world. It is called from both the
call-resolution pipeline (which already returns `Vec<Arc<ClassInfo>>`
and currently wraps each result with `Arc::new`) and the variable-
resolution pipeline (which still operates on `Vec<ClassInfo>`).
Changing `type_hint_to_classes` (and its recursive helper
`type_hint_to_classes_depth`) to return `Vec<Arc<ClassInfo>>` would
eliminate ~15 `Arc::new` wraps at call sites that were added during
the call-resolution conversion, and would be a natural stepping
stone if someone later decides to convert the variable-resolution
pipeline.

The variable-resolution pipeline (`resolve_variable_types`,
`resolve_rhs_expression`, `check_expression_for_assignment`, and
~30 helper functions) still operates on `Vec<ClassInfo>` internally.
Converting it would eliminate ~29 deep clones at bridge sites in
`rhs_resolution.rs`, `foreach_resolution.rs`, and
`closure_resolution.rs`, but the cascade touches ~86 sites across
the subsystem. The effort-to-impact ratio is poor: each eliminated
clone saves one per-request copy, not a hot-loop copy, and the
parent-chain walks in `declaring.rs`, `inheritance.rs`, and
`phpdoc.rs` will always need `Arc::unwrap_or_clone` for mutation
regardless.

---

## P1.5. Layered class resolution (zero-copy inheritance)

**Impact: High · Effort: Very High**

### Problem

`resolve_class_with_inheritance` builds a flat `ClassInfo` by cloning
the base class and then copying every method, property, and constant
from traits, parents, and interfaces into the result. For an Eloquent
model this means deep-copying hundreds of `MethodInfo` structs (each
containing `String` fields, `Vec<ParameterInfo>`, etc.). Even with
`SharedVec` making the top-level Vec clone O(1), the individual
`MethodInfo` clones during the merge are the single largest remaining
allocation cost (~4 % of CPU in `perf` profiles).

The fundamental issue: the resolved class is a **copy** of all
inherited members rather than a **view** over immutable originals.

### Ideal architecture

Replace the flat merged `ClassInfo` with a layered view that
references the originals without copying:

```text
ResolvedClass {
    own:     Arc<ClassInfo>,                  // parsed, immutable
    traits:  Vec<Arc<ClassInfo>>,             // resolved traits
    parent:  Option<Arc<ResolvedClass>>,      // resolved parent (recursive)
    virtual: Vec<Arc<MethodInfo>>,            // @method, @mixin, scopes
    iface_fill: HashMap<String, TypeFillIn>,  // interface type enrichment
}
```

Member lookups walk the layers (own → traits → parent → virtual)
instead of iterating a single flat Vec. Dedup is handled by a
name-based `HashSet` built lazily or maintained incrementally.

Benefits:
- **Zero-copy inheritance.** Moving a method from parent to child is
  an `Arc::clone` (refcount bump), not a `MethodInfo` deep clone.
- **Shared structure.** Two child classes that extend the same parent
  share the parent's `Arc<ResolvedClass>` — the parent's methods
  exist in memory once, not once per child.
- **Cheaper cache invalidation.** Editing a child class only rebuilds
  the child's layer; the parent layer stays cached.

### Migration path

1. **`Arc<MethodInfo>` everywhere.** Change `SharedVec<MethodInfo>`
   to `SharedVec<Arc<MethodInfo>>` (and same for properties/constants).
   This makes individual method clones O(1) within the existing flat
   architecture — an incremental win without changing consumers.

2. **Introduce `ResolvedClass` struct.** Start with a thin wrapper
   that holds the flat `ClassInfo` inside, exposing the same
   iteration API. Consumers migrate incrementally.

3. **Layered storage.** Replace the flat `ClassInfo` inside
   `ResolvedClass` with the layered structure. Change iteration to
   walk layers. This is the big step — every `.methods.iter().find()`
   call site needs to use the layered iterator.

4. **Lazy dedup index.** Build a `HashMap<&str, (Layer, usize)>`
   on first access (or maintain it incrementally during layer
   construction) so that `find_method("foo")` is O(1) without
   scanning all layers.

### Risks

- Every consumer that does `.methods.iter()` or `.methods.len()`
  needs to work with the layered iterator. A `Deref`-based shim
  can ease migration but adds indirection.
- Mutation sites (`merge_traits_into`, virtual member providers)
  need to operate on the layer structure rather than pushing into
  a flat Vec.
- The `resolved_class_cache` currently stores `Arc<ClassInfo>`;
  it would store `Arc<ResolvedClass>` instead.

### When to implement

This is the right long-term direction but a major refactor (~1 month).
Evaluate after the `Arc<MethodInfo>` step (migration path step 1)
is complete and profiling confirms that the flat-merge copy cost is
still the dominant bottleneck.

---

## P2. Recursive string substitution in `apply_substitution`

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

The resolved-class cache mitigates this by
caching the result, so substitution only runs on cache misses. But
cache misses still happen: first access, after edits that trigger
invalidation, and for generic classes with different type arguments.

The short-term mitigations (early-exit check and `Cow` return type)
are implemented. The remaining work is the long-term structural fix.

### Fix

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

---

## P3. Parallel pre-filter in `find_implementors`

**Impact: Medium · Effort: Medium**

`find_implementors` Phase 3 reads every unloaded classmap file
sequentially: `fs::read_to_string`, string pre-filter for the target
name, then `parse_and_cache_file`. On a project with thousands of
vendor classes, this loop is dominated by I/O latency. The string
pre-filter rejects most files (the target name appears in very few),
so the vast majority of reads are wasted.

### Fix

Split Phase 3 into two sub-phases:

1. **Parallel pre-filter.** Collect the candidate paths into a
   `Vec<PathBuf>`, then use `std::thread::scope` to read files and
   run the `raw.contains(target_short)` check in parallel. Return
   only the paths that pass the filter along with their content.

2. **Sequential parse.** For the (few) files that pass, call
   `parse_and_cache_file` sequentially. This step mutates `ast_map`
   and calls `class_loader`, which may re-lock shared state.

The same pattern applies to Phase 5 (PSR-4 directory walk for files
not in the classmap). The pre-filter I/O is the bottleneck; the
parse step processes very few files and is fast.

### Trade-off

Thread spawning overhead is only worthwhile when the candidate set
is large. Skip parallelism when the candidate count is below a
threshold (e.g. 8 files).

---

## P4. `memmem` for block comment terminator search

**Impact: Low-Medium · Effort: Low**

The current block comment skip in `find_classes` and `find_symbols`
uses `memchr(b'*', ...)` and then checks the next byte for `/`.
This is effective but can false-match on `*` characters inside
docblock annotations (e.g. `@param`, `@return`, starred lines).
Each false match falls through to a single-byte advance, which is
correct but suboptimal for large docblocks.

### Fix

Replace `memchr(b'*', ...)` with `memmem::find(content[i..], b"*/")`.
This searches for the two-byte sequence `*/` directly, skipping all
intermediate `*` characters in a single SIMD pass. The `memmem`
searcher is already imported and used for keyword pre-screening.

For typical PHP files this is a marginal improvement. For files with
very large docblocks (e.g. generated API documentation classes with
hundreds of `@method` tags), it avoids O(n) false `*` matches inside
the comment body.

---

## P5. `memmap2` for file reads during scanning

**Impact: Low-Medium · Effort: Low**

All file-scanning paths (`scan_files_parallel_classes`,
`scan_files_parallel_psr4`, `scan_files_parallel_full`, and the
`find_implementors` pre-filter) use `std::fs::read(path)` which
copies the entire file into a heap-allocated `Vec<u8>`. When the OS
page cache already has the file mapped, `memmap2` can provide a
read-only view of the file's pages without any copy.

### Fix

Add `memmap2` as a dependency. In the parallel scan helpers, replace
`std::fs::read(path)` with `unsafe { Mmap::map(&file) }`. The
`find_classes` and `find_symbols` scanners already accept `&[u8]`,
so the change is confined to the call sites.

### Safety

Memory-mapped reads are `unsafe` because another process could
truncate the file while the map is live, causing a SIGBUS. In
practice this does not happen during LSP initialization (the user is
not deleting PHP files while the editor starts). A fallback to
`fs::read` on map failure handles edge cases.

### When to implement

Profile first. On Linux with a warm page cache the difference
between `read` and `mmap` is small for files under ~100 KB (which
covers most PHP files). The benefit is more pronounced on macOS
where `read` involves an extra kernel-to-userspace copy. If
profiling shows that file I/O is no longer the bottleneck after
parallelisation, this item can be dropped.

---

## P6. O(n²) transitive eviction in `evict_fqn`

**Impact: Low-Medium · Effort: Low**

The `evict_fqn` function in `virtual_members/mod.rs` runs a
fixed-point loop that scans the entire resolved-class cache on each
iteration to find transitive dependents. In a large project with a
deep class hierarchy (common in Laravel codebases with hundreds of
Eloquent models), editing a base class can trigger a cascade of
evictions where each round does a full cache scan.

The `depends_on_any` helper also matches against both the FQN and
the short name of the evicted class, which increases the chance of
false-positive transitive evictions (e.g. two unrelated classes that
share a short name like `Builder`).

### Fix

Build a reverse-dependency index (`HashMap<String, Vec<String>>`)
that maps each FQN to the set of cached FQNs that directly depend
on it. Maintain this index alongside cache insertions and removals.
On eviction, walk the reverse index instead of scanning the entire
cache, turning the O(n²) loop into O(dependents).

If the reverse index is too much bookkeeping, a simpler first step
is to collect all dependents in a single pass (instead of the
current iterative fixed-point loop) by doing a breadth-first walk
of the dependency graph within the cache.

---

## P7. `diag_pending_uris` uses `Vec::contains` for deduplication

**Impact: Low · Effort: Low**

`schedule_diagnostics` and `schedule_diagnostics_for_open_files`
deduplicate pending URIs with `Vec::contains`, which is O(n) per
insertion. When a class signature changes, every open file is
queued, and each insertion scans the entire pending list.

For typical usage (< 50 open files) this is imperceptible. It
becomes measurable only with hundreds of open tabs and rapid
cross-file edits.

### Fix

Replace `Vec<String>` with `IndexSet<String>` (from `indexmap`) or
`HashSet<String>` + a separate `Vec<String>` for ordering. The
worker drains the collection on each wake, so insertion order is
not important and a plain `HashSet` suffices.

---

## P8. `find_class_in_ast_map` linear fallback scan

**Impact: Low · Effort: Low**

The fast O(1) `fqn_index` lookup in `find_class_in_ast_map` covers
the common case. The slow fallback iterates every file in `ast_map`
linearly. The comment says this covers "race conditions during
initial indexing" and anonymous classes.

During initial indexing with many files open, the fallback could
cause micro-stutters if the `fqn_index` has not been populated yet
for a requested class. In steady state the fallback is rarely hit.

### Fix

Audit the code paths that can reach the fallback to determine
whether they are still reachable after the `fqn_index` was added.
If they are not, replace the fallback with a `None` return and a
debug log. If they are, consider populating `fqn_index` earlier in
the pipeline (e.g. during the byte-level scan phase) to close the
window.

---

## P9. `resolved_class_cache` generic-arg specialisation

**Impact: Medium · Effort: Medium**

The resolved-class cache is keyed by `(FQN, Vec<String>)`. Every
distinct generic instantiation of the same class (e.g.
`Builder<User>`, `Builder<Order>`, `Builder<Product>`) triggers a
full `resolve_class_fully` call, even though the base resolution
(inheritance merging, trait merging, virtual member injection) is
identical. Only the final generic substitution differs.

In a Laravel codebase with hundreds of Eloquent models, this means
`Builder` is fully resolved hundreds of times, once per model.

### Fix

Cache the base-resolved class (before generic substitution)
separately, keyed by FQN alone. When a generic instantiation is
requested, look up the base-resolved class and apply
`apply_substitution` on top. The substitution step is cheap
(string rewriting) compared to the full resolution (inheritance
walking, trait merging, virtual member providers).

This requires splitting `resolve_class_fully` into two stages:
base resolution (cached by FQN) and generic specialisation (cached
by `(FQN, Vec<String>)` as today, but with a much cheaper miss
path).

---

## P10. Redundant `parse_and_cache_file` from multiple threads

**Impact: Medium · Effort: Low**

When two threads simultaneously try to resolve the same vendor
class, both miss `fqn_index`, both call `parse_and_cache_file`,
and both parse the same file. The second parse is wasted work.
This is most visible during the Phase 2 diagnostic pass when many
threads resolve vendor classes for the first time.

### Fix

Add a `DashSet<String>` (or similar) of "currently being parsed"
URIs. Before calling `parse_and_cache_file`, insert the URI into
the set. If the insert fails (another thread is already parsing
it), spin-wait or skip and let the other thread's result propagate
through `fqn_index`. Remove the URI from the set after parsing
completes.

---

## P11. Uncached base-resolution in `build_scope_methods_for_builder`

**Impact: Low-Medium · Effort: Low**

`build_scope_methods_for_builder` calls
`resolve_class_with_inheritance` (base resolution) for the model
class. This is not covered by the thread-local resolved-class
cache, which stores fully-resolved classes (after virtual member
injection), not base-resolved ones.

Every time an Eloquent `Builder<Model>` is resolved with scope
injection, the model is base-resolved from scratch. With many
Builder instantiations in a single file this adds up.

### Fix

Either introduce a separate base-resolution cache (keyed by FQN),
or restructure so `build_scope_methods_for_builder` accepts the
already-resolved model class from the caller (which may already
have it from the resolved-class cache).

---

## P12. `find_or_load_function` Phase 1.75 serial bottleneck

**Impact: Low · Effort: Low**

For the first unknown function that misses both `global_functions`
and `autoload_function_index`, Phase 1.75 iterates all known
autoload file paths and calls `update_ast` on each unparsed one
until the function is found. With ~50 autoload files this is a
one-time cost per thread, but it blocks the thread while it
happens.

### Fix

Pre-load all autoload files during initialisation (in the
`initialized` handler, after the byte-level scan). This moves the
cost to startup, where it can run in parallel with other init work,
and eliminates the blocking fallback during interactive use.

---

## Appendix: Profiling

### Commands

```sh
# Record (Ctrl-C after ~60s):
perf record -g --call-graph dwarf -- \
  ./target/release/phpantom_lsp analyze \
  src/core/Purchase/Services/PurchaseFileService.php

# Text report (top functions):
perf report --stdio --no-children | head -80

# Flamegraph (requires the `flamegraph` crate or perf-tools):
perf script | flamegraph > /tmp/phpantom.svg
```

### Pathological test file

`PurchaseFileService.php` (~700-line Eloquent-heavy service with
~55 imports) is the most expensive single file encountered so far.
The per-collector timing is controlled by a `>= 2s` threshold in
`src/analyse.rs` Phase 2 (search for `⏱`). It prints a breakdown
like:

```
⏱  63.2s  src/core/Purchase/Services/PurchaseFileService.php
  [fast=1ms cls=40ms mem=23696ms fn=12ms unres=16781ms arg=22568ms impl=0ms depr=54ms]
```
