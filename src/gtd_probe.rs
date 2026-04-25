//! Diagnostic probe for Go-to-Definition hot paths.
//!
//! Counts invocations of suspect functions during a single GTD request
//! and emits a `tracing::warn!` when any path exceeds a threshold or
//! the request exceeds a wall-clock budget. Enabled by the standard
//! `tracing` filter — e.g. `RUST_LOG=phpantom_lsp::gtd_probe=info`.
//!
//! Used to triage hangs reported by users on dense files where GTD on
//! `$var->method()` never returns. The probe itself has no behaviour:
//! it only observes. Targeted fixes are applied based on which
//! counter dominates in the warning output.

use std::cell::Cell;
use std::time::Instant;

thread_local! {
    static GTD_START: Cell<Option<Instant>> = const { Cell::new(None) };
    static FIND_DECLARING_CLASS: Cell<u64> = const { Cell::new(0) };
    static RESOLVE_TARGET_CLASSES: Cell<u64> = const { Cell::new(0) };
    static BUILD_METHOD_TEMPLATE_SUBS: Cell<u64> = const { Cell::new(0) };
}

/// Wall-clock threshold above which a GTD request is considered slow
/// and the probe emits a warning unconditionally.
const SLOW_GTD_MILLIS: u128 = 1000;

/// Per-counter thresholds. Set so a "normal" GTD on a typical file
/// stays well below all of them; exceeding any one is a strong signal
/// for the hot path.
const HIGH_FIND_DECLARING_CLASS: u64 = 200;
const HIGH_RESOLVE_TARGET_CLASSES: u64 = 1000;
const HIGH_BUILD_METHOD_TEMPLATE_SUBS: u64 = 200;

/// RAII guard that scopes a probe window. Constructing one resets all
/// counters; dropping it emits the report.
///
/// Place at the top of the GTD entry function. Recursion into the
/// same entry inside a single request is unusual but tolerated — only
/// the outermost guard's drop reports.
pub(crate) struct GtdProbeGuard {
    label: &'static str,
    detail: String,
}

impl GtdProbeGuard {
    pub(crate) fn enter(label: &'static str, detail: impl Into<String>) -> Self {
        GTD_START.with(|s| s.set(Some(Instant::now())));
        FIND_DECLARING_CLASS.with(|c| c.set(0));
        RESOLVE_TARGET_CLASSES.with(|c| c.set(0));
        BUILD_METHOD_TEMPLATE_SUBS.with(|c| c.set(0));
        Self {
            label,
            detail: detail.into(),
        }
    }
}

impl Drop for GtdProbeGuard {
    fn drop(&mut self) {
        let elapsed = GTD_START
            .with(|s| s.take().map(|t| t.elapsed()))
            .unwrap_or_default();
        let fdc = FIND_DECLARING_CLASS.with(Cell::get);
        let rtc = RESOLVE_TARGET_CLASSES.with(Cell::get);
        let bmts = BUILD_METHOD_TEMPLATE_SUBS.with(Cell::get);

        let slow = elapsed.as_millis() >= SLOW_GTD_MILLIS;
        let high = fdc >= HIGH_FIND_DECLARING_CLASS
            || rtc >= HIGH_RESOLVE_TARGET_CLASSES
            || bmts >= HIGH_BUILD_METHOD_TEMPLATE_SUBS;

        if slow || high {
            tracing::warn!(
                target: "phpantom_lsp::gtd_probe",
                label = self.label,
                detail = %self.detail,
                elapsed_ms = elapsed.as_millis() as u64,
                find_declaring_class = fdc,
                resolve_target_classes = rtc,
                build_method_template_subs = bmts,
                "slow GTD or counter over threshold",
            );
        } else {
            tracing::debug!(
                target: "phpantom_lsp::gtd_probe",
                label = self.label,
                detail = %self.detail,
                elapsed_ms = elapsed.as_millis() as u64,
                find_declaring_class = fdc,
                resolve_target_classes = rtc,
                build_method_template_subs = bmts,
                "GTD complete",
            );
        }
    }
}

#[inline]
pub(crate) fn inc_find_declaring_class() {
    FIND_DECLARING_CLASS.with(|c| c.set(c.get().saturating_add(1)));
}

#[inline]
pub(crate) fn inc_resolve_target_classes() {
    RESOLVE_TARGET_CLASSES.with(|c| c.set(c.get().saturating_add(1)));
}

#[inline]
pub(crate) fn inc_build_method_template_subs() {
    BUILD_METHOD_TEMPLATE_SUBS.with(|c| c.set(c.get().saturating_add(1)));
}
