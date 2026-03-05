# PHPantom — Type Inference & Resolution

This document covers type resolution gaps: generic resolution, conditional
return types, type narrowing, PHP version features, and stub attribute
handling. Items that are purely about *completion UX* or *stub metadata
extraction* live in [todo-completion.md](todo-completion.md).

Items are ordered by **impact** (descending), then **effort** (ascending)
within the same impact tier.

| Label | Scale |
|---|---|
| **Impact** | **Critical**, **High**, **Medium-High**, **Medium**, **Low-Medium**, **Low** |
| **Effort** | **Low** (≤ 1 day), **Medium** (2-5 days), **Medium-High** (1-2 weeks), **High** (2-4 weeks), **Very High** (> 1 month) |

---

## 1. Pipe operator (PHP 8.5)
**Impact: High · Effort: Low**

PHP 8.5 introduced the pipe operator (`|>`):

```php
$result = $input
    |> htmlspecialchars(...)
    |> strtoupper(...)
    |> fn($s) => "<b>$s</b>";
```

The mago parser already produces `Expression::Pipe` nodes, and the
closure resolution module walks into pipe sub-expressions to find
closures. However, **no type resolution** is performed for the pipe
result — the RHS callable's return type is never resolved, so
`$result->` after a pipe chain produces no completions.

**Fix:** In `resolve_rhs_expression`, add a `Expression::Pipe` arm
that resolves the RHS callable (function reference, closure, or
arrow function) and returns its return type. For first-class
callable syntax (`htmlspecialchars(...)`), reuse the existing
`extract_first_class_callable_return_type` logic.

---

## 2. Function-level `@template` generic resolution
**Impact: High · Effort: Medium**

`MethodInfo` has `template_params` and `template_bindings` fields that
enable method-level generic resolution at call sites (e.g.
`@template T` + `@param class-string<T> $class` + `@return T`).
`FunctionInfo` has **neither** field, so standalone functions that use
`@template` lose their generic type information entirely.

The only function-level template handling today is
`synthesize_template_conditional` in `parser/functions.rs`, which
converts the narrow `@return T` + `@param class-string<T>` pattern
into a `ConditionalReturnType`.  This does not cover the general case
where template params appear inside a generic return type:

```php
/**
 * @template TKey of array-key
 * @template TValue
 * @param  array<TKey, TValue>  $value
 * @return \Illuminate\Support\Collection<TKey, TValue>
 */
function collect($value = []) { ... }

/**
 * @template TValue
 * @param  TValue  $value
 * @param  callable(TValue): TValue  $callback
 * @return TValue
 */
function tap($value, $callback = null) { ... }
```

After `$users = collect($userArray)`, the result is an unparameterised
`Collection` — element type information is lost, and
`$users->first()->` produces no completions.

This affects Laravel helpers (`collect`, `value`, `retry`, `tap`,
`with`, `transform`, `data_get`), PHPStan/Psalm helper libraries, and
any userland function using `@template`.

### Implementation plan

1. **Add fields to `FunctionInfo`** (in `types.rs`):

   ```text
   pub template_params: Vec<String>,
   pub template_bindings: Vec<(String, String)>,
   ```

   Mirror the existing `MethodInfo` fields.

2. **Populate during parsing** (in `parser/functions.rs`):

   Extract `@template` tags via `extract_template_params` and
   `@param`-based bindings via `extract_template_param_bindings`,
   the same functions already used for method-level templates in
   `parser/classes.rs`.

3. **Resolve at call sites** (in `variable_resolution.rs`,
   `resolve_rhs_function_call`):

   After loading the `FunctionInfo` and before falling through to
   `type_hint_to_classes`, check whether the function has
   `template_params`.  If so:

   a. For each template param, try to infer the concrete type from
      the call-site arguments using `template_bindings` (positional
      match between `$paramName` and the `ArgumentList`).  Reuse
      the existing `resolve_rhs_expression` / `type_hint_to_classes`
      to get the argument's type.

   b. Build a substitution map `{T => ConcreteType, ...}`.

   c. Apply the substitution to `return_type` via
      `apply_substitution` before passing it to
      `type_hint_to_classes`.

   This mirrors what `apply_generic_args` does for class-level
   templates, but applied to a function call.

4. **Text-based path** (in `text_resolution.rs`):

   The inline chain resolver (`resolve_raw_type_from_call_chain`)
   also needs the same treatment for chains like
   `collect($arr)->first()->`.  After resolving the function's
   return type, apply template substitution before continuing the
   chain.

