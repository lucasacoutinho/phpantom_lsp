# PHPantom — Eager Class Resolution

The stack overflow in `resolve_class_fully_inner` (caused by unbounded
mutual recursion between the lazy resolution pipeline and virtual
member providers) has been fixed via topological sort with eager cache
pre-population (ER1) plus recursion/depth guards throughout the
resolution pipeline (ER2). Virtual member application is now fully
iterative (ER3): providers read from the resolved-class cache instead
of resolving dependencies recursively, and topological population
order guarantees all dependencies are already cached. The depth guards
from ER2 remain as safety nets that never fire in practice.
Incremental repopulation on file change (ER4) ensures evicted classes
are eagerly re-populated in dependency order. String interning (ER6)
makes all symbol name comparisons pointer-sized and gives identity
hashing on hot-path lookups.

| Label      | Scale                                                                                                                  |
| ---------- | ---------------------------------------------------------------------------------------------------------------------- |
| **Impact** | **Critical**, **High**, **Medium-High**, **Medium**, **Low-Medium**, **Low**                                           |
| **Effort** | **Low** (≤ 1 day), **Medium** (2-5 days), **Medium-High** (1-2 weeks), **High** (2-4 weeks), **Very High** (> 1 month) |

---

## The core problem: resolution is called per expression, not once

PHPantom's diagnostic pipeline calls `resolve_class_fully_inner`
(which runs `resolve_class_with_inheritance` + virtual member
application + interface merging) every time a variable's type needs
to be resolved during analysis. On the `shared` project (~36k PHP
files), a single `analyze` run triggers **251,032 calls** to
`resolve_class_with_inheritance` in the diagnostic phase alone.

The resolved-class cache mitigates this for repeated lookups of the
same class, but the forward walker (`build_diagnostic_scopes`) still
pays the cost of cache-key construction, lock acquisition, and
context setup on every expression. On `example.php` (~6 000 lines,
~300 classes), `build_diagnostic_scopes` alone takes **35.6 seconds**
(78% of total analysis time in debug builds). The diagnostic
collectors add another 9.8 seconds (21%). Everything else (init,
parse, eager populate) is under 1 second.

### How Mago avoids this

Mago (`mago/crates/codex/src/`) uses a fundamentally different
architecture:

1. **Separated method storage.** `ClassLikeMetadata.methods` is an
   `AtomSet` (just names). The actual method metadata lives in a
   global `CodebaseMetadata.function_likes: HashMap<(Atom, Atom),
   FunctionLikeMetadata>` keyed by `(declaring_class, method_name)`.
   During inheritance, Mago copies only lightweight `Atom` identifiers
   (interned string pointers). It never clones method metadata.

2. **One-pass population.** `populate_codebase()` toposorts classes,
   then walks each one iteratively, merging trait/parent/interface
   members by copying `MethodIdentifier` atoms. After population the
   `CodebaseMetadata` is immutable. The analyzer just reads from it.

3. **O(1) method resolution.** The analyzer resolves a method call
   with two hash lookups: get class metadata, get method by
   identifier. No inheritance walk, no merging, no virtual member
   application. Everything was pre-computed.

4. **String interning (`Atom`).** All names are interned. Equality
   is a pointer comparison. HashMap lookups use identity hashing.

5. **Single-pass analysis.** The analyzer walks the AST once,
   resolving types and emitting diagnostics inline. There is no
   separate "build scope cache" pass followed by diagnostic
   collectors. Each statement is analyzed as it is encountered.

---

## ER5 — Mago-style separated metadata

**Impact: High · Effort: High**

Restructure PHPantom's class/method storage to match Mago's
architecture. This is the single highest-impact change for analysis
performance: it eliminates the per-expression resolution cost that
dominates analysis time.

### Completed (Phases 1-3)

- **Phase 1: MethodStore infrastructure.** Added
  `Backend.method_store: MethodStore` (an
  `Arc<RwLock<HashMap<(String, String), Arc<MethodInfo>>>>`)
  populated alongside `fqn_index` in `update_ast_inner` and
  `parse_and_cache_content_versioned`. Eviction on re-parse
  via `evict_methods_for_fqns`.
- **Phase 2: Centralised lookup helpers.** Added
  `ClassInfo::get_method()`, `get_method_ci()`,
  `get_method_arc()`, and `has_method()`. Migrated all ~60
  non-test call sites from
  `.methods.iter().find(|m| m.name == X)` /
  `.methods.iter().any(|m| m.name == X)` to these helpers.
  Future phases can swap the linear scan for an O(1) index
  without touching call sites again.
- **Phase 3: Arc-wrapped methods.** Changed
  `ClassInfo.methods` from `SharedVec<MethodInfo>` to
  `SharedVec<Arc<MethodInfo>>`. Inheritance merge now uses
  `Arc::clone` (refcount bump) when no generic substitution
  is needed, avoiding deep clones of the full `MethodInfo`
  struct. When substitution IS needed, `Arc::make_mut` gives
  copy-on-write semantics.
- **Phase 3.5: Atom-based FQN and cache keys.** Changed
  `ClassInfo::fqn()` to return `Atom` (interned string,
  Copy, pointer-sized equality) instead of allocating a new
  `String` on every call. Changed `ResolvedClassCacheKey`
  from `(String, Vec<String>)` to `(Atom, Vec<String>)`.
  This eliminates the FQN string allocation on every cache
  lookup (previously ~251K allocations per diagnostic pass
  on the `shared` project). Cache key construction for the
  common non-generic case is now allocation-free (just a
  Copy of the `Atom` + an empty `Vec::new()`).

### Remaining — Phase 4: Separated storage with pre-populated metadata

