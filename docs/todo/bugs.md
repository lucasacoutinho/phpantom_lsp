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

#### B15. Inline `@var` cast overrides variable type in same-line RHS

| | |
|---|---|
| **Impact** | Low |
| **Effort** | Low |

When an inline `/** @var T */` annotation precedes a reassignment
like `$data = $data->toArray()`, PHPantom resolves `$data` on the
RHS as type `T` instead of its previous type. The `@var` cast
should only apply to the variable after the assignment completes.

**Reproducer:**

```php
class Data {
    public function toArray(): array { return []; }
}
class Test {
    public function run(Data $data): array {
        /** @var array<string, mixed> */
        $data = $data->toArray();
        //      ^^^^^ resolved as array<string, mixed> instead of Data
        return $data;
    }
}
```

Produces: `Cannot access method 'toArray' on type 'array<string, mixed>'`

**Root cause:** This is a variant of B13 (variable type resolved from
reassignment target inside RHS). B13 was fixed for the case where the
assignment target name matches the variable in the RHS expression,
but the `@var` inline annotation applies the type override before the
RHS is evaluated, so the check does not catch it.

**Impact in shared codebase:** 2 false positives (Klarna.php and
GoogleTagManagerClient.php, both using `/** @var array<string, mixed> */
$data = $data->toArray()`).

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