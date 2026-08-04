//! Composer-environment-aware class resolution.
//!
//! A workspace can contain multiple independent Composer projects. Each one
//! may install a different physical definition for the same class FQN. The
//! global class index remains useful as a workspace-wide fallback for linked
//! source packages, but vendor definitions must be selected from the project
//! that owns the file making the request.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use tower_lsp::lsp_types::Url;

use crate::Backend;
use crate::ci_map::CiMap;

// ─── Ambient analysis context ────────────────────────────────────────

thread_local! {
    /// The file currently being analyzed on this thread, if any.
    ///
    /// Set by [`with_analysis_context`] at per-file analysis entry points
    /// (diagnostics, hover, completion). Read by the class loader so type
    /// resolution can prefer the Composer environment that owns the
    /// analyzed file — mirroring which autoloader would be active at
    /// runtime. A thread-local fits the execution model: every analysis
    /// pass runs synchronously on one blocking thread, like the scoped
    /// per-analysis caches in `collect_slow_diagnostics`.
    static ANALYSIS_CONTEXT: RefCell<Option<AnalysisContext>> = const { RefCell::new(None) };
}

struct AnalysisContext {
    id: u64,
    uri: String,
    /// Per-analysis memo of contextual FQN lookups. Class resolution is
    /// hot (thousands of lookups per file), so repeated names skip the
    /// project-index walk after the first hit.
    resolved: HashMap<String, Option<String>>,
}

/// RAII guard that restores the previous ambient context on drop, so
/// nested activations (e.g. a collector re-entering analysis) unwind
/// correctly.
pub(crate) struct AnalysisContextGuard {
    previous: Option<AnalysisContext>,
}

impl Drop for AnalysisContextGuard {
    fn drop(&mut self) {
        ANALYSIS_CONTEXT.with(|slot| {
            *slot.borrow_mut() = self.previous.take();
        });
    }
}

/// Mark `uri` as the file under analysis for the current thread until
/// the returned guard is dropped.
pub(crate) fn with_analysis_context(uri: &str) -> AnalysisContextGuard {
    static NEXT_CONTEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let previous = ANALYSIS_CONTEXT.with(|slot| {
        slot.borrow_mut().replace(AnalysisContext {
            id: NEXT_CONTEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            uri: uri.to_string(),
            resolved: HashMap::new(),
        })
    });
    AnalysisContextGuard { previous }
}

/// Identity of the current ambient analysis pass, or zero outside one.
/// Class-loader memo entries include this value so same-FQN answers cannot
/// cross Composer-environment boundaries on a reused worker thread.
pub(crate) fn analysis_context_id() -> u64 {
    ANALYSIS_CONTEXT.with(|slot| slot.borrow().as_ref().map_or(0, |context| context.id))
}

#[derive(Debug, Clone)]
pub(crate) struct ComposerProjectIndex {
    root: PathBuf,
    vendor_path: PathBuf,
    classes: CiMap<String>,
}

impl ComposerProjectIndex {
    fn contains_path(&self, path: &Path) -> bool {
        path.starts_with(&self.root) || path.starts_with(&self.vendor_path)
    }
}

fn normalise_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn uri_file_path(uri: &str) -> Option<PathBuf> {
    let path = Url::parse(uri).ok()?.to_file_path().ok()?;
    Some(normalise_path(&path))
}

fn same_uri(left: &str, right: &str) -> bool {
    left == right
        || uri_file_path(left)
            .zip(uri_file_path(right))
            .is_some_and(|(left, right)| left == right)
}

fn is_vendor_path(projects: &[ComposerProjectIndex], path: &Path) -> bool {
    projects
        .iter()
        .any(|project| path.starts_with(&project.vendor_path))
}

fn is_vendor_uri(projects: &[ComposerProjectIndex], uri: &str) -> bool {
    uri_file_path(uri).is_some_and(|path| is_vendor_path(projects, &path))
}

impl Backend {
    /// Register a Composer project before any of its autoload helpers are
    /// parsed. Registration is idempotent so re-initialization and rescans can
    /// refresh the same entry without creating duplicate environments.
    pub(crate) fn register_composer_project(&self, root: &Path, vendor_path: &Path) {
        let root = normalise_path(root);
        let vendor_path = normalise_path(vendor_path);
        let mut projects = self.workspace.composer_project_indexes.write();
        if let Some(project) = projects.iter_mut().find(|project| project.root == root) {
            project.vendor_path = vendor_path;
            return;
        }
        projects.push(ComposerProjectIndex {
            root,
            vendor_path,
            classes: CiMap::new(),
        });
        projects.sort_by(|left, right| left.root.cmp(&right.root));
    }

