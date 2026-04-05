# PHPantom — LSP Features

Items are ordered by **impact** (descending), then **effort** (ascending)
within the same impact tier.

| Label      | Scale                                                                                                                  |
| ---------- | ---------------------------------------------------------------------------------------------------------------------- |
| **Impact** | **Critical**, **High**, **Medium-High**, **Medium**, **Low-Medium**, **Low**                                           |
| **Effort** | **Low** (≤ 1 day), **Medium** (2-5 days), **Medium-High** (1-2 weeks), **High** (2-4 weeks), **Very High** (> 1 month) |

---

## F2. Partial result streaming via `$/progress`

**Impact: Medium · Effort: Medium-High**

The LSP spec (3.17) allows requests that return arrays — such as
`textDocument/implementation`, `textDocument/references`,
`workspace/symbol`, and even `textDocument/completion` — to stream
incremental batches of results via `$/progress` notifications when both
sides negotiate a `partialResultToken`. The final RPC response then
carries `null` (all items were already sent through progress).

This would let PHPantom deliver the _first_ useful results almost
instantly instead of blocking until every source has been scanned.

### Streaming between existing phases

`find_implementors` already runs five sequential phases (see
`docs/ARCHITECTURE.md` § Go-to-Implementation):

1. **Phase 1 — ast_map** (already-parsed classes in memory) — essentially
   free. Flush results immediately.
2. **Phase 2 — class_index** (FQN → URI entries not yet in ast_map) —
   loads individual files. Flush after each batch.
3. **Phase 3 — classmap files** (Composer classmap, user + vendor mixed)
   — iterates unique file paths, applies string pre-filter, parses
   matches. This is the widest phase and the best candidate for
   within-phase streaming (see below).
4. **Phase 4 — embedded stubs** (string pre-filter → lazy parse) — flush
   after stubs are checked.
5. **Phase 5 — PSR-4 directory walk** (user code only, catches files not
   in the classmap) — disk I/O + parse per file, good candidate for
   per-file streaming.

Each phase boundary is a natural point to flush a `$/progress` batch,
so the editor starts populating the results list while heavier phases
are still running.

### Prioritising user code within Phase 3

Phase 3 iterates the Composer classmap, which contains both user and
vendor entries. Currently they are processed in arbitrary order. A
simple optimisation: partition classmap file paths into user paths
(under PSR-4 roots from `composer.json` `autoload` / `autoload-dev`)
and vendor paths (everything else, typically under `vendor/`), then
process user paths first. This way the results most relevant to the
developer arrive before vendor matches, even within a single phase.

### Granularity options

- **Per-phase batches** (simplest) — one `$/progress` notification at
  each of the five phase boundaries listed above.
- **Per-file streaming** — within Phases 3 and 5, emit results as each
  file is parsed from disk instead of waiting for the entire phase to
  finish. Phase 3 can iterate hundreds of classmap files and Phase 5
  recursively walks PSR-4 directories, so per-file flushing would
  significantly improve perceived latency for large projects.
- **Adaptive batching** — collect results for a short window (e.g. 50 ms)
  then flush, balancing notification overhead against latency.

### Applicable requests

| Request                       | Benefit                                                                         |
| ----------------------------- | ------------------------------------------------------------------------------- |
| `textDocument/implementation` | Already scans five phases; each phase's matches can be streamed                 |
| `textDocument/references`     | Will need full-project scanning; streaming is essential                         |
| `workspace/symbol`            | Searches every known class/function; early batches feel instant                 |
| `textDocument/completion`     | Less critical (usually fast), but long chains through vendor code could benefit |

### Implementation sketch

1. Check whether the client sent a `partialResultToken` in the request
   params.
2. If yes, create a `$/progress` sender. After each scan phase (or
   per-file, depending on granularity), send a
   `ProgressParams { token, value: [items...] }` notification.
3. Return `null` as the final response.
4. If no token was provided, fall back to the current behaviour: collect
   everything, return once.

---

## F3. Incremental text sync

**Impact: Low-Medium · Effort: Medium**

PHPantom uses `TextDocumentSyncKind::FULL`, meaning every
`textDocument/didChange` notification sends the entire file content.
Switching to `TextDocumentSyncKind::INCREMENTAL` means the client sends
only the changed range (line/column start, line/column end, replacement
text), reducing IPC bandwidth for large files.

