# PHPantom — Bug Fixes

## B7: PHPDoc `@param` generic array type not merged with native `array` hint

When a method has a native type hint `array` and a PHPDoc `@param` with
a generic type like `list<Request>`, PHPantom doesn't merge the PHPDoc
element type with the native `array` for narrowing purposes. After an
`is_array()` guard, the variable narrows to `array` but loses the `Request`
element type from the docblock.

Real-world example — `MobilePayConnection.php`:

```php
/**
 * @param null|list<Request>|Request $request
 */
protected function connect(string $uri, null|array|Request $request, ...): MobilePayResponse
{
    if (is_array($request)) {
        foreach ($request as $item) {
            $serializedObjects[] = $item->jsonSerialize();
            //                     ^^^^^ unresolved
        }
    }
}
```

After `is_array($request)`, `$request` narrows from `null|array|Request`
to `array`. The `@param` says the array case is `list<Request>`, so
`$item` should be `Request`. But the LSP doesn't unify the narrowed
native `array` with the docblock's `list<Request>`.

**Impact:** 1 diagnostic in the shared project
(`MobilePayConnection:76`).