    /// Add discovery-level class entries to a project's own Composer
    /// universe. Existing entries retain Composer's original precedence.
    pub(crate) fn index_composer_project_classes<I>(&self, root: &Path, entries: I)
    where
        I: IntoIterator<Item = (String, PathBuf)>,
    {
        let root = normalise_path(root);
        let mut projects = self.workspace.composer_project_indexes.write();
        let Some(project) = projects.iter_mut().find(|project| project.root == root) else {
            return;
        };
        for (fqn, path) in entries {
            let uri = crate::util::path_to_uri(&path);
            project.classes.or_insert_with(fqn, || uri);
        }
    }

    /// Associate a discovered class with whichever registered project owns
    /// its path. Used by whole-workspace scans after all subprojects have been
    /// registered.
    pub(crate) fn index_class_in_owning_composer_project(&self, fqn: String, path: &Path) {
        let path = normalise_path(path);
        let uri = crate::util::path_to_uri(&path);
        let mut projects = self.workspace.composer_project_indexes.write();
        let Some(project) = projects
            .iter_mut()
            .filter(|project| project.contains_path(&path))
            .max_by_key(|project| project.root.components().count())
        else {
            return;
        };
        let new_is_vendor = path.starts_with(&project.vendor_path);
        let replace = match project.classes.get(&fqn) {
            None => true,
            Some(existing) if same_uri(existing, &uri) => true,
            Some(existing) => {
                uri_file_path(existing).is_some_and(|path| path.starts_with(&project.vendor_path))
                    && !new_is_vendor
            }
        };
        if replace {
            project.classes.insert(fqn, uri);
        }
    }

    /// Keep a project's parsed-file contribution current without allowing an
    /// opened vendor file to replace the same FQN in another environment.
    pub(crate) fn update_composer_project_file_classes(
        &self,
        uri: &str,
        old_fqns: &[String],
        new_fqns: impl IntoIterator<Item = String>,
    ) {
        let Some(path) = uri_file_path(uri) else {
            return;
        };
        let mut projects = self.workspace.composer_project_indexes.write();
        let Some(project) = projects
            .iter_mut()
            .filter(|project| project.contains_path(&path))
            .max_by_key(|project| project.root.components().count())
        else {
            return;
        };

        for fqn in old_fqns {
            let points_to_file = project
                .classes
                .get(fqn)
                .is_some_and(|existing| same_uri(existing, uri));
            if points_to_file {
                project.classes.remove(fqn);
            }
        }

        let new_is_vendor = path.starts_with(&project.vendor_path);
        for fqn in new_fqns {
            let replace = match project.classes.get(&fqn) {
                None => true,
                Some(existing) if same_uri(existing, uri) => true,
                Some(existing) => {
                    uri_file_path(existing)
                        .is_some_and(|path| path.starts_with(&project.vendor_path))
                        && !new_is_vendor
                }
            };
            if replace {
                project.classes.insert(fqn, uri.to_string());
            }
        }
    }

    /// Remove every classmap entry contributed by one physical file. This is
    /// used before watched-file reindexing so deleted and renamed classes do
    /// not remain in their project's environment.
    pub(crate) fn remove_composer_project_file_classes(&self, uri: &str) {
        let Some(path) = uri_file_path(uri) else {
            return;
        };
        let mut projects = self.workspace.composer_project_indexes.write();
        let Some(project) = projects
            .iter_mut()
            .filter(|project| project.contains_path(&path))
            .max_by_key(|project| project.root.components().count())
        else {
            return;
        };
        project
            .classes
            .retain(|_, existing| !same_uri(existing, uri));
    }

