# PHPantom — Laravel Support: Remaining Work

> Last updated: 2026-02-28

This document tracks bugs, known gaps, and missing features in
PHPantom's Laravel Eloquent support. For the general architecture and
virtual member provider design, see `ARCHITECTURE.md`.

---

## Out of scope (and why)

| Item | Reason |
|------|--------|
| Container string aliases | Requires booting the application. Use `::class` references instead. |
| Facade `getFacadeAccessor()` with string aliases | Same problem. `@method` tags provide a workable fallback. |
| Blade templates | Separate project. See `todo-blade.md` for the implementation plan. |
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

#### 5. `*_count` relationship count properties

Accessing `$user->posts_count` is a very common Laravel pattern
(`withCount`, `loadCount`, or eager-loaded counts). We don't
synthesize these today.

```php
$user->posts_count; // int, but we know nothing about it
```

Larastan handles this **declaratively** — no call-site tracking
required.  When a property name ends with `_count`, it strips the
suffix, checks whether the remainder (converted to camelCase) is a
relationship method, and if so types the property as `int`.

**Where to change:** In `LaravelModelProvider::provide`, after
synthesizing relationship properties, iterate the relationship methods
again and push a `{snake_name}_count` property typed as `int` for
each one.  The property should have lower priority than explicit
`@property` tags.

**Priority:** Medium.  Simple to implement using the Larastan
approach and covers a very common pattern.

#### 6. `withSum()` / `withAvg()` / `withMin()` / `withMax()` aggregate properties

Similar to `withCount`, these aggregate methods produce virtual
properties named `{relation}_{function}` (e.g.
`Order::withSum('items', 'price')` → `$order->items_sum`). The same
call-site tracking challenge applies, and the type depends on the
aggregate function (`withSum`/`withAvg` → `float`,
`withMin`/`withMax` → `mixed`).

**Priority:** Low. Unlike gap 5, these can't be inferred declaratively
from the model alone — you'd need to track call-site string arguments.
The `@property` workaround applies here too.

#### 7. `$pivot` property on BelongsToMany related models

When a model is accessed through a `BelongsToMany` (or `MorphToMany`)
relationship, each related model instance gains a `$pivot` property at
runtime that provides access to intermediate table columns.

```php
/** @return BelongsToMany<Role, $this> */
public function roles(): BelongsToMany {
    return $this->belongsToMany(Role::class)->withPivot('expires_at');
}

$user->roles->first()->pivot;           // Pivot instance — we know nothing about it
$user->roles->first()->pivot->expires_at; // accessible at runtime, invisible to us
```

There are several layers of complexity here:

1. **Basic `$pivot` property.** Related models accessed through a
   `BelongsToMany` or `MorphToMany` relationship should have a `$pivot`
   property typed as `\Illuminate\Database\Eloquent\Relations\Pivot`
   (or the custom pivot class when `->using(CustomPivot::class)` is
   used). We don't currently synthesize this property at all.

2. **`withPivot()` columns.** The `withPivot('col1', 'col2')` call
   declares which extra columns are available on the pivot object.
   Tracking these requires parsing the relationship method body for
   chained `withPivot` calls — similar in difficulty to the
   `withCount` call-site problem (gap 5).

3. **Custom pivot models (`using()`).** When `->using(OrderItem::class)`
   is declared, the pivot is an instance of that custom class, which
   may have its own properties, casts, and accessors. Detecting this
   requires parsing the `->using()` call in the relationship body.

Note: Larastan does **not** handle pivot properties either — the
`$pivot` property comes from Laravel's own `@property` annotations on
the `BelongsToMany` relationship stubs. If the user's stub set
includes these annotations, it already works through our PHPDoc
provider.

**Priority:** Low-medium. The basic `$pivot` typed as `Pivot` (layer 1)
would be a modest improvement. Layers 2–3 require relationship body
parsing that we don't currently do for this purpose. The `@property`
workaround on a custom Pivot class covers most real-world needs.

#### 8. Custom Eloquent builders (`HasBuilder` / `#[UseEloquentBuilder]`)

Laravel 11+ introduced the `HasBuilder` trait and
`#[UseEloquentBuilder(UserBuilder::class)]` attribute to let models
declare a custom builder class. When present, `User::query()` and
all static builder-forwarded calls should resolve to the custom
builder instead of the base `Illuminate\Database\Eloquent\Builder`.

```php
/** @extends Builder<User> */
class UserBuilder extends Builder {
    /** @return $this */
    public function active(): static { ... }
}

class User extends Model {
    /** @use HasBuilder<UserBuilder> */
    use HasBuilder;
}

User::query()->active()->get(); // active() should resolve on UserBuilder
```

Larastan handles this via `BuilderHelper::determineBuilderName()`,
which inspects `newEloquentBuilder()`'s return type or the
`#[UseEloquentBuilder]` attribute to find the custom builder class.

**Where to change:** In `build_builder_forwarded_methods`, before
loading the standard `Eloquent\Builder`, check whether the model
declares a custom builder via `@use HasBuilder<X>` in `use_generics`
or a `newEloquentBuilder()` method with a non-default return type.
If found, load and resolve that builder class instead.