The practical benefit is bounded: Mago requires a full re-parse of the
file regardless of how the change was received, so the saving is purely
in the data transferred over the IPC channel. For files under ~1000
lines this is negligible. For very large files (5000+ lines, common in
legacy PHP), sending 200KB on every keystroke can become noticeable.

**Implementation:**

1. **Change the capability** — set `text_document_sync` to
   `TextDocumentSyncKind::INCREMENTAL` in `ServerCapabilities`.

2. **Apply diffs** — in the `did_change` handler, apply each
   `TextDocumentContentChangeEvent` to the stored file content string.
   The events contain a `range` (start/end position) and `text`
   (replacement). Convert positions to byte offsets and splice.

3. **Re-parse** — after applying all change events, re-parse the full
   file with Mago as today. No incremental parsing needed initially.

**Relationship with partial result streaming (F2):** These two features
address different performance axes. Incremental text sync reduces the
cost of _inbound_ data (client to server per keystroke). Partial result
streaming (F2) reduces the _perceived latency_ of _outbound_ results
(server to client for large result sets). They are independent and can
be implemented in either order, but if both are planned, incremental
text sync is lower priority because full-file sync is rarely the
bottleneck in practice. Partial result streaming has a more immediate
user-visible impact for go-to-implementation, find references, and
workspace symbols on large codebases.

---

## F4. Return type and closure parameter type inlay hints

**Impact: Medium · Effort: Medium**

PHPantom's inlay hints currently show **parameter names** and
**by-reference indicators** at call sites. Two additional hint kinds
would bring PHPantom to parity with Devsense and ahead of Intelephense:

### Return type hints

Show an inferred return type hint after the closing parenthesis of
functions, methods, closures, and arrow functions that lack an explicit
return type declaration:

```php
function doubled($x)  // → : int
{
    return $x * 2;
}

$fn = fn($x) => $x * 2;  // → : int
```

The hint should only appear when the return type can be inferred from
the function body (or from the callable context for closures). Functions
that already have a native return type hint or a `@return` docblock
should not receive a hint.

Ideally, clicking the hint (or double-clicking, depending on editor
support) should insert the return type declaration as a text edit.

### Closure / arrow function parameter type hints

Show an inferred type hint after untyped closure and arrow function
parameters when the type can be inferred from the callable context:

```php
$users->map(fn($u) => $u->name);
//            ^ : User

$filtered = array_filter($items, function ($item) { ... });
//                                         ^ : Item
```

The hint should only appear when the parameter has no native type hint
and the type is inferred from the enclosing callable signature (e.g.
a `Closure(User): bool` parameter type, or a `@param` on the receiving
function). Parameters that already have a type hint should not receive
a hint.

### What to avoid

- **Variable type hints at assignment sites.** Phpactor shows these
  (e.g. `$x` `: string` after every assignment). This is noisy in
  practice and clutters the editor. Do not add this kind.
- **End-of-block labels.** Phpactor shows `// class Foo` or
  `// method bar` at closing braces. This is an editor feature (most
  editors already show sticky scroll or breadcrumbs) and would add
  visual noise. Do not add this kind.

---

## F5. Call hierarchy

**Impact: Medium · Effort: Medium**

Implement `callHierarchy/incomingCalls` and
`callHierarchy/outgoingCalls` to answer "who calls this function?" and
"what does this function call?"

### Incoming calls (who calls this)

Given a function or method, find all call sites across the project.
This is conceptually similar to Find References but filtered to call
expressions and structured as a tree (each caller is itself a callable
with a location).

The existing Find References infrastructure
(`find_references_in_file`, cross-file scanning) provides the core
search. The call hierarchy handler wraps the results into
`CallHierarchyIncomingCall` items, grouping by containing function.

### Outgoing calls (what does this call)

Given a function or method, walk its AST body and collect all call
expressions (function calls, method calls, static calls, `new`
expressions). Resolve each callee to its declaration location.

This is a single-file AST walk with cross-file resolution for each
callee, similar to what go-to-definition already does.

### Prepare

`callHierarchy/prepare` returns a `CallHierarchyItem` for the symbol
at the cursor. This is straightforward: resolve the symbol, return its
name, kind, URI, range, and selection range.

### Dependencies

Call hierarchy benefits significantly from a full project index.
Without an index, incoming calls can only be found via the existing
classmap + PSR-4 scan approach (same as Find References). With a full
index (X4), the lookup becomes a simple index query.

Consider implementing after X4 (full background indexing) ships, or
accept the same scan-based latency that Find References currently has.