    /// Replace only the vendor portion of a project's index after Composer
    /// metadata changes, preserving project-source entries.
    pub(crate) fn replace_composer_project_vendor_classes<I>(
        &self,
        root: &Path,
        vendor_path: &Path,
        entries: I,
    ) where
        I: IntoIterator<Item = (String, PathBuf)>,
    {
        let root = normalise_path(root);
        let vendor_path = normalise_path(vendor_path);
        let mut projects = self.workspace.composer_project_indexes.write();
        let project =
            if let Some(project) = projects.iter_mut().find(|project| project.root == root) {
                project
            } else {
                projects.push(ComposerProjectIndex {
                    root: root.clone(),
                    vendor_path: vendor_path.clone(),
                    classes: CiMap::new(),
                });
                projects
                    .iter_mut()
                    .find(|project| project.root == root)
                    .expect("new Composer project index should exist")
            };
        let previous_vendor_path = project.vendor_path.clone();
        project.classes.retain(|_, uri| {
            uri_file_path(uri).is_none_or(|path| {
                !path.starts_with(&previous_vendor_path) && !path.starts_with(&vendor_path)
            })
        });
        project.vendor_path = vendor_path;
        for (fqn, path) in entries {
            project
                .classes
                .or_insert_with(fqn, || crate::util::path_to_uri(&path));
        }
    }

    pub(crate) fn has_composer_project_context(&self, uri: &str) -> bool {
        let Some(path) = uri_file_path(uri) else {
            return false;
        };
        self.workspace
            .composer_project_indexes
            .read()
            .iter()
            .any(|project| project.contains_path(&path))
    }

    /// Resolve an FQN in the Composer environment that owns `context_uri`.
    ///
    /// Project-local classmap entries (including that project's vendor tree)
    /// win. If the project does not provide the class, a non-vendor workspace
    /// definition remains available as the linked-package fallback used by
    /// source monorepos such as Laminas. A vendor definition belonging only to
    /// another project is never allowed to leak across the boundary.
    pub(crate) fn class_uri_for_context(&self, fqn: &str, context_uri: &str) -> Option<String> {
        let context_path = uri_file_path(context_uri);
        let projects = self.workspace.composer_project_indexes.read();
        let context_project = context_path.as_deref().and_then(|path| {
            projects
                .iter()
                .filter(|project| project.contains_path(path))
                .max_by_key(|project| project.root.components().count())
        });

        if let Some(project) = context_project
            && let Some(uri) = project.classes.get(fqn)
        {
            return Some(uri.clone());
        }

        let global = self.symbols.fqn_uri_index.read().get(fqn).cloned();
        if let Some(uri) = global.as_deref()
            && !is_vendor_uri(&projects, uri)
        {
            return global;
        }

        // Preserve linked source-package behavior even when the global index
        // happened to retain a vendor entry discovered earlier.
        let mut source_candidates: Vec<String> = projects
            .iter()
            .filter_map(|project| project.classes.get(fqn).cloned())
            .filter(|uri| !is_vendor_uri(&projects, uri))
            .collect();
        source_candidates.sort();
        source_candidates.dedup_by(|left, right| same_uri(left, right));
        if let Some(uri) = source_candidates.into_iter().next() {
            return Some(uri);
        }

        // Files outside every Composer project retain the legacy global
        // workspace behavior. Inside a project, returning another project's
        // vendor URI would be cross-environment contamination, so fail closed.
        if context_project.is_none() {
            global.or_else(|| {
                let mut candidates: Vec<String> = projects
                    .iter()
                    .filter_map(|project| project.classes.get(fqn).cloned())
                    .collect();
                candidates.sort();
                candidates.dedup_by(|left, right| same_uri(left, right));
                candidates.into_iter().next()
            })
        } else {
            None
        }
    }

    pub(crate) fn class_uris_match(&self, left: &str, right: &str) -> bool {
        same_uri(left, right)
    }

    /// Resolve `fqn` in the Composer environment of the file currently
    /// under analysis on this thread (see [`with_analysis_context`]).
    ///
    /// Returns `None` when no analysis context is active or when the
    /// context cannot supply the class — callers should then fall back
    /// to the global lookup phases.
    pub(crate) fn contextual_class_uri(&self, fqn: &str) -> Option<String> {
        let (context_uri, memoised) = ANALYSIS_CONTEXT.with(|slot| {
            let slot = slot.borrow();
            let context = slot.as_ref()?;
            Some((context.uri.clone(), context.resolved.get(fqn).cloned()))
        })?;
        if let Some(memoised) = memoised {
            return memoised;
        }
        let resolved = self.class_uri_for_context(fqn, &context_uri);
        ANALYSIS_CONTEXT.with(|slot| {
            if let Some(context) = slot.borrow_mut().as_mut() {
                context.resolved.insert(fqn.to_string(), resolved.clone());
            }
        });
        resolved
    }
}