**Note:** The existing `synthesize_template_conditional` path for
`@return T` + `@param class-string<T>` should be kept as-is — it
handles the special case where the return type is the template param
itself, which is common for container-style `resolve()`/`app()`
functions.  The new general path handles the remaining cases.

See also: `todo-laravel.md` gap 5 (`collect()` and other helper
functions lose generic type info), which is the Laravel-specific
manifestation of this gap.

---

## 3. Parse and resolve `($param is T ? A : B)` return types
**Impact: High · Effort: Medium**

PHPStan's stubs use conditional return type syntax in docblocks:

```php
/**
 * @return ($value is string ? true : false)
 */
function is_string(mixed $value): bool {}
```

This is the mechanism behind all `is_*` function type narrowing in
PHPStan — the stubs declare the conditional, and the type specifier
reads it.  If we parse this syntax from stubs and `@return` tags, we
get type narrowing for `is_string`, `is_int`, `is_array`,
`is_object`, `is_null`, `is_bool`, `is_float`, `is_numeric`,
`is_scalar`, `is_callable`, `is_iterable`, `is_countable`, and
`is_resource` from annotations alone, without any hard-coded function
list.

The syntax also appears in userland code (PHPStan and Psalm both
support it), and in array function stubs:

```php
/**
 * @return ($array is non-empty-array ? non-empty-list<T> : list<T>)
 */
function array_values(array $array): array {}
```

**Implementation:** Extend the return type parser in
`docblock/types.rs` to recognise the `($paramName is Type ? A : B)`
pattern.  At call sites, check the argument's type against the
condition and select the appropriate branch.  This could reuse or
extend the existing `ConditionalReturnType` infrastructure.

---

## 4. Inherited docblock type propagation
**Impact: High · Effort: Medium**

When a child class overrides a method from a parent class or interface,
the ancestor's richer docblock types should flow down unconditionally.
Inheritance is the default — if the ancestor says `@return list<Pen>`
and the child just says `: array`, the resolved return type must be
`list<Pen>`. There is no opt-in; `@inheritDoc` is functionally
meaningless because a child that can run code already has the parent's
contract. The only thing that *blocks* inheritance is the child
providing its own docblock type that is equally or more specific.

**Example:**

```php
interface PenHolder {
    /** @return list<Pen> */
    public function getPens(): array;
}

class Drawer implements PenHolder {
    // No docblock — native return type is just `array`.
    public function getPens(): array { return []; }
}

$d = new Drawer();
$d->getPens()[0]-> // ← should complete Pen members
```

Today `Drawer::getPens()` resolves to `return_type: "array"` because
the method's own docblock has no `@return` tag and the native hint is
`array`. The interface's `@return list<Pen>` is never consulted.

**Root cause:** `resolve_class_with_inheritance` (inheritance.rs L155)
and `resolve_class_fully_inner` (virtual_members/mod.rs L360) both
skip a parent/interface method when the child already declares one
with the same name — the child wins unconditionally. No fallback
check compares the richness of the return type.

**What needs to change:**

1. **During inheritance merging** (`resolve_class_with_inheritance`):
   when the child already has a method with the same name, don't
   just skip — enrich it. If the child's `return_type` equals its
   `native_return_type` (i.e. no docblock refined it) and the
   ancestor's `return_type` differs from its `native_return_type`
   (i.e. the ancestor *does* have a richer docblock type), copy the
   ancestor's `return_type` onto the child's method. Do the same
   for each parameter's `type_hint` when the child's matches its
   `native_type_hint`. Also inherit `description` and
   `return_description` when the child lacks them.

2. **During interface merging** (`resolve_class_fully_inner`): same
   logic — when an interface method is skipped because the class
   already defines it, enrich the existing method with the
   interface's richer types and descriptions.

3. **Child docblock wins when present.** If the child provides its
   own `@return` or `@param` type (even if less specific), that is
   an intentional override and the ancestor type is not propagated.
   The test is simple: does the child's effective type differ from
   its native type? If yes, the child wrote a docblock — respect it.

**Scope of the fix:** This affects completion (return type drives
chain resolution), hover (return type displayed), and signature help
(parameter types shown). All three automatically benefit once the
merged `MethodInfo` carries the richer type.

**Properties too:** The same pattern applies to properties. An
interface declaring `@property-read list<Pen> $pens` should
propagate to an implementing class's `$pens` property if the class
only has a native `array` type hint.

---

