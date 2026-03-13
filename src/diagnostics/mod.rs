//! Diagnostics — publish LSP diagnostics for PHP files.
//!
//! This module collects diagnostics from multiple providers and publishes
//! them via `textDocument/publishDiagnostics`.  Currently implemented:
//!
//! - **`@deprecated` usage diagnostics** — report references to symbols
//!   marked `@deprecated` with `DiagnosticTag::Deprecated` (renders as
//!   strikethrough in most editors).
//! - **Unused `use` dimming** — dim `use` declarations that are not
//!   referenced anywhere in the file with `DiagnosticTag::Unnecessary`.
//! - **Unknown class diagnostics** — report `ClassReference` spans that
//!   cannot be resolved through any resolution phase (use-map, local
//!   classes, same-namespace, class_index, classmap, PSR-4, stubs).
//! - **Unknown member diagnostics** — report `MemberAccess` spans where
//!   the member does not exist on the resolved class after full
//!   resolution (inheritance + virtual member providers).  Suppressed
//!   when the class has `__call` / `__callStatic` / `__get` magic methods.
//! - **Unresolved member access diagnostics** (opt-in) — report
//!   `MemberAccess` spans where the **subject type** cannot be resolved
//!   at all.  Off by default; enable via `[diagnostics]
//!   unresolved-member-access = true` in `.phpantom.toml`.  Uses
//!   `Severity::HINT` to surface type-coverage gaps without drowning
//!   the editor in warnings.
//!
//! Diagnostics are published **asynchronously** via [`Backend::schedule_diagnostics`].
//! On every `did_change` event a version counter is bumped and the
//! diagnostic worker is notified.  The worker debounces rapid edits
//! (waits [`DIAGNOSTIC_DEBOUNCE_MS`] after the last notification) and
//! then runs a single diagnostic pass.  At most one pass runs at a time;
//! if new edits arrive while a pass is in flight, a single follow-up
//! pass is scheduled once the current one finishes.  This two-slot
//! design (one running, one pending) ensures diagnostics never block
//! completion, hover, or other latency-sensitive requests.

mod deprecated;
pub(crate) mod helpers;
pub(crate) mod unknown_classes;
pub(crate) mod unknown_functions;
pub(crate) mod unknown_members;
pub(crate) mod unresolved_member_access;
mod unused_imports;

use std::sync::atomic::Ordering;

use tower_lsp::lsp_types::*;

use crate::Backend;

/// How long to wait after the last keystroke before publishing diagnostics.
const DIAGNOSTIC_DEBOUNCE_MS: u64 = 500;