**Priority:** Medium. Custom builders are the recommended pattern
for complex query scoping in modern Laravel and Larastan supports
them. Without this, users of custom builders get no completions for
their builder-specific methods via static calls on the model.

#### 9. `#[Scope]` attribute (Laravel 11+)

Laravel 11 introduced the `#[Scope]` attribute as an alternative to
the `scopeX` naming convention. Methods decorated with `#[Scope]`
are available on the builder without needing the `scope` prefix:

```php
class User extends Model {
    #[Scope]
    protected function active(Builder $query): void { ... }
}

User::active()->get(); // works at runtime via #[Scope]
```

Larastan checks for this attribute in `BuilderHelper::searchOnEloquentBuilder()`.
We currently only detect scopes via the `scopeX` naming convention in
`is_scope_method`.

**Where to change:** In the parser, extract `#[Scope]` attributes
from method declarations. In `LaravelModelProvider::provide`, treat
methods with the `#[Scope]` attribute the same as `scopeX` methods
(strip the first `$query` parameter, expose as both static and
instance virtual methods).

**Priority:** Low-medium. The `scopeX` convention still works and
is more common. The `#[Scope]` attribute is newer and adoption is
growing.

#### 10. Higher-order collection proxies

Laravel collections support higher-order proxies via magic properties
like `$users->map->name` or `$users->filter->isActive()`. These
produce a `HigherOrderCollectionProxy` that delegates property
access / method calls to each item in the collection.

```php
$users->map->email;           // Collection<int, string>
$users->filter->isVerified(); // Collection<int, User>
$users->each->notify();       // void (side-effect)
```

Larastan handles this with `HigherOrderCollectionProxyPropertyExtension`
and `HigherOrderCollectionProxyExtension`, which resolve the proxy's
template types and delegate property/method lookups to the collection's
value type.

**Priority:** Low. This is a convenience syntax and most users use
closures instead. Requires synthesizing virtual properties on
collection classes that return a proxy type parameterised with the
collection's value type.

#### 11. `collect()` and other helper functions lose generic type info

Laravel's `collect()` helper is annotated with function-level
`@template` parameters:

```php
/**
 * @template TKey of array-key
 * @template TValue
 * @param array<TKey, TValue> $value
 * @return \Illuminate\Support\Collection<TKey, TValue>
 */
function collect($value = []) { ... }
```

We correctly resolve the return type as `Collection`, but the
generic arguments `TKey` and `TValue` are lost — the result is an
unparameterised `Collection`, so `$users = collect($array)` followed
by `$users->first()->` produces no completions for the element type.

**Root cause:** `FunctionInfo` has no `template_params` or
`template_bindings` fields (unlike `MethodInfo`, which has both).
The `synthesize_template_conditional` function only handles the
narrow pattern `@return T` where `T` is a bare template param bound
via `@param class-string<T>`.  It does **not** handle `@return
Collection<TKey, TValue>` where multiple template params appear
inside a generic return type.

This affects every Laravel helper that uses function-level generics:
`collect()`, `value()`, `retry()`, `tap()`, `with()`, `transform()`,
`data_get()`, plus non-Laravel functions with the same pattern.

**Where to change:** Add `template_params: Vec<String>` and
`template_bindings: Vec<(String, String)>` to `FunctionInfo` (mirror
the existing fields on `MethodInfo`).  Populate them in
`parser/functions.rs` from `@template` and `@param` annotations.
In `resolve_rhs_function_call` (in `variable_resolution.rs`), after
loading the `FunctionInfo`, build a substitution map from template
bindings → call-site argument types and apply it to the return type
before passing it to `type_hint_to_classes`.  See the general TODO
item (§ PHP Language Feature Gaps, "Function-level `@template`
generic resolution") for the full implementation plan.

**Priority:** Medium-high.  `collect()` alone is used in virtually
every Laravel codebase, and the loss of element types breaks
completion chains on the resulting collection.

#### 12. `$this` in inferred callable parameter types resolves to wrong class

When a closure parameter is untyped and the inference system extracts
callable param types from the called method's signature, `$this` in
the extracted type resolves to the **calling class** (the class
containing the user's code) instead of the class that declares the
method.

```php
// Builder::when() signature (from Conditionable trait):
// @param callable($this, mixed): $this $callback

// In a controller:
User::when($active, function ($query) {
    $query->  // $query inferred as Controller, not Builder<User>
});
```

The callable param types are extracted as raw strings by
`extract_callable_param_types`.  When `$this` appears in these
strings, `resolve_closure_params_with_inferred` passes them to
`type_hint_to_classes`, which resolves `$this` relative to
`ctx.current_class` — the class the user is editing, not the class
that owns the method.

In practice, most users type-hint the closure parameter explicitly
(`function (Builder $query) { ... }`), which bypasses the inference
entirely.  The gap only manifests for untyped closure params.

**Where to change:** In `infer_callable_params_from_receiver` (and
the static variant), after extracting callable param types, replace
any literal `$this` or `static` tokens with the FQN of the receiver
class before returning them.  This ensures the inferred types
reference the declaring class rather than the calling class.

**Priority:** Low.  The explicit type-hint workaround is standard
practice, and most IDE-aware codebases already type their closure
parameters.