## 5. File system watching for vendor and project changes
**Impact: Medium-High · Effort: Medium**

PHPantom loads Composer artifacts (classmap, PSR-4 mappings, autoload
files) once during `initialized` and caches them for the session. If
the user runs `composer update`, `composer require`, or `composer remove`
while the editor is open, the cached data goes stale. The user gets
completions and go-to-definition based on the old package versions
until they restart the editor.

### What to watch

| Path | Trigger | Action |
|---|---|---|
| `vendor/composer/autoload_classmap.php` | Changed | Reload classmap |
| `vendor/composer/autoload_psr4.php` | Changed | Reload PSR-4 mappings |
| `vendor/composer/autoload_files.php` | Changed | Re-scan autoload files for global functions/constants |
| `composer.json` | Changed | Reload project PSR-4 prefixes, re-check vendor dir |
| `composer.lock` | Changed | Good secondary signal that packages changed |

All three `autoload_*.php` files are rewritten atomically by Composer
on every `install`, `update`, `require`, `remove`, and `dump-autoload`.
Watching these is sufficient to catch any package change.

### Implementation options

1. **LSP `workspace/didChangeWatchedFiles`** — register file watchers
   via `client/registerCapability` during `initialized`. The editor
   handles the OS-level watching and sends notifications. This is the
   cleanest approach and works cross-platform. Register glob patterns
   for the vendor Composer files and `composer.json`.

2. **Server-side `notify` crate** — use the `notify` Rust crate to
   watch the file system directly. More control but adds a dependency
   and duplicates what the editor already provides.

Option 1 is preferred. The LSP spec's `DidChangeWatchedFilesRegistrationOptions`
supports glob patterns like `**/vendor/composer/autoload_*.php`.

### Reload strategy

- On change notification, re-run the same parsing logic from
  `initialized` for the affected artifact.
- Invalidate `class_index` entries that came from vendor files (their
  parsed AST may have changed).
- Clear and re-populate `classmap` from the new `autoload_classmap.php`.
- Log the reload to the output panel so the user knows it happened.
- Debounce rapid changes (Composer writes multiple files in sequence)
  with a short delay (e.g. 500ms) to avoid redundant reloads.

---

## 6. Property hooks (PHP 8.4)
**Impact: Medium · Effort: Medium**

PHP 8.4 introduced property hooks (`get` / `set`):

```php
class User {
    public string $name {
        get => strtoupper($this->name);
        set => trim($value);
    }
}
```

The mago parser (v1.8) already produces `Property::Hooked` and
`PropertyHook` AST nodes, and the generic `.modifiers()`, `.hint()`,
`.variables()` methods mean hooked properties are extracted for basic
completion. However:

- **Hook bodies are never walked.** Variables and anonymous classes
  inside `get`/`set` bodies are invisible to resolution.
- **`$value` parameter** inside `set` hooks is not offered for
  variable completion.
- **Asymmetric visibility** (`public private(set) string $name`) is
  not recognised — the `set` visibility is ignored, so filtering
  may incorrectly allow setting a property that should be
  write-restricted.

**Fix:** In `extract_class_like_members`, match `Property::Hooked`
explicitly, walk hook bodies for anonymous classes and variable
scopes, and parse the set-visibility modifier into a new
`set_visibility` field on `PropertyInfo`.

---

## 7. Narrow types of `&$var` parameters after function calls
**Impact: Medium · Effort: Medium**

When a function takes a parameter by reference, the variable's type
after the call should reflect what the function writes to it.  PHPStan
has `FunctionParameterOutTypeExtension` for this.

Key examples:

| Function | Parameter | Type after call |
|---|---|---|
| `preg_match($pattern, $subject, &$matches)` | `$matches` | Typed array shape based on the regex |
| `preg_match_all($pattern, $subject, &$matches)` | `$matches` | Nested typed array based on the regex |
| `parse_str($string, &$result)` | `$result` | `array<string, string>` |
| `sscanf($string, $format, &...$vars)` | `$vars` | Types based on format specifiers |

The most impactful case is `preg_match` — PHPStan's
`RegexArrayShapeMatcher` parses the regex pattern to produce a precise
array shape for `$matches`, including named capture groups.  A simpler
first step would be to just type `$matches` as `array<int|string,
string>` when passed to `preg_match`.

**Implementation:** When resolving a variable's type after a function
call where the variable was passed by reference, look up the
function's parameter annotations for `@param-out` tags (PHPStan/Psalm
extension) or use a built-in map for known functions.

