# PHPantom — Ignored Fixture Tasks

There are **228 fixture tests** in `tests/fixtures/`. Of these, **224
pass** and **4 are ignored** because they exercise features or bug
fixes that are not yet implemented. Each ignored fixture has a
`// ignore:` comment explaining what is missing.

This document groups the 4 ignored fixtures by the underlying work
needed to un-ignore them. Tasks are ordered by the number of fixtures
they unblock (descending), then by estimated effort. Once a task is
complete, remove the `// ignore:` line from each fixture, verify the
fixture passes, and delete the task from this file.

After completing a task, run the full CI suite:

```
cargo test
cargo clippy -- -D warnings
cargo clippy --tests -- -D warnings
cargo fmt --check
php -l example.php
```

---

## 11. `class-string<T>` on interface method not inherited (1 fixture)

**Ref:** [type-inference.md §25](type-inference.md#25-class-stringt-on-interface-method-not-inherited)
**Impact: Medium · Effort: Medium**

When an interface method uses `class-string<T>` and a class implements
that interface, the generic return type is lost during inheritance
merging.

**Fixture:**

- [ ] `generics/class_string_generic_interface.fixture` — `class-string<T>` on interface method not propagated

---

## 16. Generic `@phpstan-assert` with `class-string<T>` (1 fixture)

**Ref:** [type-inference.md §20](type-inference.md#20-generic-phpstan-assert-with-class-stringt-parameter-inference)
**Impact: Medium · Effort: Medium-High**

`@phpstan-assert T $value` with `@template T` bound via a
`class-string<T>` parameter should infer the narrowed type from the
class-string argument at the call site.

**Fixture:**

- [ ] `narrowing/phpstan_assert_generic.fixture` — `assertInstanceOf(Foo::class, $x)` narrows `$x` to `Foo`

---

## 25. Pass-by-reference parameter type inference (1 fixture)

**Ref:** [type-inference.md §7](type-inference.md#7-narrow-types-of-var-parameters-after-function-calls)
**Impact: Low · Effort: Medium**

Functions that accept `&$var` parameters can change the variable's type.
After calling such a function, the variable's type should reflect the
function's documented effect (e.g. `preg_match($pattern, $subject, $matches)`
should give `$matches` an array type).

**Fixture:**

- [ ] `variable/pass_by_reference.fixture` — `&$var` parameter type inferred after call

---

## 26. Pipe operator (PHP 8.5) (1 fixture)

**Ref:** [type-inference.md §1](type-inference.md#1-pipe-operator-php-85)
**Impact: Low · Effort: Medium**

The `|>` pipe operator (PHP 8.5) passes the left side as the first
argument to the right side and returns the result.

**Fixture:**

- [ ] `pipe_operator/basic_pipe.fixture` — `$x |> foo(...)` resolves return type

---

## Summary by effort

Medium effort, single fixture:

| Task | Fixtures |
|---|---|
| §11 `class-string<T>` on interface method | 1 |
| §16 Generic `@phpstan-assert` with `class-string<T>` | 1 |
| §25 Pass-by-reference parameter type inference | 1 |
| §26 Pipe operator (PHP 8.5) | 1 |