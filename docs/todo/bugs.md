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

#### B17. Static property access subject resolves to containing class instead of property type

| | |
|---|---|
| **Impact** | Low |
| **Effort** | Low |

When a static property is accessed via `self::$prop->method()`,
PHPantom resolves the member-access subject to the class containing
the static property instead of the property's declared type.

**Reproducer:**

```php
class Connection {
    public function setConfig(Config $config): void {}
}

class ConnectionManager {
    private static Connection $instance;

    public static function getInstance(): Connection {
        self::$instance->setConfig($config);
        //              ^^^^^^^^^ "Method 'setConfig' not found on class 'ConnectionManager'"
        return self::$instance;
    }
}
```

PHPantom reports `setConfig` not found on `ConnectionManager` instead
of looking it up on `Connection` (the declared type of `$instance`).

**Root cause:** The diagnostic subject resolution in
`src/diagnostics/unknown_members.rs` — the `StaticAccess` →
`PropertyChain` path returns the class that owns the static property
rather than the property's type hint.

**Impact in shared codebase:** 1 false positive
(`MobilePayConnectionManager::$instance->setMobilePayConnectionConfiguration()`).

---

#### B18. Null-init variable + loop reassignment doesn't build union type

| | |
|---|---|
| **Impact** | Low |
| **Effort** | Medium |

When a variable is initialized to `null` and conditionally reassigned
inside a loop, PHPantom resolves the variable to `null` without
considering the reassignment sites.  Guard clauses like
`$var !== null` can't narrow the type because the variable was never
resolved to a union containing both `null` and the real type.

**Reproducer:**

```php
$lastPaidEnd = null;
foreach ($periods as $period) {
    $lastPaidEnd = $period->end;  // CarbonImmutable
}
if ($lastPaidEnd !== null && $lastPaidEnd->diffInDays() > 30) { ... }
//                           ^^^^^^^^^^^^ "on type 'null'"
```

PHPantom should build a union `CarbonImmutable|null` from both
assignment sites, then the `!== null` guard should narrow it to
`CarbonImmutable`.

**Root cause:** `walk_statements_for_assignments` in
`src/completion/variable/resolution.rs` doesn't aggregate all
assignment sites for a variable into a union type.  It picks the
first or most recent assignment rather than building a union from
all reachable assignments.

**Impact in shared codebase:** 1 false positive
(CustomerService.php L302 — `diffInDays` on `null`).

---

#### B19. Guard clause with `continue`/`return` doesn't narrow type

| | |
|---|---|
| **Impact** | Low |
| **Effort** | Low |

After `if (!$var) { continue; }` or `if (!$var) { return; }`,
PHPantom should narrow `$var` to non-null (or non-falsy) in the
code that follows.  Currently the nullable type persists.

**Reproducer:**

```php
$warehouseOrderline = $warehouseOrderLines[$key] ?? null;
if (!$warehouseOrderline) {
    continue;
}
$warehouseOrderline->actualAmount;  // "on type 'null'"
```

After the `continue`, `$warehouseOrderline` is guaranteed non-null,
but PHPantom still sees `null`.

**Root cause:** `src/completion/variable/resolution.rs` — early-exit
narrowing (guard clause + `continue`/`return`/`throw`/`break`) is
not implemented for the variable type resolution path.

**Impact in shared codebase:** 2 false positives (PCNService.php
L1073 `actualAmount`, L1077 `amount`).

---