---

## 8. SPL iterator generic stubs
**Impact: Medium · Effort: Medium**

PHPStan's `iterable.stub` provides full `@template TKey` /
`@template TValue` annotations for the entire SPL iterator hierarchy:
`ArrayIterator`, `FilterIterator`, `LimitIterator`,
`CachingIterator`, `RegexIterator`, `NoRewindIterator`,
`InfiniteIterator`, `AppendIterator`, `IteratorIterator`,
`RecursiveIteratorIterator`, `CallbackFilterIterator`, and more.

This means `foreach` over any SPL iterator properly resolves element
types.  If we rely on php-stubs for these classes, we are almost
certainly missing these generic annotations.  We should either:

- Ship our own stub overlays for the SPL iterator classes with
  `@template` annotations (like PHPStan does), or
- Detect and use PHPStan's stubs when present in the project's
  vendor directory.

---

## 9. Asymmetric visibility (PHP 8.4)
**Impact: Low-Medium · Effort: Low**

Separate from property hooks, PHP 8.4 allows asymmetric visibility on
plain and promoted properties. PHP 8.5 extended this to static
properties as well.

```php
class Settings {
    public private(set) string $name;

    public function __construct(
        public protected(set) int $retries = 3,
    ) {}
}
```

PHPantom currently extracts a single `Visibility` per property.
Completion filtering uses this to decide whether a property should
appear. A `public private(set)` property should appear for reading
from outside the class but not for assignment contexts.

**Fix:** Add an optional `set_visibility: Option<Visibility>` to
`PropertyInfo`. Populate it from the AST modifier list (the parser
exposes the set-visibility keyword). Completion filtering does not
currently distinguish read vs write contexts, so the immediate fix
is just to store the value; context-aware filtering can follow later.

---

## 10. `str_contains` / `str_starts_with` / `str_ends_with` → non-empty-string narrowing
**Impact: Low-Medium · Effort: Low**

When `str_contains($haystack, $needle)` appears in a condition and
`$needle` is known to be a non-empty string, PHPStan narrows
`$haystack` to `non-empty-string`.  The same applies to
`str_starts_with`, `str_ends_with`, `strpos`, `strrpos`, `stripos`,
`strripos`, `strstr`, and the `mb_*` equivalents.

This is lower priority for an LSP because `non-empty-string` does
not directly enable class-based completion, but it would improve
hover type display and catch bugs if we ever add diagnostics.

See `StrContainingTypeSpecifyingExtension` in PHPStan.

---

## 11. `count` / `sizeof` comparison → non-empty-array narrowing
**Impact: Low-Medium · Effort: Low**

`if (count($arr) > 0)` or `if (count($arr) >= 1)` narrows `$arr` to
a non-empty-array.  PHPStan handles a full matrix of comparison
operators and integer range types against `count()` / `sizeof()` calls.

See `CountFunctionTypeSpecifyingExtension` and the count-related
branches in `TypeSpecifier::specifyTypesInCondition`.

---

## 12. Fiber type resolution
**Impact: Low · Effort: Low**

`Generator<TKey, TValue, TSend, TReturn>` has dedicated support for
extracting each type parameter (value type for foreach, send type
for `$var = yield`, return type for `getReturn()`). `Fiber` has no
equivalent handling — `Fiber::start()`, `Fiber::resume()`, and
`Fiber::getReturn()` don't resolve their generic types.

PHP userland rarely annotates Fiber with generics (unlike Generator),
so this is low priority. If demand appears, the fix would mirror the
Generator extraction in `docblock/types.rs`.

---

## 13. Non-empty-string propagation through string functions
**Impact: Low · Effort: Low**

PHPStan tracks `non-empty-string` through string-manipulating
functions.  If you pass a `non-empty-string` to `addslashes()`,
`urlencode()`, `htmlspecialchars()`, `escapeshellarg()`,
`escapeshellcmd()`, `preg_quote()`, `rawurlencode()`, or
`rawurldecode()`, the return type is also `non-empty-string`.

This is low priority for an LSP because the narrower string type
does not directly enable class-based completion.  However, if we
ever add hover type display or diagnostics, this information
would improve accuracy.

See `NonEmptyStringFunctionsReturnTypeExtension` in PHPStan.

---

## 14. `Closure::bind()` / `Closure::fromCallable()` return type preservation
**Impact: Low · Effort: Low-Medium**

