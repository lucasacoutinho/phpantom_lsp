# PHPantom — Signature Help: Improvement Plan

Signature help is architecturally solid — dual-path detection (AST-based
`CallSite` lookup + text-based fallback), precomputed comma offsets for
active parameter tracking, content patching for unclosed parens, and
chain/constructor/first-class-callable resolution all work well.

The remaining work is almost entirely **presentation-layer wiring**: the
data needed for rich signature help already exists on `ParameterInfo`,
`MethodInfo`, and `FunctionInfo` (added during the hover overhaul), but
`build_signature` and `ResolvedCallableTarget` don't propagate it to the
LSP response yet.

Items are ordered by impact (descending), then effort (ascending).

---

<!-- ============================================================ -->
<!--  TIER 1 — WIRING (data exists, just needs plumbing)          -->
<!-- ============================================================ -->

## Tier 1 — Wire Existing Data

✅ **All Tier 1 items are complete.** The signature help popup is now two
lines: a compact parameter list using native PHP types with a shortened
return type, plus a per-parameter `@param` description (prefixed with
the effective docblock type when it differs from the native hint).
Default values appear in parameter labels. Retrigger on `)` dismisses
the popup.

---

<!-- ============================================================ -->
<!--  TIER 2 — NEW EXTRACTION                                     -->
<!-- ============================================================ -->

## Tier 2 — New Extraction Work

### 4. Attribute constructor signature help
**Impact: Medium · Effort: Medium**

PHP 8 attributes take constructor arguments:

```php
#[Route('/users', methods: ['GET'])]
class UserController {}
```

Signature help should fire inside the attribute's parentheses and show
the attribute class's `__construct` parameters — the same as `new Route(`.

#### Current state

`emit_call_site` in `symbol_map/extraction.rs` only handles
`CallExpression`, `ObjectCreationExpression`, and their variants.
`Attribute` nodes are not visited for call-site emission.

#### Implementation

1. **Emit `CallSite` for attributes** — in `symbol_map/extraction.rs`,
   add handling in the attribute extraction path.  When an `Attribute`
   node has an `argument_list`, emit a `CallSite` with:
   - `call_expression: format!("new {}", attr_name)` — so the existing
     constructor resolution path picks it up.
   - `args_start` / `args_end` from the attribute's argument list parens.
   - `comma_offsets` from the argument list's separator tokens.

2. **Resolve the attribute name** — the attribute name must be resolved
   through the file's use-map (same as class references).  The existing
   `CallSite` resolution in `resolve_callable_target` handles `new ClassName`
   and resolves it via the class loader, so this should work automatically.

3. **Edge case: nested attributes** (PHP 8.1) — `#[Outer(new Inner(...))]`
   should show `Inner`'s constructor when the cursor is inside `Inner(`.
   This should work naturally since `ObjectCreationExpression` inside
   attribute argument lists is already handled.

#### Tests

- Unit test: `extract_symbol_map` on `#[FooAttr($x, ` → assert a
  `CallSite` with `call_expression: "new FooAttr"` and correct
  `args_start` / `comma_offsets`.
- Integration test: define an attribute class with `__construct(string $path, array $methods)`,
  use it as `#[FooAttr(`, request signature help → assert the constructor
  parameters appear.
- Integration test: cursor on second parameter `#[FooAttr('/path', ` →
  assert `active_parameter` is 1.
- Integration test: nested `#[Outer(new Inner(` → assert Inner's
  constructor is shown.

---

### 5. Closure / arrow function parameter signature help
**Impact: Medium · Effort: Medium**

Signature help should work when invoking a variable that holds a closure
or arrow function:

```php
$format = fn(string $name, int $age): string => "$name ($age)";
$format('Alice', 30);  // ← signature help here
```

#### Current state

`extract_callable_target_from_variable` handles first-class callables
(`$fn = makePen(...)`) by scanning for the `(...)` suffix.  Closures
and arrow functions assigned to variables are not detected because they
don't end with `(...)`.

#### Implementation

1. **Detect closure/arrow assignments** — in
   `extract_callable_target_from_variable`, if the RHS does not end with
   `(...)`, check whether it starts with `function(` or `fn(`.  If so,
   return a synthetic identifier (e.g. `"__closure_at_L{line}"`) that
   the resolver can look up.

