//! Performance benchmarks for `textDocument/references`.
//!
//! Run with: `cargo bench --bench references`
//!
//! These exercise the four hot paths the recent reference-search-speedup
//! work targets:
//!
//! 1. **Warm class references** — the inverted `ReferenceIndex` is
//!    pre-populated for every file in the workspace; `find_references`
//!    is just a hash-key lookup plus location materialisation.
//! 2. **Warm member references with hierarchy filter** — exercises the
//!    cached `subject_fqns` path, since every call site has a
//!    statically-resolvable subject (`$instance->foo()` where `$instance`
//!    is typed by `new Target()`).
//! 3. **Warm common-method references** — many classes share a method
//!    name (`__construct`); the index returns a large candidate set and
//!    the per-entry hierarchy filter dominates.
//! 4. **Cold workspace walk** — only the file containing the target is
//!    opened up front; the first `find_references` call triggers
//!    `ensure_reference_candidates_indexed`, which walks the whole
//!    workspace, prefilters by needle (memchr-backed), and parses
//!    matching files. This is the cost paid on the user's *first*
//!    Find References after launching the editor.
//!
//! Setup uses real on-disk temp directories (via `tempfile`) so the
//! workspace-walk paths actually exercise gitignore-aware file
//! discovery, not just in-memory `did_open` state.
//!
//! `Backend::update_ast` is called directly (synchronously) instead of
//! going through async `did_open`, which both keeps benchmarks
//! single-threaded and matches the pattern used in
//! `bench_diagnostics_phpactor_fixtures` in `benches/completion.rs`.

use std::path::Path;

use criterion::{BatchSize, Criterion, black_box, criterion_group, criterion_main};
use phpantom_lsp::Backend;
use tempfile::TempDir;
use tower_lsp::lsp_types::Position;

// ─── Workspace generators ──────────────────────────────────────────────────

/// Build a temp workspace with one `Target` class plus `caller_count`
/// `Caller{i}` classes that each construct and call a method on `Target`.
///
/// `hot_method` is the method name used in both the declaration and the
/// call sites, so callers can pick a unique name (warm benchmarks) or a
/// shared name like `__construct` (common-name benchmarks).
fn build_target_workspace(caller_count: usize, hot_method: &str) -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");

    std::fs::write(
        dir.path().join("Target.php"),
        format!(
            "<?php\n\
class Target {{\n\
    public function {hot}(): void {{}}\n\
    public function untouched(): void {{}}\n\
}}\n",
            hot = hot_method,
        ),
    )
    .expect("write Target.php");

    for i in 0..caller_count {
        std::fs::write(
            dir.path().join(format!("Caller{i}.php")),
            format!(
                "<?php\n\
class Caller{i} {{\n\
    public function run(): void {{\n\
        $t = new Target();\n\
        $t->{hot}();\n\
    }}\n\
}}\n",
                hot = hot_method,
            ),
        )
        .expect("write Caller.php");
    }

    dir
}

/// Build a temp workspace where `Target` extends a chain of base classes
/// and every method call on the leaf goes through `$this`, so the cached
/// `subject_fqns` path (rather than the variable fallback) is exercised.
fn build_this_workspace(method_count: usize, hot_method: &str) -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");

    std::fs::write(
        dir.path().join("Target.php"),
        format!(
            "<?php\n\
class Target {{\n\
    public function {hot}(): void {{}}\n\
}}\n",
            hot = hot_method,
        ),
    )
    .expect("write Target.php");

    // One file with `method_count` methods, each calling `$this->{hot}()`.
    let mut leaf = String::from("<?php\nclass Leaf extends Target {\n");
    for i in 0..method_count {
        leaf.push_str(&format!(
            "    public function caller{i}(): void {{ $this->{hot}(); }}\n",
            hot = hot_method,
        ));
    }
    leaf.push_str("}\n");
    std::fs::write(dir.path().join("Leaf.php"), leaf).expect("write Leaf.php");

    dir
}