Variables holding closure literals, arrow functions, and first-class
callables now resolve to the `Closure` class, so `$fn->bindTo()`,
`$fn->call()`, etc. offer completions.  The remaining gap is
*preserving the closure's callable signature* through `Closure::bind()`
and resolving `Closure::fromCallable('functionName')` to the actual
function's signature as a typed `Closure`.  This is relevant for DI
containers and middleware patterns but is a niche use case.

See `ClosureBindDynamicReturnTypeExtension` and
`ClosureFromCallableDynamicReturnTypeExtension` in PHPStan.

---

## 15. `@param-closure-this`
**Impact: Medium-High · Effort: Medium**

The `@param-closure-this` PHPDoc tag declares what `$this` refers to
inside a closure parameter. This is critical for frameworks like Laravel
where closures are commonly rebound via `Closure::bindTo()`:

```php
/**
 * @param-closure-this \Illuminate\Routing\Route $callback
 */
function group(Closure $callback): void;
```

Without this, `$this->` inside the closure body produces no completions.
Laravel's routing (`Route::group`), testing (`$this->get(...)` inside
test closures), and macro APIs all rely on closure rebinding.

**Implementation:**

1. **Docblock parsing** — recognise `@param-closure-this` in
   `docblock/tags.rs`. The tag format is
   `@param-closure-this TypeName $paramName`. Extract the type string
   and the parameter name it applies to.

2. **Store on `ParameterInfo`** — add an optional `closure_this_type:
   Option<String>` field to `ParameterInfo`. During function/method
   extraction, if the docblock contains `@param-closure-this` for a
   parameter, populate this field.

3. **Closure `$this` resolution** — when resolving `$this` inside a
   closure body, check whether the closure is passed as an argument to
   a function/method call. If so, resolve the receiving function,
   find the target parameter, and check for `closure_this_type`. If
   present, use that type instead of the lexical `$this` class.

4. **Interaction with `Closure::bind()`** — this tag is the static
   analysis equivalent of runtime `Closure::bindTo()`. The two
   features are complementary: `@param-closure-this` handles the
   common case where the rebinding happens inside the called function,
   while `Closure::bind()` support (§14) handles explicit user-side
   rebinding.

---

## 16. `key-of<T>` and `value-of<T>` resolution
**Impact: Medium · Effort: Medium**

PHPantom already parses `key-of<T>` and `value-of<T>` as type keywords
but does not resolve them to concrete types. When `T` is bound to a
concrete array type, these utility types should resolve:

- `value-of<array{a: string, b: int}>` → `string|int`
- `key-of<array{a: string, b: int}>` → `'a'|'b'`
- `value-of<array<string, User>>` → `User`
- `key-of<array<string, User>>` → `string`

These types commonly appear in PHPStan-typed libraries and in
`@template` constraints. For example:

```php
/**
 * @template T of array
 * @param T $array
 * @return value-of<T>
 */
function first(array $array): mixed;
```

**Implementation:** plug into the generic substitution pipeline in
`inheritance.rs` / `completion/types/resolution.rs`. After template
parameters are substituted with concrete types, detect `key-of<...>`
and `value-of<...>` wrappers and resolve them by inspecting the inner
type:

- If the inner type is an `array{...}` shape, collect the key or value
  types from the shape fields.
- If the inner type is `array<K, V>`, extract `K` or `V` directly.
- If the inner type is still an unresolved template parameter, leave
  it as-is (it may resolve later in the chain).

---

## 17. `@implements` generic resolution
**Impact: Medium-High · Effort: Medium**

When a class declares `@implements SomeInterface<ConcreteType>`, the
generic parameters should be substituted into inherited interface
methods. This works for `@extends` on classes but the `@implements`
path is not wired up.

**Example:**

```php
/** @template T */
interface Repository {
    /** @return T */
    public function find(int $id);
}

/** @implements Repository<User> */
class UserRepository implements Repository {
    public function find(int $id) { /* ... */ }
}

$repo = new UserRepository();
$repo->find(1); // should resolve to User
```

A related variant is `@implements` through an extended interface chain
with key+value types (e.g. `@implements IteratorAggregate<string, Item>`).

**Discovered via:** fixture conversion (class_implements_single,
class_implements_multiple, implements_parameter_type).

---

## 18. `@phpstan-assert` on static method calls
**Impact: Medium · Effort: Medium**

`@phpstan-assert` type guards only work on standalone function calls
today. When the assertion is on a static method (e.g.
`Assert::instanceOf($value, Foo::class)`), the narrowing does not
apply to the calling scope.

