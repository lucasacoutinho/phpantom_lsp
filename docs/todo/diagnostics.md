# PHPantom — Diagnostics

Items are ordered by **impact** (descending), then **effort** (ascending)
within the same impact tier.

| Label | Scale |
|---|---|
| **Impact** | **Critical**, **High**, **Medium-High**, **Medium**, **Low-Medium**, **Low** |
| **Effort** | **Low** (≤ 1 day), **Medium** (2-5 days), **Medium-High** (1-2 weeks), **High** (2-4 weeks), **Very High** (> 1 month) |

---

## Severity philosophy

PHPantom assigns diagnostic severity based on runtime consequences:

| Severity | Criteria | Examples |
|---|---|---|
| **Error** | Would crash at runtime. The code is definitively wrong. | Member access on a scalar type (`$int->foo()`). Calling a function that doesn't exist (`doesntExist()`). |
| **Warning** | Likely wrong but could work for reasons we can't verify statically. The types are poor but the code might be correct at runtime. | Accessing a member that doesn't exist on a non-final class (`$user->grantAccess()` where `User` has no such method but a subclass might). Unknown class in a type position (`Class 'Foo' not found`). Subject type resolved to an unknown class so members can't be verified. |
| **Hint** | The codebase lacks type information. Off by default or very subtle. Poorly typed PHP is so common that showing these by default would be noise for most users. Anyone who does care about type safety is likely running PHPStan already. Unless our engine becomes very strong, these diagnostics either expose our own inference gaps or bother users who never opted into static analysis. | `mixed` subject member access (opt-in via `unresolved-member-access`). Deprecated symbol usage (rendered as strikethrough). |
| **Information** | Advisory. Something the developer might want to know. | Unused `use` import (rendered as dimmed). Unresolved type in a PHPDoc tag. |

---

## 1. Scalar member access diagnostic — remaining gaps
**Impact: High · Effort: Low**

Member access on a scalar type is always a runtime error. The
`scalar_member_access` diagnostic (Error severity) is implemented for
bare variables, property chains, and call expression returns. Duplicate
suppression (dropping the `unresolved_member_access` hint when a more
specific diagnostic overlaps) is also done.

### Remaining gaps

| Scenario | Current | Expected |
|---|---|---|
| `$user->getName()->trim()` | Silent | **Error**: Cannot access method 'trim' on type 'string' |
| `$user->getAge()->value` | Silent | **Error**: Cannot access property 'value' on type 'int' |

These are method-return-chain subjects where the return type is scalar.
The `resolve_scalar_subject_type` function handles `CallExpr` callee
types for standalone functions and static methods, but method call
chains where the intermediate callee is itself a call expression are
not yet covered.

---

## 2. Chain and function-return member diagnostics
**Impact: High · Effort: Medium**

When a method return chain or function call return resolves to a known
class, unknown member diagnostics should fire on the final member. This
is the same "known type, missing member" logic that already works for
direct variables and parameters, but the symbol map's `subject_text`
for chain and call expressions isn't being resolved through to the end.

### Current state

- Direct variable and parameter member access: working.
- Static access on known class: working.
- Property chains (`$user->getProfile()->nonexistent`): NOT flagged.
- Function return member access (`getUser()->nonexistent`): NOT flagged.

### Gaps to fix

| Scenario | Current | Expected |
|---|---|---|
| `$user->getProfile()->nonexistent` | Silent | **Warning**: Property 'nonexistent' not found on class 'Profile' |
| `$user->getProfile()->fakeFn()` | Silent | **Warning**: Method 'fakeFn' not found on class 'Profile' |
| `getUser()->nonexistent` | Silent | **Warning**: Property 'nonexistent' not found on class 'User' |
| `getUser()->fakeMethod()` | Silent | **Warning**: Method 'fakeMethod' not found on class 'User' |

### Implementation notes