/// Build a workspace where `caller_count` classes each declare *their own*
/// `__construct`, plus a `Target` class with `__construct`. Used to stress
/// the "common method name" path: the inverted index returns one entry per
/// caller's `__construct`, and the hierarchy filter is what narrows the
/// result down to just the `Target` callers.
fn build_common_method_workspace(caller_count: usize) -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");

    std::fs::write(
        dir.path().join("Target.php"),
        "<?php\n\
class Target {\n\
    public function __construct() {}\n\
    public function work(): void {}\n\
}\n",
    )
    .expect("write Target.php");

    for i in 0..caller_count {
        // Half the callers actually use Target::__construct via `new
        // Target()`; the other half declare their own `__construct` to
        // simulate name collisions across the workspace.
        let body = if i % 2 == 0 {
            format!(
                "<?php\n\
class Caller{i} {{\n\
    public function run(): void {{\n\
        $t = new Target();\n\
        $t->work();\n\
    }}\n\
}}\n"
            )
        } else {
            format!(
                "<?php\n\
class Caller{i} {{\n\
    public function __construct() {{}}\n\
}}\n"
            )
        };
        std::fs::write(dir.path().join(format!("Caller{i}.php")), body).expect("write Caller.php");
    }

    dir
}

/// Open every PHP file under `root` on the backend via `update_ast`,
/// which both populates `symbol_maps` and the inverted `ReferenceIndex`
/// for that URI. After this returns the warm-cache hot path is fully
/// primed and `find_references` runs without any disk walk.
fn warm_open_all(backend: &Backend, root: &Path) {
    for entry in walk_php_files(root) {
        let content = std::fs::read_to_string(&entry).expect("read fixture");
        let uri = format!("file://{}", entry.display());
        backend.update_ast(&uri, &content);
        // Ensure get_file_content (used inside find_references) returns
        // the parsed source by registering it as an "open" file.
        backend
            .open_files()
            .write()
            .insert(uri, std::sync::Arc::new(content));
    }
}

fn walk_php_files(root: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let entries = std::fs::read_dir(root).expect("read_dir");
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("php") {
            out.push(path);
        }
    }
    out
}

// ─── Benchmarks ────────────────────────────────────────────────────────────

const WARM_CALLER_COUNT: usize = 50;
const COLD_CALLER_COUNT: usize = 100;
const COMMON_METHOD_CALLER_COUNT: usize = 50;

/// Warm find-references on the `Target` class declaration. The inverted
/// index is fully populated; this measures the post-index lookup cost.
fn bench_references_warm_class(c: &mut Criterion) {
    let dir = build_target_workspace(WARM_CALLER_COUNT, "uniqueMethodName");
    let backend = Backend::new_test_with_workspace(dir.path().to_path_buf(), Vec::new());
    warm_open_all(&backend, dir.path());

    let target_path = dir.path().join("Target.php");
    let target_uri = format!("file://{}", target_path.display());
    let target_content = std::fs::read_to_string(&target_path).expect("read Target.php");

    // Cursor on `Target` class name (line 1, character 6).
    let position = Position::new(1, 6);

    c.bench_function("references_warm_class_50_files", |b| {
        b.iter(|| {
            let _ = black_box(backend.find_references(
                &target_uri,
                &target_content,
                position,
                /* include_declaration */ true,
            ));
        })
    });
}

/// Warm find-references on the `uniqueMethodName` method declaration
/// where every call site is `$variable->uniqueMethodName()`. This goes
/// through the variable-subject fallback path (subject_fqns == None),
/// so each entry costs a `resolve_subject_to_fqns` call against the
/// type engine.
fn bench_references_warm_method_variable_subject(c: &mut Criterion) {
    let dir = build_target_workspace(WARM_CALLER_COUNT, "uniqueMethodName");
    let backend = Backend::new_test_with_workspace(dir.path().to_path_buf(), Vec::new());
    warm_open_all(&backend, dir.path());

    let target_path = dir.path().join("Target.php");
    let target_uri = format!("file://{}", target_path.display());
    let target_content = std::fs::read_to_string(&target_path).expect("read Target.php");

    // Cursor on `uniqueMethodName` declaration (line 2, character 20).
    let position = Position::new(2, 20);

    c.bench_function("references_warm_method_variable_subject", |b| {
        b.iter(|| {
            let _ =
                black_box(backend.find_references(&target_uri, &target_content, position, true));
        })
    });
}