The same limitation applies to `@phpstan-assert-if-true` and
`@phpstan-assert-if-false` on static method calls.

**Discovered via:** fixture conversion (phpstan_assert_static).

---

## 19. Negated `@phpstan-assert !Type`
**Impact: Medium · Effort: Low-Medium**

When a function declares `@phpstan-assert !Foo $param`, calling it
should remove `Foo` from the variable's union type. Today the negation
prefix is not parsed, so the assertion is either ignored or
misinterpreted as a positive assertion.

**Discovered via:** fixture conversion (phpstan_assert_negated).

---

## 20. Generic `@phpstan-assert` with `class-string<T>` parameter inference
**Impact: Medium · Effort: Medium-High**

When a function declares `@phpstan-assert T $value` with a
`@template T` bound via a `class-string<T>` parameter, the narrowed
type should be inferred from the class-string argument at the call
site. For example:

```php
/**
 * @template T of object
 * @param class-string<T> $class
 * @phpstan-assert T $value
 */
function assertInstanceOf(string $class, mixed $value): void {}

assertInstanceOf(Foo::class, $x);
$x->fooMethod(); // $x should be narrowed to Foo
```

**Discovered via:** fixture conversion (phpstan_assert_generic).

---

## 21. Property-level narrowing
**Impact: Medium · Effort: Medium**

`$this->prop instanceof Foo` inside an `if` block does not narrow the
type of `$this->prop` for subsequent member access. Only local
variables participate in narrowing today.

**Discovered via:** fixture conversion (property_narrowing,
property_narrowing_negated, combination/property_instanceof).

---

## 22. Sequential `assert()` calls do not accumulate
**Impact: Low-Medium · Effort: Low**

Multiple `assert($x instanceof Foo); assert($x instanceof Bar);`
statements on the same variable should accumulate. Today only the
last assertion's narrowing applies.

**Discovered via:** fixture conversion (sequential_narrowing).

---

## 23. Double negated `instanceof` narrowing
**Impact: Low · Effort: Low**

`if (!!($x instanceof Foo))` and `if (!(!$x instanceof Foo) { return; }`
do not resolve correctly. The double negation cancels out but the
narrowing engine does not simplify it.

**Discovered via:** fixture conversion (bangbang_instanceof).

---

## 24. Literal string conditional return type
**Impact: Low · Effort: Low-Medium**

Conditional return types using literal string comparison
(`$param is "foo"`) are not resolved. Only class/interface type checks
work in the conditional return type parser today.

**Discovered via:** fixture conversion (conditional_return_type_string).

---

## 25. `class-string<T>` on interface method not inherited
**Impact: Medium · Effort: Medium**

When an interface method uses `class-string<T>` in its return type
and a class implements that interface, the implementing class's method
does not inherit the generic return type. The `class-string<T>`
pattern works on the interface directly but is lost during
inheritance merging.

**Discovered via:** fixture conversion (class_string_generic_interface).

---

## 26. `@method` with `static` or `$this` return type on parent class
**Impact: Medium · Effort: Low-Medium**

When a parent class declares `@method static foo()` or
`@method $this bar()`, calling the method on a child class should
return the child class type. Today the virtual method's return type
is not rewritten through the inheritance chain.

**Discovered via:** fixture conversion (virtual_member/method_returns_static,
virtual_member/method_returns_this).

---

## 27. `new $classStringVar` and `$classStringVar::staticMethod()`
**Impact: Low-Medium · Effort: Medium**

When a variable holds a `class-string<T>`, `new $var` should resolve
to `T` and `$var::staticMethod()` should resolve through `T`'s static
methods. Neither path is supported today.

**Discovered via:** fixture conversion (type/class_string_new,
type/class_string_static_call).

---

## 28. `__invoke()` return type resolution
**Impact: Low-Medium · Effort: Low**

Calling an object as a function (`$obj()`) does not resolve the return
type from the object's `__invoke()` method. The call expression path
does not check for `__invoke` when the callee is a variable holding
an object type.

**Discovered via:** fixture conversion (call_expression/invoke_return_type).

---

## 29. `@phpstan-type` alias in foreach context
**Impact: Low · Effort: Low**

When a method's return type uses a `@phpstan-type` alias and the result
is iterated in a `foreach`, the alias is not resolved before the
foreach value type is extracted.

**Discovered via:** fixture conversion (type/phpstan_type_alias).

---

## 30. Invoked closure/arrow function return type
**Impact: Low · Effort: Low-Medium**