2. **Parse closure parameters** — alternatively, skip the
   `resolve_callable_target` pathway entirely.  When the variable is
   assigned a closure/arrow function, parse the parameters and return
   type directly from the AST of the assignment RHS.  Build the
   `ResolvedCallableTarget` inline without going through class
   resolution.

   This is the cleaner approach: closures don't have classes, so the
   existing class-based resolution is the wrong abstraction.  The
   `SymbolMap` already records `VarDefSite` for the assignment, and the
   AST is available.

3. **Label prefix** — use `$format` (the variable name) or the closure's
   inferred signature as the label prefix.

#### Tests

- Integration test: `$fn = fn(string $x): int => 0; $fn(` → assert
  signature help shows `string $x` with return type `int`.
- Integration test: `$fn = function(int $a, int $b): int { ... }; $fn('x', ` →
  assert `active_parameter` is 1.
- Integration test: `$fn = $obj->method(...)` (existing first-class
  callable path) → continues to work unchanged.

---

<!-- ============================================================ -->
<!--  TIER 3 — POLISH                                             -->
<!-- ============================================================ -->

## Tier 3 — Polish

### 7. Multiple overloaded signatures
**Impact: Low · Effort: Medium-High**

Some PHP functions have multiple signatures depending on argument count
or types.  For example, `array_map` can be called as:

```php
array_map(callable $callback, array $array): array
array_map(null, array ...$arrays): array
```

The LSP protocol supports returning multiple `SignatureInformation`
entries with an `activeSignature` index.  Today we return a single
signature.

#### Current state

phpstorm-stubs define multiple function entries (or parameter variants
annotated with `#[PhpStormStubsElementAvailable]`) for overloaded
functions.  Our PHP-version filtering selects one variant.  We don't
model true overloads.

#### Implementation

This is a deeper change:

1. When a function has multiple stub entries (or when a class has
   multiple `__construct` signatures for different PHP versions),
   collect all applicable signatures.
2. Return them all in the `signatures` array.
3. Set `activeSignature` based on argument-count matching: pick the
   first signature whose parameter count accommodates the current
   argument count.

**Deferred** — the single-signature approach covers 99% of real usage.

---

### 8. Named argument awareness in active parameter
**Impact: Low · Effort: Medium**

When the user types a named argument (`callback: ` in `array_map(callback: `),
the active parameter should highlight the `$callback` parameter regardless
of its positional index.

#### Current state

Active parameter is computed purely by counting commas before the cursor.
Named arguments are handled by the named-argument completion system
(`completion/named_args.rs`) but the signature help active-parameter
tracking doesn't consult argument names.

#### Implementation

1. In `detect_call_site_from_map`, after computing the comma-based
   `active` index, extract the text of the current argument segment.
2. If the segment matches `identifier:` (named argument syntax), look up
   which parameter index corresponds to that name.
3. Override `active_parameter` with the named parameter's index.

This requires access to the resolved parameters (to map name → index),
which isn't available in the detection layer.  The override could be
applied later in `resolve_signature`, after `resolve_callable` returns
the parameter list.

---

## Summary

| # | Item | Impact | Effort | Data Ready | Target |
|---|---|---|---|---|---|
| 4 | Attribute constructor sig help | Medium | Medium | ❌ | Sprint 2 |
| 5 | Closure/arrow function sig help | Medium | Medium | ❌ | Sprint 2 |
| 7 | Multiple overloaded signatures | Low | Medium-High | ❌ | Backlog |
| 8 | Named argument active parameter | Low | Medium | ❌ | Backlog |

---

## 9. Language construct signature help and hover
**Impact: Low · Effort: Low**

PHP language constructs that use parentheses (`unset()`, `isset()`, `empty()`,
`eval()`, `exit()`, `die()`, `print()`, `list()`) are not function calls in the
AST. Mago parses them as dedicated statement/expression nodes (e.g.
`Statement::Unset`) with no `ArgumentList`, so no `CallSite` is emitted and
neither signature help nor hover fires inside their parentheses. The phpstorm-stubs
don't define them either since they are keywords, not functions.

Supporting them requires emitting synthetic `CallSite` entries from the
statement-level extraction in `symbol_map.rs` and adding hardcoded parameter
metadata (e.g. `unset(mixed ...$vars): void`) in `resolve_callable`. Hover would
need a similar hardcoded lookup.