impl Backend {
    /// Collect all diagnostics for a single file and publish them.
    ///
    /// Called from the diagnostic worker task spawned by
    /// [`schedule_diagnostics`](Self::schedule_diagnostics).  Both
    /// `did_open` and `did_change` schedule diagnostics asynchronously
    /// so that lazy stub parsing (which can trigger hundreds of
    /// cache-miss parses on first open) never blocks the LSP response.
    ///
    /// `uri_str` is the file URI string (e.g. `"file:///path/to/file.php"`).
    /// `content` is the full text of the file.
    pub(crate) async fn publish_diagnostics_for_file(&self, uri_str: &str, content: &str) {
        let client = match &self.client {
            Some(c) => c,
            None => return,
        };

        // Skip diagnostics for stub files — they are internal.
        if uri_str.starts_with("phpantom-stub://") || uri_str.starts_with("phpantom-stub-fn://") {
            return;
        }

        // Skip diagnostics for vendor files — they are third-party code
        // and should not produce warnings in the user's editor.  The
        // vendor URI prefixes are built during `initialized` from the
        // workspace root and each subproject's `config.vendor-dir`.
        {
            let prefixes = self.vendor_uri_prefixes.lock();
            if prefixes.iter().any(|p| uri_str.starts_with(p.as_str())) {
                return;
            }
        }

        let uri = match uri_str.parse::<Url>() {
            Ok(u) => u,
            Err(_) => return,
        };

        // ── Phase 1: fast diagnostics (cheap, no type resolution) ───────
        // Publish these immediately together with the *previous* slow
        // diagnostics so the editor dims unused imports and strikes
        // through deprecated symbols without waiting for the heavier
        // collectors, and without flickering away existing warnings.
        let mut fast_diagnostics = Vec::new();

        // ── @deprecated usage diagnostics ───────────────────────────────
        self.collect_deprecated_diagnostics(uri_str, content, &mut fast_diagnostics);

        // ── Unused `use` dimming ────────────────────────────────────────
        self.collect_unused_import_diagnostics(uri_str, content, &mut fast_diagnostics);

        // Merge fresh fast diagnostics with stale slow diagnostics so
        // the editor never shows a gap where slow diagnostics vanish
        // and then reappear.
        let mut phase1 = fast_diagnostics.clone();
        {
            let cache = self.diag_last_slow.lock();
            if let Some(prev_slow) = cache.get(uri_str) {
                phase1.extend(prev_slow.iter().cloned());
            }
        }
        deduplicate_diagnostics(&mut phase1);
        client.publish_diagnostics(uri.clone(), phase1, None).await;

        // ── Phase 2: slow diagnostics (require type resolution) ─────────
        let mut slow_diagnostics = Vec::new();

        // ── Unknown class references ────────────────────────────────────
        self.collect_unknown_class_diagnostics(uri_str, content, &mut slow_diagnostics);

        // ── Unknown member access ───────────────────────────────────────
        self.collect_unknown_member_diagnostics(uri_str, content, &mut slow_diagnostics);

        // ── Unknown function calls ──────────────────────────────────────
        self.collect_unknown_function_diagnostics(uri_str, content, &mut slow_diagnostics);

        // ── Unresolved member access (opt-in) ───────────────────────────
        self.collect_unresolved_member_access_diagnostics(uri_str, content, &mut slow_diagnostics);

        // Cache the fresh slow diagnostics for the next fast-phase merge.
        {
            let mut cache = self.diag_last_slow.lock();
            cache.insert(uri_str.to_string(), slow_diagnostics.clone());
        }

        // ── Combine fast + slow and deduplicate ─────────────────────────
        let mut diagnostics = fast_diagnostics;
        diagnostics.extend(slow_diagnostics);

        // When multiple providers flag the same span, suppress the
        // lower-value diagnostic.  In particular, suppress
        // `unresolved_member_access` (Hint) when a more specific
        // `unknown_member`, `scalar_member_access`, or `unknown_class`
        // diagnostic already covers an overlapping range.
        deduplicate_diagnostics(&mut diagnostics);

        client.publish_diagnostics(uri, diagnostics, None).await;
    }

    /// Notify the diagnostic worker that new work is available.
    ///
    /// Bumps the diagnostic version counter and wakes the worker.
    /// The worker will debounce rapid calls (waiting
    /// [`DIAGNOSTIC_DEBOUNCE_MS`] after the *last* notification) and
    /// then run a single diagnostic pass.
    ///
    /// This returns immediately — all diagnostic computation happens
    /// in the background so that completion, hover, and signature help
    /// are never blocked.
    pub(crate) fn schedule_diagnostics(&self, uri: String) {
        {
            let mut pending = self.diag_pending_uris.lock();
            if !pending.contains(&uri) {
                pending.push(uri);
            }
        }
        // Bump version so the worker knows there is fresh work.
        self.diag_version.fetch_add(1, Ordering::Release);
        // Wake the worker (no-op if it is already awake).
        self.diag_notify.notify_one();
    }

    /// Schedule a diagnostic pass for every currently open file.
    ///
    /// Called when a class signature changes in one file, because
    /// diagnostics in other open files (unknown member, unknown class,
    /// deprecated usage) may depend on the changed class.  The edited
    /// file itself is excluded (it is already scheduled by the caller).
    pub(crate) fn schedule_diagnostics_for_open_files(&self, exclude_uri: &str) {
        let uris: Vec<String> = self
            .open_files
            .read()
            .keys()
            .filter(|u| u.as_str() != exclude_uri)
            .cloned()
            .collect();
        if uris.is_empty() {
            return;
        }
        {
            let mut pending = self.diag_pending_uris.lock();
            for uri in uris {
                if !pending.contains(&uri) {
                    pending.push(uri);
                }
            }
        }
        self.diag_version.fetch_add(1, Ordering::Release);
        self.diag_notify.notify_one();
    }