Immediately invoked closures and arrow functions do not resolve their
return type:

```php
$result = (fn(): Foo => new Foo())();
$result->method(); // unresolved
```

The call expression resolution does not detect that the callee is a
parenthesized closure/arrow function expression.

**Discovered via:** fixture conversion (call_expression/arrow_fn_invocation).

---

## 31. Resolved-class cache: key by FQN + generic args
**Impact: High · Effort: Medium**

The `resolved_class_cache` is keyed by bare FQN (e.g.
`Illuminate\Database\Eloquent\Builder`). When a generic class is
resolved with different type arguments at different call sites
(`Builder<User>` vs `Builder<Post>`), the first resolution wins and
subsequent lookups return the wrong entry. This is the root cause of
the "cache poisoning" bug where model-specific scope methods were lost
on hover, signature help, and deprecation diagnostics.

The current workaround (check the candidate directly before falling
back to the cache) is correct but bypasses the cache for exactly the
cases where it would save the most work: fully-resolved generic classes
with virtual members.

### Why this is the highest-priority cache improvement

Profiling with instrumentation revealed that the dominant cost in
diagnostic passes is **uncached Builder resolution inside the Laravel
virtual member provider**. `build_builder_forwarded_methods` in
`virtual_members/laravel/builder.rs` deliberately calls
`resolve_class_fully` (the uncached variant) because the FQN-keyed
cache cannot distinguish `Builder<Brand>` from `Builder<Product>`.
Every Model resolution triggers a fresh Builder walk (inheritance,
traits, `@mixin Query\Builder`, virtual members). On a file with 3
model classes and 60+ member accesses, this accounts for the majority
of the ~144 ms (release) diagnostic cost.

With a generic-aware cache key, `build_builder_forwarded_methods` can
safely use the cached variant. Each `Builder<Model>` instantiation is
resolved once and reused. This is the single change with the largest
expected speedup.

### Proposed design

Change the cache key from `String` (bare FQN) to `(String, Vec<String>)`
where the second element is the concrete type argument list. For
non-generic classes the list is empty, so the common case stays cheap.

1. `resolve_class_fully_inner` already receives the `ClassInfo` which
   carries `template_params` and the caller's substitution context.
   Thread the concrete type args through to the cache key construction.
2. Update `class_fqn` (or replace it) to return `(fqn, generic_args)`.
3. The cache store and lookup both use the compound key.
4. Update `build_builder_forwarded_methods` to use
   `resolve_class_fully_cached` (or the maybe-cached variant) now that
   different model instantiations get separate cache entries.
5. Remove the "check candidate first" workarounds in `hover/mod.rs`,
   `call_resolution.rs`, and `diagnostics/deprecated.rs`.

### Risks

- Generic args must be normalized (sorted, deduplicated, fully
  qualified) to avoid near-miss cache entries.
- The cache will have more entries (one per instantiation), but each
  entry is a `ClassInfo` clone that would have been computed anyway.
  Memory overhead is proportional to the number of distinct
  instantiations actually used, not the combinatorial space.

---

## 32. Targeted cache invalidation (done) and remaining gaps
**Impact: Low-Medium · Effort: Done / Low**

### What was done

`update_ast` and `parse_and_cache_content_versioned` now use targeted
invalidation instead of clearing the entire `resolved_class_cache`.
Only FQNs defined in the edited file (both old and new, to handle
renames) are evicted. Classes from other files keep their cached
resolution across edits.

### Why the measured speedup is small today

Profiling with per-call instrumentation showed that the cache hit rate
on cross-file workspaces is lower than expected. The root cause is
§31: `build_builder_forwarded_methods` calls the **uncached**
`resolve_class_fully` on every Model resolution (deliberately, because
the FQN-keyed cache conflates `Builder<Brand>` with `Builder<Post>`).
This means the most expensive resolution (Builder with its traits,
`@mixin`, and virtual members) is always computed from scratch, and
targeted invalidation cannot help.

On `example.php` specifically, targeted invalidation provides zero
benefit because all 164 classes are defined in the same file. Editing
the file evicts every FQN, equivalent to a full clear.

### Remaining gap

Once §31 (generic-keyed cache) is implemented, targeted invalidation
will compound with it: vendor/stub classes that are never edited will
stay cached across keystrokes indefinitely. The combination of both
changes should yield the 5-10x speedup on repeated diagnostic passes
that was originally estimated.

No further code changes are needed for targeted invalidation itself.
The remaining work is §31.

