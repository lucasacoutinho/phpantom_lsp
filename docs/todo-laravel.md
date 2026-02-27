# PHPantom — Laravel Support: Remaining Work

> Last updated: 2026-02-27

This document tracks bugs, known gaps, and missing features in
PHPantom's Laravel Eloquent support. For the general architecture and
virtual member provider design, see `ARCHITECTURE.md`.

---

## Out of scope (and why)

| Item | Reason |
|------|--------|
| Container string aliases | Requires booting the application. Use `::class` references instead. |
| Facade `getFacadeAccessor()` with string aliases | Same problem. `@method` tags provide a workable fallback. |
| Blade templates | Large scope, separate project. |
| Model column types from DB/migrations | Unreasonable complexity. Require `@property` annotations (via ide-helper or hand-written). |
| Legacy Laravel versions | We target current Larastan-style annotations. Older code may degrade gracefully. |
| Application provider scanning | Low-value, high-complexity. |

---

## Philosophy (unchanged)

- **No application booting.** We never boot a Laravel application to
  resolve types.
- **No SQL/migration parsing.** Model column types are not inferred from
  database schemas or migration files.
- **Larastan-style hints preferred.** We expect relationship methods to be
  annotated in the style that Larastan expects. Fallback heuristics
  are best-effort.
- **Facades fall back to `@method`.** Facades whose `getFacadeAccessor()`
  returns a string alias cannot be resolved. `@method` tags on facade
  classes provide completion without template intelligence.

---

## Model property source gaps

The `LaravelModelProvider` synthesizes virtual properties from several
sources on Eloquent models. The table below summarises what we handle
today and what is still missing.

### What we cover

| Source | Type info | Notes |
|--------|-----------|-------|
| `$casts` / `casts()` | Rich (built-in map, custom cast `get()` return type, enum, `Castable`, `CastsAttributes<TGet>` generics fallback) | |
| `$attributes` defaults | Literal type inference (string, bool, int, float, null, array) | Fallback when no `$casts` entry |
| `$fillable`, `$guarded`, `$hidden` | `mixed` | Last-resort column name fallback |
| Legacy accessors (`getXAttribute()`) | Method's return type | |
| Modern accessors (returns `Attribute`) | Always `mixed` | **See gap 1 below** |
| Relationship methods | Generic params or body inference | |

### Gaps (ranked by impact)

#### 1. Modern accessor `Attribute<TGet>` generic extraction

Modern accessors (Laravel 9+) return `Illuminate\Database\Eloquent\Casts\Attribute`.
We detect these correctly and synthesize a virtual property, but the
property is always typed `mixed`. When the return type carries a generic
argument (e.g. `Attribute<string>` or `Attribute<string, never>` via
`@return` or a native return type), we should extract the first generic
parameter and use it as the property type.

```php
// @return Attribute<string>
protected function firstName(): Attribute { ... }
// Expected: $first_name typed as `string`
// Actual:   $first_name typed as `mixed`
```

This is the most impactful gap because modern accessors are the
recommended approach since Laravel 9.

**Where to change:** `is_modern_accessor` already strips generics to
match the base type. A companion function (or inline logic in `provide`)
should extract the first generic arg from the return type string via
`parse_generic_args` and pass it through instead of hard-coding `mixed`.

#### 2. `$visible` array not included in column name extraction

The `$visible` property lists attribute names that should appear in
serialized output. It functions identically to `$fillable`/`$guarded`/
`$hidden` as a source of column names.

**Where to change:** Add `"visible"` to the `targets` array in
`extract_column_names` in `parser/classes.rs`.

#### 3. `$dates` array (deprecated)

Before `$casts`, Laravel used `protected $dates = [...]` to mark
columns as Carbon instances. This was deprecated in favour of
`casts()` with a `datetime` type, but older codebases still use it.
Columns listed in `$dates` should be typed as `\Carbon\Carbon`.

**Where to change:** Add a new `extract_dates_definitions` function in
`parser/classes.rs` (similar to `extract_column_names` but returning
`Vec<(String, String)>` with each column mapped to `\Carbon\Carbon`).
Merge these into `casts_definitions` at a lower priority than explicit
`$casts` entries, or add a separate field on `ClassInfo` and handle
priority in the provider.

#### 4. `$appends` array

The `$appends` property lists accessor names that should always be
included in `toArray()` / `toJson()`. These reference existing
accessors, so in most cases the accessor method itself already produces
the virtual property. Parsing `$appends` would only help when the
accessor is defined in an unloaded parent class.

**Priority:** Low. The accessor method is the real source of truth.