    /// Long-lived background task that processes diagnostic requests.
    ///
    /// Spawned once during `initialized`.  Loops forever, waiting for
    /// [`schedule_diagnostics`](Self::schedule_diagnostics) to signal
    /// new work.  On each iteration:
    ///
    /// 1. Wait for a notification (new edit arrived).
    /// 2. Debounce: sleep [`DIAGNOSTIC_DEBOUNCE_MS`], then check
    ///    whether the version counter moved (more edits).  If so,
    ///    loop back to step 2.
    /// 3. Snapshot the pending URI and current file content.
    /// 4. Run the diagnostic collectors and publish results.
    /// 5. Loop back to step 1.
    ///
    /// Because there is exactly one instance of this task, at most one
    /// diagnostic pass runs at a time.  If edits arrive during step 4
    /// the version counter will have moved, and step 1 picks up
    /// immediately after step 4 finishes — giving the two-slot
    /// (one running + one pending) behaviour.
    pub(crate) async fn diagnostic_worker(&self) {
        loop {
            // ── Step 1: wait for work ───────────────────────────────
            self.diag_notify.notified().await;

            // ── Step 2: debounce ────────────────────────────────────
            loop {
                let version_before = self.diag_version.load(Ordering::Acquire);
                tokio::time::sleep(std::time::Duration::from_millis(DIAGNOSTIC_DEBOUNCE_MS)).await;
                let version_after = self.diag_version.load(Ordering::Acquire);
                if version_before == version_after {
                    // No new edits during the sleep — proceed.
                    break;
                }
                // More edits arrived — loop and debounce again.
            }

            // ── Step 3: snapshot all pending URIs ────────────────────
            let uris: Vec<String> = {
                let mut pending = self.diag_pending_uris.lock();
                std::mem::take(&mut *pending)
            };
            if uris.is_empty() {
                continue;
            }

            // ── Step 4: collect and publish for each URI ────────────
            // Snapshot content for each URI individually, releasing the
            // read lock before each async publish call so that
            // `did_change` is never blocked.
            for uri in &uris {
                let content = {
                    let files = self.open_files.read();
                    match files.get(uri) {
                        Some(c) => c.clone(),
                        None => continue,
                    }
                };
                self.publish_diagnostics_for_file(uri, &content).await;
            }
        }
    }

    /// Clear diagnostics for a file (e.g. on `did_close`).
    pub(crate) async fn clear_diagnostics_for_file(&self, uri_str: &str) {
        // Remove cached slow diagnostics so we don't leak memory.
        self.diag_last_slow.lock().remove(uri_str);

        let client = match &self.client {
            Some(c) => c,
            None => return,
        };

        let uri = match uri_str.parse::<Url>() {
            Ok(u) => u,
            Err(_) => return,
        };

        client.publish_diagnostics(uri, Vec::new(), None).await;
    }
}

