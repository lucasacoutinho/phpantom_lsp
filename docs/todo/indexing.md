# PHPantom — Indexing & File Discovery

This document covers how PHPantom discovers, parses, and caches class
definitions across the workspace. The goal is to remain fast and
lightweight by default while offering progressively richer modes for
users who want exhaustive workspace intelligence.

---

## Current state

PHPantom relies on Composer's generated `autoload_classmap.php` for
cross-file class resolution. This works well when the classmap is
present and up to date, but fails silently when:

- The user has not run `composer dump-autoload -o`.
- The classmap is stale (new classes added since last dump).
- The project does not use Composer at all.

Diagnostics run asynchronously but still trigger a cascade of lazy
stub and vendor file parses on first open. This is fast enough to not
block the editor, but contributes to memory growth and delays
diagnostic results.

Find References relies on `collect_php_files_gitignore` to walk the
entire workspace directory sequentially, then `update_ast` each file
one at a time. Go-to-Implementation walks Composer PSR-4 directories
via `collect_php_files`. Both process files sequentially and on large
codebases this is noticeably slow.

---

## Strategy modes

Four indexing strategies, selectable via `.phpantom.toml`:

```toml
[indexing]
# "composer" (default) - use composer classmap, self-scan on fallback
# "self"    - always self-scan, ignore composer classmap
# "full"    - background-parse all project files for rich intelligence
# "none"    - no proactive scanning
strategy = "composer"
```

### `"composer"` (default)

Use Composer's classmap when available and complete. Fall back to
self-scan when the classmap is missing or incomplete. This is the
zero-config experience.

### `"self"`

Always build the classmap ourselves. Ignores `autoload_classmap.php`
entirely. For users who refuse to run `composer dump-autoload -o` or
who are actively editing `composer.json` dependencies.

### `"full"`

Background-parse every PHP file in the project. Uses Composer data to
guide file discovery when available, falls back to scanning all PHP
files in the workspace when it is not. Populates the ast_map,
symbol_maps, and all derived indices. Enables workspace symbols, fast
find-references without on-demand scanning, and rich hover on
completion items. Memory usage grows proportionally to project size.

### `"none"`

No proactive file scanning. Still uses Composer's classmap if present,
still resolves classes on demand when the user triggers completion or
hover, still has embedded stubs. The only difference from `"composer"`
is that it never falls back to self-scan when the classmap is missing
or incomplete. This is essentially what PHPantom does today.

---

## Phase 1: Self-generated classmap

**Goal:** When the Composer classmap is missing or incomplete, build
one ourselves so the user gets cross-file resolution without manual
steps. This is the top priority for 0.5.0.

### Detection

1. Check whether `vendor/composer/autoload_classmap.php` exists.
2. If it exists, check whether it contains namespace prefixes from
   `composer.json`'s PSR-4 mappings. If the PSR-4 config lists
   namespaces that are absent from the classmap, the classmap is
   likely from a non-optimized dump and is incomplete.
3. If the classmap is missing or incomplete, fall back to self-scan.

### Self-scan implementation

Mirror what Composer does: walk directories listed in
`composer.json`'s `autoload` section (PSR-4, classmap entries) and
vendor packages, read each `.php` file, extract `namespace\ClassName`
pairs, and populate `self.classmap`.

The scanner should be a single-pass byte-level state machine (not a
full AST parse). Composer uses `php_strip_whitespace()` followed by a
regex; we can do better with a direct scan that skips
strings/comments/heredocs inline. Libretto's `FastScanner::find_classes`
is a good reference implementation: ~300 lines, handles all PHP edge
cases, uses `memchr` for SIMD-accelerated keyword detection.

The scan has two parts:

**User files** (from `composer.json`):
- `autoload.psr-4` directories (with PSR-4 compliance filtering).
- `autoload.classmap` directories.
- `autoload-dev.psr-4` and `autoload-dev.classmap` directories.

**Vendor files** (from `vendor/composer/installed.json`):
- Each installed package's autoload directories.

