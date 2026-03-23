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

#### B12. Interface-extends-interface constants (and other members) not merged

| | |
|---|---|
| **Impact** | Low-Medium |
| **Effort** | Low-Medium |

When an interface extends multiple parent interfaces (e.g.
`interface CarbonInterface extends DateTimeInterface, JsonSerializable, UnitValue`),
`resolve_class_with_inheritance` only walks the `parent_class` field (the
first extended interface). The remaining parent interfaces stored in the
`interfaces` list are not traversed for member merging.

Then `resolve_class_fully_inner` calls `resolve_class_with_inheritance` on
each interface collected from the class, but that inner call has the same
limitation — it does not recurse into the interface's own `interfaces` list.

**Reproducer:** `Illuminate\Support\Carbon::JANUARY` — the `JANUARY` constant
lives on `Carbon\Constants\UnitValue`, which `CarbonInterface` extends (6th
in the extends list). PHPantom reports "Member 'JANUARY' not found on class
'Illuminate\Support\Carbon'".

**Fix:** In `resolve_class_with_inheritance`, when processing an interface
(or always), also merge members from all entries in `self.interfaces`, not
just `parent_class`. Alternatively, make `resolve_class_fully_inner`
recursively collect parent interfaces when resolving each interface.

Affects 4 diagnostics in shared (Carbon month constants) and likely more in
other projects that use interfaces with multi-extends chains.

---

#### B13. Variable type resolved from reassignment target inside RHS expression

| | |
|---|---|
| **Impact** | Low |
| **Effort** | Medium |

When a variable is reassigned with an expression that references itself in
the RHS arguments, PHPantom resolves the variable to the NEW type inside
those arguments instead of the original type.

**Reproducer:**
```php
public function requestToken(PaymentTokenRequest $request, ...): ... {
    // $request is PaymentTokenRequest here
    $request = new CreateRecurringSessionRequest(
        paymentMethodReference: $request->uuid,  // ← PHPantom resolves $request as CreateRecurringSessionRequest
    );
}
```

PHP evaluates all arguments before performing the assignment, so `$request->uuid`
should resolve against `PaymentTokenRequest`. PHPantom's variable definition
offset tracking considers the new definition active too early — it should
only take effect after the full RHS expression is evaluated.

Affects 1 diagnostic in shared. Edge case but could appear in code that
reuses variable names across reassignments with self-referencing expressions.