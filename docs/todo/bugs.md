# PHPantom — Bug Fixes

## B10. `instanceof` ternary narrowing fails when target class is in a phar
**Impact: Low · Effort: Low-Medium**

Pattern:
```php
$types = $argType instanceof UnionType ? $argType->getTypes() : [$argType];
```

The type engine is unified: completion and diagnostics both use
`resolve_variable_types` → `walk_statements_for_assignments` →
`try_apply_ternary_instanceof_narrowing`. A local-class test
(assignment RHS context, interface parameter, `instanceof` subclass
in ternary condition) passes with zero diagnostics.

The failure in the shared project is specific to PHPStan phar classes.
`PHPStan\Type\UnionType` lives inside `phpstan.phar` and may not be
loadable at resolution time, so `apply_instanceof_inclusion` silently
fails (cannot resolve the class name to a `ClassInfo`), and the
variable keeps its un-narrowed type (`Type`).

**Observed in:** `DecimalDivThrowTypeExtension:54` — `$argType` is
typed as `PHPStan\Type\Type`, `getTypes()` exists on `UnionType` but
not on the `Type` interface. The `instanceof UnionType` check in the
ternary condition should narrow the type in the then-branch.

**Root cause:** The `instanceof` target class (`UnionType`) cannot be
loaded from the phar, so `apply_instanceof_inclusion` has no
`ClassInfo` to narrow to. The narrowing architecture itself is correct
and unified across completion and diagnostics. The fix is either
phar class indexing or suppressing diagnostics when the `instanceof`
target class is unresolvable (since the developer clearly expects
narrowing to occur).

---

## B11. `static` return type not resolved through `class-string<T>` context
**Impact: Low · Effort: Medium**

Pattern:
```php
/** @param class-string<BackedEnum> $class */
function enum(BackedEnum $value, string $class): void {
    foreach ($class::cases() as $item) {
        $name = $item->name;   // unresolved
        $val  = $item->value;  // unresolved
    }
}
```

`UnitEnum::cases()` returns `static[]`. When called on a variable
typed as `class-string<BackedEnum>`, the `static` return type should
resolve to `BackedEnum[]`, making `$item` typed as `BackedEnum`.
Currently `static` is not substituted through the `class-string<T>`
context, so `$item` has no type and `->name` / `->value` are
unresolvable.

**Observed in:** `OptionList:27,30` — `$class::cases()` where
`$class` is `class-string<BackedEnum>`.

**Root cause:** The `class-string<T>` static dispatch fix (`caa6163`)
resolves method signatures and return types for static calls on
`class-string`-typed variables, but does not substitute `static` in
the return type with the bound type `T`. The substitution
`static → BackedEnum` needs to happen when the call target is
resolved through a `class-string<BackedEnum>` context.

---

## B12. `Collection::reduce()` generic return type not inferred
**Impact: Low · Effort: Medium**

Pattern:
```php
$result = $collection
    ->reduce(fn(Decimal $carry, OrderProduct $p): Decimal => $carry->add($p->price), new Decimal('0'))
    ->add($total);  // unresolved
```

The `reduce()` method on Laravel collections has this signature:
```
@template TReduceInitial
@template TReduceReturnType
@param callable(TReduceInitial|TReduceReturnType, TValue, TKey): TReduceReturnType $callback
@param TReduceInitial $initial
@return TReduceReturnType
```

PHPantom should infer:
- `TReduceInitial = Decimal` (from the `$initial` argument)
- `TReduceReturnType = Decimal` (from the closure return type hint)
- Therefore `reduce()` returns `Decimal`

The bidirectional template inference (`4329efe`) partially addresses
this, but `reduce()` still returns unresolved. The likely gap is the
union `TReduceInitial|TReduceReturnType` in the callable's first
parameter position: the inference engine may not decompose the union
to extract individual template bindings when both templates appear
in the same callable parameter type.

**Observed in:** `FlowService:517` — `->reduce(fn(Decimal $c, ...):
Decimal => ..., new Decimal('0'))->add($total)`.

---

## B13. Array shape tracking from keyed literal assignments in loops
**Impact: Low · Effort: High**

Pattern:
```php
$bundleProductCounts = [];
foreach ($items as $item) {
    $bundleProductCounts[$item->id] = [
        'bundle' => $item->productBundle,
        'count'  => 1,
    ];
}
foreach ($bundleProductCounts as $entry) {
    $entry['bundle']->parentProduct();  // unresolved
}
```

PHPantom tracks array value types from variable-key assignments
(`$arr[$key] = $value`), but when the value is an array literal with
string keys (a shape), the element type is not preserved as a shape.
Subsequent access like `$entry['bundle']->method()` requires knowing
that `'bundle'` maps to a specific class type.

**Observed in:** `ProductSupplyAmountChangeListener:58` — array built
with `['bundle' => $productBundle, 'count' => 1]` in a loop, then
iterated; `$bundleProductCount['bundle']->parentProduct()` is
unresolvable because the shape is lost.

**Depends on:** T19 (structured type representation) or at minimum
a basic array shape inference that preserves `array{key: Type}` from
literal array constructors and propagates it through foreach.

---

## B14. `DB::select()` returns bare `array`, element type unresolvable
**Impact: Low · Effort: Low**

Pattern:
```php
$result = DB::select('select sum(...) as incoming_qty from ...');
if ($result[0] instanceof stdClass && $result[0]->incoming_qty) {
    return Convert::toInt($result[0]->incoming_qty);  // unresolved
}
```

The `DB` facade's `@method` annotation declares `select()` as
returning bare `array` rather than `array<int, stdClass>`. Since the
return type has no element type, `$result[0]` is untyped and
`->incoming_qty` is unresolvable.

Two compounding issues:

1. **Stub/facade return type gap:** `DB::select()` should return
   `array<int, stdClass>` (which is what the underlying
   `Connection::select()` actually returns), but the facade
   annotation only says `array`.

2. **`instanceof` narrowing on array element access:** Even if the
   array element type were known, `$result[0] instanceof stdClass`
   should narrow `$result[0]` to `stdClass` within the condition.
   The narrowing system only matches bare variable names (`$var`),
   not subscript expressions (`$var[0]`). This is a T20 concern.

**Observed in:** `PurchaseFileService:1087,1089`.

**Partial workaround:** PHPantom could patch `DB::select()` to return
`array<int, stdClass>` via framework-specific return type overrides
(similar to how scope methods are handled). The `instanceof`
narrowing gap (issue 2) would remain but would no longer trigger
here because `stdClass` property access is already suppressed.