`composer.json` only describes the user's own code. Vendor package
locations come from `installed.json`, which Composer writes into the
vendor directory and users never touch. If `installed.json` does not
exist (the user hasn't run `composer install` yet), we can only scan
what `composer.json` describes. A file watcher will pick up vendor
files once they appear, but that is Phase 2 work. For 0.5.0 the user
restarts the LSP after installing dependencies.

PSR-0 support is deferred. It can be plugged in later with minimal
effort but covers very few modern packages.

### Non-Composer projects

When no `composer.json` exists at all, self-scan falls back to walking
all PHP files under the workspace root (excluding hidden directories).
This produces a classmap with no PSR-4 compliance filtering. It will
pick up some irrelevant files but provides basic cross-file resolution
for projects that don't use Composer.

### Output

The result is a `HashMap<String, PathBuf>` in the same format as the
existing `self.classmap`. Everything downstream (resolution,
diagnostics, go-to-definition) works unchanged.

### User feedback

When self-scan is triggered, log a message:
> PHPantom: Building class index. Run `composer dump-autoload -o` for
> faster startup.

If the user has `strategy = "self"` configured, skip the suggestion.

### Reference material

- `composer/class-map-generator` (`PhpFileParser.php`,
  `PhpFileCleaner.php`) is the source of truth for what Composer
  actually does. Our scanner must produce the same classmap for the
  same input.
- Libretto's `libretto-autoloader` crate (`fast_parser.rs`,
  `scanner.rs`) is a working Rust implementation of the same logic.
  MIT licensed, uses `mago-syntax` and `rayon`. The `FastScanner`
  (byte-level, no AST) is the right model for Phase 1. The full
  `Parser` (mago-syntax AST) is overkill for classmap generation.
- Libretto's `IncrementalCache` (`lib.rs`) uses mtime + semantic
  fingerprint tracking with `rkyv` serialization. Worth evaluating
  if/when we add disk caching, but not needed for Phase 1 where the
  in-memory scan should be fast enough.

---

## Phase 2: Staleness detection and auto-refresh

**Goal:** Keep the classmap fresh without user intervention.

### Trigger points

- On `workspace/didChangeWatchedFiles`: if `composer.json` or
  `composer.lock` changed, schedule a rescan of vendor directories
  (the user likely ran `composer install` or `composer update`).
- On `did_save` of a PHP file: if the file is in a PSR-4 directory,
  do a targeted single-file rescan (read the file, extract class
  names, update the classmap entry). This is cheap enough to do
  synchronously.

### Targeted refresh

For single-file changes, re-scan only that file and update/remove its
classmap entries. No need to rescan the entire workspace.

For dependency changes (vendor rescan), this is the expensive case but
happens rarely (a few times per day at most).

---

## Phase 3: Parallel file processing

**Goal:** Speed up workspace-wide operations (find references,
go-to-implementation, self-scan, diagnostics) by processing files in
parallel with priority awareness.

### Why not rayon?

`rayon` is the obvious choice for "process N files in parallel" and
Libretto uses it successfully. But it runs its own thread pool
separate from tokio's runtime. When rayon saturates all cores on a
batch scan, tokio's async tasks (completion, hover, signature help)
get starved for CPU time. There is no clean way to pause a rayon
batch when a high-priority LSP request arrives.

### Why the classmap is not a prerequisite

The classmap is a convenience for O(1) class lookup and class name
completion. But most resolution already works on demand via PSR-4
(derive path from namespace, check if file exists). Class name
completion is a minor subset of what users actually trigger. This
means classmap generation can run at normal priority without blocking
the user. They can start writing code immediately while the classmap
builds in the background.

### Architecture

A single `tokio::spawn_blocking` pool with priority-aware scheduling.
All file processing goes through one system:

- **High:** Files needed for an active LSP request (completion, hover,
  go-to-definition). The user is typing. Serve immediately.
- **Normal:** Classmap generation, find-references, go-to-
  implementation scans. Important but can wait.
- **Low:** Full background indexing (Phase 5) and diagnostics. Runs
  when nothing else needs attention.

When nothing is queued at High, Normal runs at full speed. When
nothing is at Normal, Low gets all resources. The moment a completion
request arrives, Low pauses, High is served, Low resumes. No idle
waste, no starvation.

Implementation: a priority channel or a simple check-before-spawn
pattern. Before spawning the next Low task, check whether any High or
Normal work is pending. This keeps us in one thread pool, one
scheduling model, no contention between separate runtimes.

### Quick wins before full parallelism

- Pre-filter files before reading them. For find-references looking
  for class `Foo`, use the classmap to find which files define `Foo`,
  then only scan files that contain the string "Foo". This is what
  we already do for GTI but could be applied more broadly.
- Use `memmap2` for reading files instead of `read_to_string` when
  scanning large directories. Avoids copying file contents into
  userspace when the OS page cache already has them.

---

## Phase 4: Completion item detail on demand

**Goal:** Show type signatures, docblock descriptions, and
deprecation info in completion item hover without parsing every
possible class up front.

### Current limitation

When completion shows `SomeClass::doThing()`, hovering over that item
in the completion menu shows nothing because we haven't parsed
`SomeClass`'s file yet. Parsing it on demand would be fine for one
item, but the editor may request resolve for dozens of items as the
user scrolls.

### Approach: "what's already discovered"

