# PHPantom — LSP Features

Items are ordered by **impact** (descending), then **effort** (ascending)
within the same impact tier.

| Label      | Scale                                                                                                                  |
| ---------- | ---------------------------------------------------------------------------------------------------------------------- |
| **Impact** | **Critical**, **High**, **Medium-High**, **Medium**, **Low-Medium**, **Low**                                           |
| **Effort** | **Low** (≤ 1 day), **Medium** (2-5 days), **Medium-High** (1-2 weeks), **High** (2-4 weeks), **Very High** (> 1 month) |

---

## F1. PHPDoc block generation on `/**`

**Impact: Medium-High · Effort: Medium · Status: implemented**

Both entry points (completion on `/**`, on-type formatting on Enter)
generate docblock skeletons following the enrichment-only rules below.
Template-parameter detection, `@extends`/`@implements` for class-likes,
space-aligned `@param` blocks, blank-line grouping, and `@var` for all
properties/constants are all implemented. 66 generation tests and 77
PHPDoc completion tests cover the behaviour.

### Delivery mechanism

Two entry points, both already wired up:

1. **Completion** (`try_generate_docblock`) — fires when the cursor is
   right after `/**` in editors that do not auto-close it. Returns a
   snippet `CompletionItem` with tab stops.
2. **On-type formatting** (`try_generate_docblock_on_enter`) — fires on
   Enter via `textDocument/onTypeFormatting`. Detects a freshly
   auto-generated empty `/** … */` block (VS Code, Zed, Neovim with
   auto-pairs) and replaces it with the filled docblock.

### What to generate — functions and methods

Only add tags that carry information the native type hints cannot
express. No special treatment for overrides — apply the same rules.

**`@param`** — include a tag when:

| Native type            | Action                                          |
| ---------------------- | ----------------------------------------------- |
| Missing                | `@param ${mixed} $name`                         |
| `array`                | `@param ${array} $name`                         |
| `Closure` / `callable` | `@param (${Closure()}) $name`                   |
| Union containing above | `@param array\|string $name` (echo raw type)    |
| Class with templates   | `@param ClassName<${T}> $name` (template names) |
| Anything else          | Skip (already fully expressed)                  |

Template detection: load the class via `class_loader`, check for
`@template` tags on it. Use the template parameter names as snippet
tab-stop placeholders (e.g. `Collection<${1:TKey}, ${2:TValue}>`).

`mixed`, `array`, and callable placeholders are tab stops so the user
can type over them immediately. Union types containing `array`,
`Closure`, or `callable` echo the raw type string so the user can
refine the array/callable portion.

**`@return`** — same logic as `@param`:

| Native return type     | Action                                     |
| ---------------------- | ------------------------------------------ |
| Missing                | `@return ${mixed}`                         |
| `void`                 | Skip                                       |
| `array`                | `@return ${array}`                         |
| `Closure` / `callable` | `@return (${Closure()})`                   |
| Union containing above | `@return array\|string` (echo raw type)    |
| Class with templates   | `@return ClassName<${T}>` (template names) |
| Anything else          | Skip                                       |

**`@throws`** — always add for every uncaught exception type detected
in the body. Reuse `find_uncaught_throw_types`. Auto-import via
`additional_text_edits` when the exception is not yet imported.

**Ordering and alignment:**

Tags are listed in this order, with a blank `*` separator line
between different tag groups (but not before the first group, and
not between tags of the same kind):

1. `@param` tags (all params together)
2. `@throws` tags
3. `@return`

No summary line is emitted when tags are present. When there are no
tags at all, generate a summary-only skeleton (`/**\n * \n */`) so
the user can type a description.

Within the `@param` block, parameter names are space-aligned:

```
 * @param Collection<int, Alert> $activeAlerts
 * @param string                 $reason
```

Right-pad the type string so that all `$name` tokens start at the same
column.

### What to generate — class-likes

Check whether the class `extends` or `implements` something whose
definition has `@template` parameters. If so, generate `@extends` /
`@implements` tags with the template parameter names as tab stops:

```
/**
 * @extends Factory<${1:TModel}>
 */
class UserFactory extends Factory
```

If neither parent nor interfaces have templates, generate a
summary-only skeleton: `/**\n * \n */`.

### What to generate — properties

Always generate `/** @var Type */` with the native type pre-filled so
the user has a starting point for adding a description.

| Native type          | Generated                     |
| -------------------- | ----------------------------- |
| Missing              | `/** @var ${mixed} */`        |
| `array`              | `/** @var ${array} */`        |
| Class with templates | `/** @var ClassName<${T}> */` |
| Any other type       | `/** @var thatType */`        |

### What to generate — constants

Same as properties: `/** @var Type */` with the declared type or
`${mixed}` if untyped.

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
