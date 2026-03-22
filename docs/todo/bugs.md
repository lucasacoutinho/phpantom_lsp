# PHPantom — Bug Fixes

Known bugs and incorrect behaviour. These are distinct from feature
requests — they represent cases where existing functionality produces
wrong results. Bugs should generally be fixed before new features at
the same impact tier.

Items are ordered by **impact** (descending), then **effort** (ascending)
within the same impact tier.

| Label      | Scale                                                                                                                  |
| ---------- | ---------------------------------------------------------------------------------------------------------------------- |
| **Impact** | **Critical**, **High**, **Medium-High**, **Medium**, **Low-Medium**, **Low**                                           |
| **Effort** | **Low** (≤ 1 day), **Medium** (2-5 days), **Medium-High** (1-2 weeks), **High** (2-4 weeks), **Very High** (> 1 month) |

---

#### B2. Trait `$this` member access produces false positives

| | |
|---|---|
| **Impact** | High |
| **Effort** | Medium |

When a trait method accesses `$this->prop` or `$this->method()`, the
diagnostic resolver only sees the trait's own members. It does not
consider the classes that `use` the trait, so it emits "not found"
warnings for members that exist on every host class.

This accounts for roughly 14% of false-positive `unknown_member`
diagnostics in the same triage run.

```php
trait LogsErrors {
    public function logError(): void {
        $this->model;       // ← "Property 'model' not found on trait"
        $this->eventType;   // ← same
    }
}

class ImportJob {
    use LogsErrors;
    public string $model = 'Product';
    public string $eventType = 'import';
}
```

**Fix direction:** suppress `unknown_member` diagnostics for `$this->`
inside trait methods, or resolve `$this` to the union of all classes
that `use` the trait (at least within the same file or project).

---

#### B3. Type narrowing missing in `&&` expressions

| | |
|---|---|
| **Impact** | Medium |
| **Effort** | Low |

`instanceof` checks inside `&&` chains do not narrow the variable type
for subsequent operands in the same expression. Narrowing already works
inside `if` bodies, but the `&&` short-circuit path is not handled.

```php
// Works: narrowing inside if body
if ($e instanceof QueryException) {
    $e->errorInfo; // ✓ resolved
}

// Broken: narrowing inside && operand
$e instanceof QueryException && $e->errorInfo; // ← "not found on Throwable"
```

**Fix direction:** when walking `&&` (logical AND) expressions,
propagate `instanceof` narrowing from the left operand to the right
operand, the same way it is already propagated into `if` bodies.