/// Suppress redundant diagnostics when a more specific provider already
/// covers the same (or overlapping) range.
///
/// Rules:
/// - Drop `unresolved_member_access` when any `unknown_member`,
///   `scalar_member_access`, or `unknown_class` diagnostic overlaps.
fn deduplicate_diagnostics(diagnostics: &mut Vec<Diagnostic>) {
    use unknown_classes::UNKNOWN_CLASS_CODE;
    use unknown_members::{SCALAR_MEMBER_ACCESS_CODE, UNKNOWN_MEMBER_CODE};
    use unresolved_member_access::UNRESOLVED_MEMBER_ACCESS_CODE;

    // Collect ranges of higher-priority diagnostics.
    let priority_ranges: Vec<Range> = diagnostics
        .iter()
        .filter_map(|d| {
            let code = match &d.code {
                Some(NumberOrString::String(s)) => s.as_str(),
                _ => return None,
            };
            if code == UNKNOWN_MEMBER_CODE
                || code == SCALAR_MEMBER_ACCESS_CODE
                || code == UNKNOWN_CLASS_CODE
            {
                Some(d.range)
            } else {
                None
            }
        })
        .collect();

    if priority_ranges.is_empty() {
        return;
    }

    diagnostics.retain(|d| {
        let code = match &d.code {
            Some(NumberOrString::String(s)) => s.as_str(),
            _ => return true,
        };
        if code != UNRESOLVED_MEMBER_ACCESS_CODE {
            return true;
        }
        // Drop this diagnostic if any priority diagnostic overlaps.
        !priority_ranges
            .iter()
            .any(|pr| ranges_overlap(pr, &d.range))
    });
}

/// Check whether two LSP ranges overlap (share at least one character).
fn ranges_overlap(a: &Range, b: &Range) -> bool {
    // Two ranges overlap when neither ends before the other starts.
    !(a.end.line < b.start.line
        || (a.end.line == b.start.line && a.end.character <= b.start.character)
        || b.end.line < a.start.line
        || (b.end.line == a.start.line && b.end.character <= a.start.character))
}

/// Build a diagnostic range from byte offsets, returning `None` if the
/// conversion fails (e.g. invalid offset or multi-byte boundary).
pub(crate) fn offset_range_to_lsp_range(
    content: &str,
    start_byte: usize,
    end_byte: usize,
) -> Option<Range> {
    let start_pos = byte_offset_to_position(content, start_byte)?;
    let end_pos = byte_offset_to_position(content, end_byte)?;
    Some(Range {
        start: start_pos,
        end: end_pos,
    })
}