/// Warm find-references on a method whose call sites all use `$this`.
/// Hits the cached `subject_fqns` path: index lookup → hash check →
/// push from `entry.range`. Should be close to the class-references
/// baseline because no symbol-map or content access is needed per
/// entry.
fn bench_references_warm_method_this_subject(c: &mut Criterion) {
    let dir = build_this_workspace(WARM_CALLER_COUNT, "uniqueMethodName");
    let backend = Backend::new_test_with_workspace(dir.path().to_path_buf(), Vec::new());
    warm_open_all(&backend, dir.path());

    let target_path = dir.path().join("Target.php");
    let target_uri = format!("file://{}", target_path.display());
    let target_content = std::fs::read_to_string(&target_path).expect("read Target.php");

    // Cursor on `uniqueMethodName` declaration (line 2, character 20).
    let position = Position::new(2, 20);

    c.bench_function("references_warm_method_this_subject", |b| {
        b.iter(|| {
            let _ =
                black_box(backend.find_references(&target_uri, &target_content, position, true));
        })
    });
}

/// Warm find-references on `__construct`, where many unrelated classes
/// also declare a `__construct`. The index returns ~Cclasses entries;
/// the per-entry hierarchy filter does the real work.
fn bench_references_warm_common_method(c: &mut Criterion) {
    let dir = build_common_method_workspace(COMMON_METHOD_CALLER_COUNT);
    let backend = Backend::new_test_with_workspace(dir.path().to_path_buf(), Vec::new());
    warm_open_all(&backend, dir.path());

    let target_path = dir.path().join("Target.php");
    let target_uri = format!("file://{}", target_path.display());
    let target_content = std::fs::read_to_string(&target_path).expect("read Target.php");

    // Cursor on `__construct` declaration (line 2, character 20).
    let position = Position::new(2, 20);

    c.bench_function("references_warm_common_method_name", |b| {
        b.iter(|| {
            let _ =
                black_box(backend.find_references(&target_uri, &target_content, position, true));
        })
    });
}

/// Cold find-references: only `Target.php` is open up front; the first
/// `find_references` call triggers the workspace walk + memchr-backed
/// prefilter + parallel parse of matching files. This is what the user
/// actually pays for the *first* Find References after launching the
/// editor on a fresh project.
///
/// Each iteration uses a fresh backend so the workspace-indexed flag
/// resets and the walk runs again.
fn bench_references_cold_workspace_walk(c: &mut Criterion) {
    let dir = build_target_workspace(COLD_CALLER_COUNT, "uniqueMethodName");
    let workspace_root = dir.path().to_path_buf();
    let target_path = dir.path().join("Target.php");
    let target_uri = format!("file://{}", target_path.display());
    let target_content = std::fs::read_to_string(&target_path).expect("read Target.php");

    let position = Position::new(1, 6); // Cursor on `Target` class name.

    c.bench_function("references_cold_workspace_walk_100_files", |b| {
        b.iter_batched(
            || {
                let backend = Backend::new_test_with_workspace(workspace_root.clone(), Vec::new());
                // Only Target.php is opened — Callers stay on disk so the
                // workspace walk has to discover and parse them.
                backend.update_ast(&target_uri, &target_content);
                backend.open_files().write().insert(
                    target_uri.clone(),
                    std::sync::Arc::new(target_content.clone()),
                );
                backend
            },
            |backend| {
                let _ = black_box(backend.find_references(
                    &target_uri,
                    &target_content,
                    position,
                    true,
                ));
            },
            BatchSize::PerIteration,
        )
    });
}

criterion_group!(
    benches,
    bench_references_warm_class,
    bench_references_warm_method_variable_subject,
    bench_references_warm_method_this_subject,
    bench_references_warm_common_method,
    bench_references_cold_workspace_walk,
);
criterion_main!(benches);