**Reference implementation:** `mago/crates/codex/src/`

The goal is to make the resolved codebase metadata **immutable after
population** so that the diagnostic/analysis pass never calls
`resolve_class_with_inheritance` at all. Instead, it reads from a
pre-populated, flat metadata store using O(1) lookups.

Phase 4 is broken into sub-phases:

#### Phase 4a: Method index on ClassInfo ✓

Added `method_index: AtomMap<u32>` and `indexed_method_count: u32`
to `ClassInfo`. The `get_method`, `get_method_arc`, and `has_method`
helpers use O(1) hash lookup when the index is valid, falling back
to linear scan when the class has been mutated after indexing
(detected via `indexed_method_count` mismatch). The index is rebuilt
once in `resolve_class_fully_inner` right before the resolved class
is cached, ensuring all cached classes have a valid index while
classes under construction safely use the linear-scan fallback.

#### Phase 4b: Eliminate double scope walk ✓

Fixed a bug where `build_diagnostic_scopes` ran twice per file in
release builds: once explicitly in the analyze loop, and again
inside `collect_slow_diagnostics`. Added an early-return guard that
skips the walk when the scope cache is already populated. This cut
`slow` time from 9.4s to 0.6s on example.php.

#### Phase 4c: Eliminate ClassInfo cloning (next)

**Impact: Critical. Effort: Medium-High.**

Fresh profiling (perf, release build, example.php) shows the
dominant cost is **ClassInfo cloning and allocation churn**:

| Symbol | Self % | Notes |
|---|---|---|
| `_int_malloc` | 7.3% | heap allocation |
| `__memmove_avx_unaligned_erms` | 5.7% | memcpy from clones |
| `ClassInfo::clone` | 4.2% | deep-clone of ClassInfo |
| `_int_free_chunk` | 4.2% | heap deallocation |
| `cfree` | 3.6% | free() |
| `drop_in_place<ClassInfo>` | 3.3% | destructor |
| `core::fmt::write` | 3.0% | string formatting |
| `malloc_consolidate` | 2.8% | allocator bookkeeping |
| `Vec::clone` | 2.4% | vector cloning |
| `String::clone` | 2.2% | string cloning |

**Combined: ~38% of total CPU in allocation, cloning, and dropping.**

The root cause: `type_hint_to_classes_typed` returns
`Vec<ClassInfo>` (owned). Every caller that has an
`Arc<ClassInfo>` must deep-clone the struct (which contains
`SharedVec<Arc<MethodInfo>>`, `SharedVec<PropertyInfo>`, multiple
`Vec<Atom>`, `AtomMap` fields, etc.) just to pass it through
the resolution pipeline.

**Plan:**

1. Change `ResolvedType.class_info` from `Option<ClassInfo>` to
   `Option<Arc<ClassInfo>>`. This propagates Arc sharing through
   the entire variable resolution pipeline.

2. Change `type_hint_to_classes_typed` to return
   `Vec<Arc<ClassInfo>>`. Callers that need mutation (generic
   substitution, scope injection) clone only when necessary
   via `Arc::make_mut` or `Arc::unwrap_or_clone`.

3. Change `ResolvedType::into_classes` to return
   `Vec<Arc<ClassInfo>>` (rename to `into_arced_classes` or
   merge with existing method).

4. Update `resolve_rhs_method_call_inner` and other RHS
   resolution functions to work with `&ClassInfo` references
   from the Arc instead of owned values.

5. Change `find_class_by_name` callers to use `Arc::clone`
   (refcount bump) instead of `ClassInfo::clone` (deep copy).

**Expected impact:** Eliminates ~10-15% of total CPU (clone +
drop + associated malloc/free). Combined with the double-walk
fix, should bring example.php from ~9.5s to ~5-6s.

#### Phase 4d: Reduce string formatting overhead

**Impact: Medium. Effort: Low-Medium.**

3% self-time in `core::fmt::write` + 1.4% in `format_inner` +
1.3% in `Ustr::from` (atom interning). These come from:

- `format!("{}\\{}", ns, name)` in `ClassInfo::fqn()` — called
  on every resolution. Already mitigated by returning `Atom` but
  the initial intern still allocates.
- Type string construction during template substitution.
- `name.to_string()` calls throughout the resolution pipeline.

Plan: audit hot-path `format!()` calls and replace with
pre-computed or cached values where possible.

### Profiling data (current, release build, example.php)

Measured after Phase 4a + double-walk fix:

| Phase | Wall time | Notes |
|---|---|---|
| Init + parse + eager populate | < 1s | negligible |
| `build_diagnostic_scopes` | **8.9s** | forward walker |
| Fast diagnostics | 0.0s | |
| Slow diagnostics | 0.6s | (was 9.4s before double-walk fix) |
| **Total** | **~9.5s** | target: < 3s |

Instrumentation data from the scope walk:

- 8,726 calls to `resolve_rhs_expression` (8.3s top-level time)
- 5,559 resolved-class cache hits, 396 misses
- Subject pipeline fallback: 1 call (negligible)

The 8.3s is dominated by allocation churn (ClassInfo cloning)
inside the type resolution pipeline, not by resolution logic.

---


## Success criteria

- `phpantom_lsp analyze --project-root . example.php` completes
  in under 2 seconds (release build).
- Analysis time for example.php (~300 classes) approaches Mago's
  speed (sub-second).
- No test regressions in the existing test suite.
- Memory usage for the populated metadata is within 2x of the
  current `ast_map` + `class_index` combined size.
- The diagnostic/analysis pass never calls
  `resolve_class_with_inheritance`. All class metadata is
  pre-populated and immutable during analysis.