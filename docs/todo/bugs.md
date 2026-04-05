# PHPantom — Bug Fixes

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