---

## 33. Signature-level cache invalidation: skip eviction when only method bodies change
**Impact: High · Effort: Low-Medium**

During normal editing the user types inside a method body. The class's
signature (method names, parameter types, return types, property types,
constants, parent class, interfaces, traits, template params, docblock
tags) has not changed. Only byte offsets and AST nodes inside method
bodies differ between the old and new parse. Yet targeted invalidation
(§32) still evicts every FQN defined in the edited file, because
`update_ast` replaces the `ast_map` entry and cannot tell whether the
change was signature-relevant or body-only.

The old `ClassInfo` entries (from the previous parse, stored in
`ast_map`) and the new ones (just parsed) are both available at the
point where invalidation happens. A cheap comparison at the signature
level can decide whether to skip eviction entirely for each FQN.

### What counts as "signature"

Fields that affect the resolved-class cache (inheritance, virtual
members, type resolution):

**ClassInfo level:**
`kind`, `name`, `file_namespace`, `parent_class`, `interfaces`,
`used_traits`, `mixins`, `is_final`, `is_abstract`,
`deprecation_message`, `template_params`, `template_param_bounds`,
`extends_generics`, `implements_generics`, `use_generics`,
`type_aliases`, `trait_precedences`, `trait_aliases`,
`class_docblock`, `backed_type`, `laravel`

**MethodInfo level:**
`name`, `parameters` (name, type_hint, is_required, default_value,
is_variadic, is_reference), `return_type`, `native_return_type`,
`is_static`, `visibility`, `conditional_return`,
`deprecation_message`, `template_params`, `template_param_bounds`,
`template_bindings`, `has_scope_attribute`, `is_virtual`

**PropertyInfo level:**
`name`, `type_hint`, `visibility`, `is_static`, `is_readonly`,
`deprecation_message`, `is_virtual`

**ConstantInfo level:**
`name`, `type_hint`, `value`, `visibility`, `deprecation_message`

### What is NOT signature-relevant

- `start_offset`, `end_offset`, `keyword_offset` on `ClassInfo`
- `name_offset` on `MethodInfo`, `PropertyInfo`, `ConstantInfo`
- `description`, `return_description`, `link` on `MethodInfo`
- `description` on `PropertyInfo`
- `description` on `ConstantInfo`
- `ParameterInfo::name_offset`

These change on every keystroke (offset shifts) or are display-only
(descriptions, links) and do not affect resolution.

### Proposed design

1. Add a `fn signature_eq(&self, other: &ClassInfo) -> bool` method to
   `ClassInfo`. It compares all signature-relevant fields listed above
   and returns `true` when they are identical. Methods are compared as
   sets (order-insensitive by name) so that reordering methods in
   source does not trigger eviction.

2. In `update_ast_inner`, before evicting a FQN from the cache,
   look up the old `ClassInfo` in the previous `ast_map` entry and
   the new `ClassInfo` from the current parse. If
   `old.signature_eq(&new)`, skip eviction for that FQN.

3. The sub-structs (`MethodInfo`, `PropertyInfo`, `ConstantInfo`,
   `ParameterInfo`) each get a `signature_eq` method with the same
   pattern: compare everything except offsets and descriptions.

### Expected impact

During normal editing (typing inside method bodies), zero cache
entries are evicted. The resolved-class cache stays fully warm across
keystrokes. Combined with §32 (targeted invalidation) and §31
(generic-keyed cache), this means:

- Typing inside a method body: zero evictions, all cache hits
- Adding/removing a method or changing a type hint: evicts only that
  class's FQN(s), dependents self-correct on next resolution
- Changing inheritance (extends, implements, use): evicts the class,
  dependents may briefly be stale (acceptable, same as §32)

For `example.php` (164 classes in one file), this is transformative:
editing inside any method body keeps all 164 resolved entries warm,
turning the current "evict everything" into "evict nothing" for the
overwhelmingly common case.

### Risks

- If `signature_eq` misses a field that affects resolution, the cache
  serves stale data. Mitigate with a `#[cfg(debug_assertions)]`
  assertion that compares old and new resolved output when
  `signature_eq` returns `true`, catching mismatches during testing.
- The comparison has a cost, but it is negligible: comparing a few
  strings and vecs in memory versus the alternative of re-walking
  inheritance chains, loading files, and injecting virtual members.
- `class_docblock` changes (adding `@method`, `@property` tags) are
  correctly detected as signature changes because the raw docblock
  text is compared.