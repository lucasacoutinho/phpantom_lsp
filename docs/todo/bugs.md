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

#### B3. Type engine does not resolve `$this`/`static` inside traits

| | |
|---|---|
| **Impact** | Medium |
| **Effort** | Low |

When a trait method (or a closure inside a trait method) accesses
members via `$this`, `self`, `static`, or `parent`, the type engine
resolves the subject to the trait itself rather than to the host class.
Since the trait does not declare the members that the host class
provides, the type engine reports the members as missing. This affects
completion, hover, and go-to-definition for any trait that relies on
host-class members.

The diagnostic pass has a suppression heuristic for this case, but
the underlying problem is in the type engine: it should resolve
`$this`/`static`/`self` inside a trait to the using class (when known)
or defer resolution (when the host class is not known). The
suppression heuristic also fails when the access is inside a closure
nested within a trait method.

**Observed:** 43 cases in `shared`. The largest cluster (41) is
`BusinessCentralErrorHandlerTrait` where `$this->model`,
`$this->eventType`, etc. are properties provided by the host class.
`SalesInfoGlobalTrait` contributes 2 cases where `static::where()` and
`static::query()` are called inside a closure within a trait method.

**Fix:** When the type engine encounters `$this`/`static`/`self` inside
a trait, it should attempt to resolve to the known host class(es). For
the analyze pass where no specific host class is open, the engine
should recognise that trait member accesses are inherently incomplete
and avoid reporting members as missing. The closure nesting issue is a
separate symptom: `find_innermost_enclosing_class` does find the trait
at the closure's offset, but the suppression check does not fire,
suggesting the `subject_text` on the `MemberAccess` span differs from
the expected `"static"` / `"$this"` keywords when emitted from inside
a closure.

---

#### B4. Variable reassignment loses type when parameter name is reused

| | |
|---|---|
| **Impact** | Medium |
| **Effort** | Medium |

When a method parameter is reassigned mid-body, PHPantom sometimes
continues to use the parameter's original type instead of the new
assignment's type.

**Observed:** In `FileUploadService::uploadFile()`, the `$file`
parameter is typed `UploadedFile`. Later, `$file = $result->getFile()`
reassigns it to a different type. PHPantom still resolves `$file->id`
and `$file->name` against `UploadedFile` instead of the model returned
by `getFile()`. This produces 2 false-positive "not found" diagnostics.

**Fix:** The variable resolution pipeline should prefer the most recent
assignment when multiple definitions exist for the same variable name
within the same scope at the cursor offset.

---

#### B7. Overloaded built-in function signatures not representable in stubs

| | |
|---|---|
| **Impact** | Low |
| **Effort** | Low |

Some PHP built-in functions have genuinely overloaded signatures where
the valid argument counts depend on which "form" is being called. The
phpstorm-stubs format cannot express this: it declares a single
signature, so one form's required parameters become false requirements
for the other form.

Around 415 cases where parameters were simply missing their default
values have been fixed upstream in phpstorm-stubs. The remaining cases
are true overloads that the stub format cannot represent:

- `array_keys(array $array): array` vs
  `array_keys(array $array, mixed $filter_value, bool $strict = false): array`
- `mt_rand(): int` vs `mt_rand(int $min, int $max): int`

PHPStan solves this with a separate function signature map
(`functionMap.php`) that overrides stub signatures with corrected
metadata including multiple accepted argument count ranges. PHPantom
needs a similar mechanism.

**Observed:** 10 diagnostics in `shared` (8 `array_keys`, 2 `mt_rand`).

**Fix:** Maintain a small overload map (similar to PHPStan's
`functionMap.php`) that declares alternative minimum argument counts
for functions with true overloads. The argument count checker consults
this map before flagging. The map only needs entries for functions
where the stub's single signature cannot represent the valid call
forms.