The completion resolver pipeline (`resolve_target_classes`) handles all
these subject forms correctly for completion. The issue is that the
symbol map's `subject_text` for these expressions may not carry enough
context, or the diagnostic walker doesn't pass it through the full
resolver pipeline. Verify that the `subject_text` captured for
`$user->getProfile()->nonexistent` is `$user->getProfile()` and that
`resolve_target_classes` returns `Profile` for it.

---

## 3. Chain error propagation (flag only the first broken link)
**Impact: Medium · Effort: Medium**

In a fluent chain like `$m->callHome()->callMom()->callDad()`, only the
first broken link should be flagged. Subsequent links in the chain
depend on the return type of the broken call, so flagging them adds
noise without actionable information.

### Current state

- Fluent chains on `mixed` subjects: only the first link is flagged
  (the chain members have no `MemberAccess` spans because the parser
  can't resolve the subject). This works by accident.
- Fluent chains on known types where the first method is unknown:
  only the first is flagged (same reason, parser stops). Also works
  by accident.
- Scalar chains (`$user->getAge()->value->deep`): the scalar member
  access at `->value` should be flagged but `->deep` should be silent.
  Currently `->value` is not flagged at all (see item 1).

### Desired behavior

- `$m->callHome()->callMom()->callDad()` — flag only `callHome`.
- `$m->callHome(); $m->callMom();` — flag both (separate statements).
- `$user->fakeMethod()->next()->deep()` — flag only `fakeMethod`.
- `$user->getAge()->value->deep` — flag only `->value` (scalar error).

### Cross-assignment propagation (nice to have)

```
$home = $m->callHome();  // flagged
$home->callMom();        // ideally silent
```

This is harder because it requires tracking that `$home` was assigned
from an already-flagged expression. Acceptable if not implemented in
the first pass.

---

## 4. Deprecated rendering
**Impact: Low-Medium · Effort: Low**

Deprecated class references (e.g. `new OldHelper()`) currently show a
hint message but the `DiagnosticTag::DEPRECATED` tag (which renders as
strikethrough in most editors) may not be applied correctly for all
deprecated symbol types. Verify that:

- Deprecated class references in `new`, type hints, `extends`, and
  `implements` positions all render with strikethrough.
- Deprecated method calls render with strikethrough.
- Deprecated property accesses render with strikethrough.
- The deprecated diagnostic resolver uses offset-based class resolution
  (not "first class in file") for `$this`/`self`/`static` subjects, so
  that files with multiple classes resolve correctly.
- Chain subjects (`getHelper()->deprecatedMethod()`) resolve through
  the full completion pipeline, not the hand-rolled
  `resolve_subject_to_class_name` helper that can't handle chains.

---

## 5. Unresolved type in PHPDoc
**Impact: Medium · Effort: Medium**

A `@return`, `@param`, `@var`, `@throws`, `@mixin`, or `@extends` tag
references a class that cannot be resolved. This is advisory (the code
may still work if the type is only used for static analysis), so it
should be **Information** severity.

| Scenario | Expected |
|---|---|
| `@return SomeAlias` where SomeAlias is not a class, type alias, or template | **Info**: Type 'SomeAlias' in @return could not be resolved |
| `@param NonExistent $x` | **Info**: Type 'NonExistent' in @param could not be resolved |
| `@throws FakeException` | **Info**: Type 'FakeException' in @throws could not be resolved |

### Implementation notes

This partially overlaps with `unknown_classes.rs` which already flags
`ClassReference` spans in docblock type positions. The remaining gap is
PHPDoc tags that reference types which are not emitted as
`ClassReference` spans by the symbol map. Audit which docblock type
positions produce `ClassReference` spans and which don't.

---

## 6. Diagnostic suppression intelligence
**Impact: Medium · Effort: Medium**

When PHPantom proxies diagnostics from external tools, users need a way
to suppress specific warnings. Rather than forcing them to install a
separate extension or memorise each tool's suppression syntax, PHPantom
can offer **code actions to insert the correct suppression comment** for
the tool that produced the diagnostic.

PHPStan suppression is implemented: "Ignore PHPStan error" adds
`// @phpstan-ignore <identifier>` (appending to existing ignores when
present), and "Remove unnecessary @phpstan-ignore" cleans up unmatched
ignores reported by PHPStan. What remains:

### Remaining tools

- Psalm: `/** @psalm-suppress [IssueType] */` on the line or above
  the function/class.
- PHPCS: `// phpcs:ignore [Sniff.Name]` or `// phpcs:disable` /
  `// phpcs:enable` blocks.
- PHPMD: `// @SuppressWarnings(PHPMD.[RuleName])` in a docblock.
- For PHPantom's own diagnostics: support `@suppress PHPxxxx`
  in docblocks (matching PHP Tools' convention) and a config flag
  `phpantom.diagnostics.enabled: bool` (default `true`).

**Prerequisites:** Each tool needs a diagnostic proxy before its
suppression actions can be wired up.

---

## 8. Unreachable code diagnostic
**Impact: Low-Medium · Effort: Low**

Dim code that appears after unconditional control flow exits:
`return`, `throw`, `exit`, `die`, `continue`, `break`. This is a
Phase 1 (fast) diagnostic since it requires only AST structure, not
type resolution.

### Behaviour

| Scenario | Rendering |
|---|---|
| Code after `return $x;` in same block | Dimmed (DiagnosticTag::UNNECESSARY) |
| Code after `throw new \Exception()` | Dimmed |
| Code after `exit(1)` or `die()` | Dimmed |
| Code after `continue` or `break` in a loop | Dimmed |
| Code after `if (...) { return; } else { return; }` | Dimmed (both branches exit) |

Severity: **Hint** with `DiagnosticTag::UNNECESSARY` so editors dim
the text rather than underlining it. This matches how unused imports
are rendered.

### Implementation

Walk the AST statement list. After encountering a statement that
unconditionally exits the current scope (return, throw, expression
statement containing `exit`/`die`), mark all subsequent statements in
the same block as unreachable. The span covers from the start of the
first unreachable statement to the end of the last statement in the
block.

Phase 1 only handles the simple single-block case. Whole-branch
analysis (both if/else branches exit) is a future refinement.

### Debugging value

When our type engine silently resolves a method to a `never` return
type (e.g. an incorrectly resolved overload), unreachable code after
the call becomes visible, signalling the bug.

---

## 9. Implementation error diagnostic
**Impact: Medium · Effort: Medium**

Flag concrete classes that fail to implement all required methods from
their interfaces or abstract parents. PHPantom already has the
"implement missing methods" code action that detects this condition.
Surfacing it as a diagnostic makes the problem visible without the
user needing to trigger quick-fix.

### Behaviour

| Scenario | Severity | Message |
|---|---|---|
| Class implements interface but misses a method | Error | Class 'Foo' must implement method 'bar()' from interface 'Baz' |
| Class extends abstract class but misses abstract method | Error | Class 'Foo' must implement abstract method 'bar()' from class 'AbstractBaz' |
| Non-abstract class has abstract method | Error | Non-abstract class 'Foo' contains abstract method 'bar()' |

### What we already have

The `collect_implement_methods_actions` function in
`code_actions/implement_methods.rs` already:
1. Loads the class and its full inheritance chain.
2. Collects all abstract methods from interfaces and abstract parents.
3. Checks which ones are missing from the concrete class.

The diagnostic can reuse this exact logic, just emitting a diagnostic
instead of (or in addition to) offering a code action.

### Implementation

1. In the Phase 2 diagnostic collector, for each `ClassDeclaration`
   span in the symbol map, load the class via `find_or_load_class`.
2. Skip abstract classes, interfaces, traits, and enums.
3. Run the same missing-method detection as the code action.
4. Emit an Error-severity diagnostic on the class name span for each
   missing method. Group into a single diagnostic with all missing
   method names listed if there are multiple.
5. Pair the diagnostic with the existing "implement missing methods"
   code action so the quick-fix button appears inline.

### Debugging value

When our inheritance resolution misses a parent class or trait, the
implementation error diagnostic fires unexpectedly (flagging methods
that are actually implemented via a trait that we failed to resolve).
This makes inheritance resolution bugs immediately visible.