/// Convert a byte offset to an LSP `Position` (0-based line and character).
fn byte_offset_to_position(content: &str, byte_offset: usize) -> Option<Position> {
    if byte_offset > content.len() {
        return None;
    }
    let before = &content[..byte_offset];
    let line = before.matches('\n').count() as u32;
    let last_newline = before.rfind('\n').map(|i| i + 1).unwrap_or(0);
    let character = before[last_newline..].encode_utf16().count() as u32;
    Some(Position { line, character })
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use unknown_classes::UNKNOWN_CLASS_CODE;
    use unknown_members::{SCALAR_MEMBER_ACCESS_CODE, UNKNOWN_MEMBER_CODE};
    use unresolved_member_access::UNRESOLVED_MEMBER_ACCESS_CODE;

    fn make_range(line: u32, start_char: u32, end_char: u32) -> Range {
        Range {
            start: Position {
                line,
                character: start_char,
            },
            end: Position {
                line,
                character: end_char,
            },
        }
    }

    fn make_diagnostic(code: &str, range: Range) -> Diagnostic {
        Diagnostic {
            range,
            severity: Some(DiagnosticSeverity::HINT),
            code: Some(NumberOrString::String(code.to_string())),
            code_description: None,
            source: Some("phpantom".to_string()),
            message: format!("test diagnostic ({})", code),
            related_information: None,
            tags: None,
            data: None,
        }
    }

    // ── ranges_overlap ──────────────────────────────────────────────────

    #[test]
    fn overlapping_ranges_on_same_line() {
        let a = make_range(5, 0, 10);
        let b = make_range(5, 5, 15);
        assert!(ranges_overlap(&a, &b));
        assert!(ranges_overlap(&b, &a));
    }

    #[test]
    fn non_overlapping_ranges_on_same_line() {
        let a = make_range(5, 0, 5);
        let b = make_range(5, 5, 10);
        assert!(!ranges_overlap(&a, &b));
        assert!(!ranges_overlap(&b, &a));
    }

    #[test]
    fn non_overlapping_ranges_on_different_lines() {
        let a = make_range(1, 0, 10);
        let b = make_range(3, 0, 10);
        assert!(!ranges_overlap(&a, &b));
    }

    #[test]
    fn identical_ranges_overlap() {
        let a = make_range(5, 3, 8);
        assert!(ranges_overlap(&a, &a));
    }

    #[test]
    fn contained_range_overlaps() {
        let outer = make_range(5, 0, 20);
        let inner = make_range(5, 5, 10);
        assert!(ranges_overlap(&outer, &inner));
        assert!(ranges_overlap(&inner, &outer));
    }

    // ── deduplicate_diagnostics ─────────────────────────────────────────

    #[test]
    fn suppresses_unresolved_member_when_unknown_class_overlaps() {
        let range = make_range(5, 4, 20);
        let mut diags = vec![
            make_diagnostic(UNKNOWN_CLASS_CODE, range),
            make_diagnostic(UNRESOLVED_MEMBER_ACCESS_CODE, range),
        ];
        deduplicate_diagnostics(&mut diags);
        assert_eq!(diags.len(), 1);
        assert_eq!(
            diags[0].code,
            Some(NumberOrString::String(UNKNOWN_CLASS_CODE.to_string())),
        );
    }

    #[test]
    fn suppresses_unresolved_member_when_unknown_member_overlaps() {
        let range = make_range(5, 4, 20);
        let mut diags = vec![
            make_diagnostic(UNKNOWN_MEMBER_CODE, range),
            make_diagnostic(UNRESOLVED_MEMBER_ACCESS_CODE, range),
        ];
        deduplicate_diagnostics(&mut diags);
        assert_eq!(diags.len(), 1);
        assert_eq!(
            diags[0].code,
            Some(NumberOrString::String(UNKNOWN_MEMBER_CODE.to_string())),
        );
    }

    #[test]
    fn suppresses_unresolved_member_when_scalar_member_access_overlaps() {
        let range = make_range(5, 4, 20);
        let mut diags = vec![
            make_diagnostic(SCALAR_MEMBER_ACCESS_CODE, range),
            make_diagnostic(UNRESOLVED_MEMBER_ACCESS_CODE, range),
        ];
        deduplicate_diagnostics(&mut diags);
        assert_eq!(diags.len(), 1);
        assert_eq!(
            diags[0].code,
            Some(NumberOrString::String(
                SCALAR_MEMBER_ACCESS_CODE.to_string()
            )),
        );
    }

    #[test]
    fn keeps_unresolved_member_when_no_priority_diagnostic() {
        let range = make_range(5, 4, 20);
        let mut diags = vec![make_diagnostic(UNRESOLVED_MEMBER_ACCESS_CODE, range)];
        deduplicate_diagnostics(&mut diags);
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn keeps_unresolved_member_on_different_range() {
        let range_a = make_range(5, 0, 10);
        let range_b = make_range(10, 0, 10);
        let mut diags = vec![
            make_diagnostic(UNKNOWN_CLASS_CODE, range_a),
            make_diagnostic(UNRESOLVED_MEMBER_ACCESS_CODE, range_b),
        ];
        deduplicate_diagnostics(&mut diags);
        assert_eq!(diags.len(), 2);
    }

    #[test]
    fn suppresses_multiple_unresolved_members_with_priority_overlap() {
        let range = make_range(5, 4, 20);
        let other_range = make_range(8, 0, 15);
        let mut diags = vec![
            make_diagnostic(UNKNOWN_CLASS_CODE, range),
            make_diagnostic(SCALAR_MEMBER_ACCESS_CODE, other_range),
            make_diagnostic(UNRESOLVED_MEMBER_ACCESS_CODE, range),
            make_diagnostic(UNRESOLVED_MEMBER_ACCESS_CODE, other_range),
        ];
        deduplicate_diagnostics(&mut diags);
        assert_eq!(diags.len(), 2);
        assert!(diags.iter().all(|d| d.code
            != Some(NumberOrString::String(
                UNRESOLVED_MEMBER_ACCESS_CODE.to_string()
            ))));
    }

    #[test]
    fn no_op_when_no_diagnostics() {
        let mut diags: Vec<Diagnostic> = Vec::new();
        deduplicate_diagnostics(&mut diags);
        assert!(diags.is_empty());
    }
}