Use `completionItem/resolve` to populate `detail` and
`documentation` fields. If the class is already in the ast_map (parsed
during a prior resolution), return the full signature and docblock.
If not, return just the item label with no extra detail.

In `"full"` mode, everything is already parsed, so every completion
item gets rich hover for free. In `"composer"` / `"self"` mode, items
that happen to have been resolved earlier in the session get rich
detail; others don't. This is a graceful degradation that never blocks
the completion response.

### Future: speculative background parsing

When a completion list is generated, queue the unresolved classes for
background parsing at low priority. If the user lingers on the
completion menu, resolved items will progressively gain detail. This
is a nice-to-have, not a requirement.

---

## Phase 5: Full background indexing

**Goal:** Parse every PHP file in the project in the background,
enabling workspace symbols, fast find-references without on-demand
scanning, and complete completion item detail.

### Trigger

When `strategy = "full"` is set in `.phpantom.toml`.

### Design: self + second pass

Full mode is not a separate discovery system. It works exactly like
`"self"` mode (Phase 1) and then schedules a second pass:

1. **First pass (same as self):** Build the classmap via byte-level
   scanning. This completes in about a second and gives us class
   name completion and O(1) file lookup.
2. **Second pass:** Iterate every file path in the now-populated
   in-memory classmap and call `update_ast` on each one at Low
   priority. This populates ast_map, symbol_maps, class_index,
   global_functions, and global_defines.

No new file discovery logic is needed. The classmap from the first
pass already contains every relevant file path. The second pass just
enriches it.

When `composer.json` does not exist (e.g. the user opened a monorepo
root or a non-Composer project), the first pass falls back to walking
all PHP files under the workspace root, so the second pass still has
a complete file list to work from.

### Progressive enrichment

The user experiences three stages:

1. **Immediate:** LSP requests are up and running. Completion, hover,
   and go-to-definition work via on-demand resolution and stubs.
2. **Seconds:** Classmap is ready. Class name completion covers the
   full project. Cross-file resolution is O(1).
3. **Under a minute:** Full AST parse complete. Workspace symbols,
   fast find-references (no on-demand scanning), rich hover on
   completion items.

Each stage improves on the last without blocking the previous one.

### Behaviour

1. Respect the priority system from Phase 3: pause the second pass
   when higher-priority work arrives.
2. Process user code first, then vendor.
3. Report progress via `$/progress` tokens so the editor can show
   "Indexing: 1,234 / 5,678 files".

### Memory

Currently we store `ClassInfo`, `FunctionInfo`, and `SymbolMap`
structs that are not as lean as they could be. For a 21K-file
codebase, full indexing will use meaningful RAM. This is acceptable
because it's an opt-in mode, but we should profile and trim struct
sizes over time. The aim is to stay under 512MB for a full project.

### Workspace symbols

With the full index populated, `workspace/symbol` becomes a simple
filter over the ast_map and global_functions maps. No additional
infrastructure needed.

In other modes, workspace symbols still works but only returns results
from already-parsed files (opened files, on-demand resolutions, stubs).
When the user invokes workspace symbols outside of full mode, show a
one-time hint suggesting they enable `strategy = "full"` in
`.phpantom.toml` for complete coverage.

---

## Phase 6: Disk cache (evaluate later)

**Goal:** Persist the full index to disk so that restarts don't
require a full rescan.

### When to consider

Only if Phase 5 background indexing is slow enough on cold start that
users complain. Given that:
- Mago can lint 45K files in 2 seconds.
- A regex classmap scan over 21K files should be sub-second.
- Full AST parsing of a few thousand user files should take single
  digit seconds.

...disk caching may never justify its complexity. The primary use
case would be memory savings (load from disk on demand instead of
holding everything in RAM), not startup speed.

### Format options

- `bincode` / `postcard`: simple, small dependency footprint, tolerant
  of struct changes (deserialization fails gracefully instead of
  reading garbage memory). The right default choice.
- SQLite: robust, queryable, but heavier than needed for a flat
  key-value store.

Zero-copy formats like `rkyv` are ruled out. They map serialized bytes
directly into memory as if they were the original structs, which means
any struct layout change between versions reads corrupt data. PHPantom's
internal types change frequently and will continue to do so. A cache
format that silently produces garbage after an update is worse than no
cache at all.

### Invalidation

Store file mtime + content hash per entry. On startup, walk the
directory, compare mtimes, re-parse only changed files. This is
Libretto's `IncrementalCache` approach and it works well.

### Decision criteria

Implement disk caching only if:
1. Full-mode cold start exceeds 10 seconds on a representative large
   codebase, AND
2. The memory overhead of holding the full index exceeds the 512 MB
   target, or users on constrained systems report issues.

If neither condition is met, skip this phase entirely. Simpler is
better.