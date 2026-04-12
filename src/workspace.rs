//! Multi-workspace support for monorepo configurations.
//!
//! Maps file URIs to their owning subproject so that each subproject
//! can have its own PHP version and configuration.  In single-project
//! mode the [`WorkspaceMap`] degenerates to a trivial wrapper around
//! one config / one PHP version.

use std::path::{Path, PathBuf};

use crate::config;
use crate::types;

/// A single subproject within a monorepo (or the sole project in
/// single-project mode).
#[derive(Debug, Clone)]
pub struct SubProject {
    /// Absolute path of the subproject root directory (contains `composer.json`).
    pub root: PathBuf,
    /// `file://` URI prefix for this subproject, with trailing slash.
    /// Used for longest-prefix matching against file URIs.
    pub uri_prefix: String,
    /// Resolved configuration for this subproject (root config merged
    /// with any subproject-level overrides).
    pub config: config::Config,
    /// The PHP version for this subproject.
    pub php_version: types::PhpVersion,
    /// Vendor directory name (typically `"vendor"`).
    pub vendor_dir: String,
}

/// Maps file URIs to subprojects for per-project configuration and
/// PHP version resolution.
///
/// In single-project mode (one `composer.json` at workspace root),
/// every file maps to the same config and PHP version.  In monorepo
/// mode, files are matched to their containing subproject via
/// longest-prefix URI matching.
#[derive(Debug, Clone, Default)]
pub struct WorkspaceMap {
    /// Subprojects sorted by `uri_prefix` length descending, so that
    /// longest-prefix matching finds the most specific subproject first.
    subprojects: Vec<SubProject>,
    /// Fallback config for files outside any subproject.
    root_config: config::Config,
    /// Fallback PHP version for files outside any subproject.
    root_php_version: types::PhpVersion,
}

impl WorkspaceMap {
    /// Create a single-project workspace (backward compatibility).
    ///
    /// All files resolve to the same config and PHP version regardless
    /// of their path.
    pub fn single(config: config::Config, php_version: types::PhpVersion) -> Self {
        Self {
            subprojects: Vec::new(),
            root_config: config,
            root_php_version: php_version,
        }
    }

    /// Create a multi-project workspace for monorepo setups.
    ///
    /// `subprojects` will be sorted by URI prefix length descending
    /// so longest-prefix matching works correctly for nested paths.
    pub fn multi(
        mut subprojects: Vec<SubProject>,
        root_config: config::Config,
        root_php_version: types::PhpVersion,
    ) -> Self {
        // Sort by prefix length descending — longest prefix matches first.
        subprojects.sort_by(|a, b| b.uri_prefix.len().cmp(&a.uri_prefix.len()));
        Self {
            subprojects,
            root_config,
            root_php_version,
        }
    }

    /// Returns `true` if this is a multi-workspace (monorepo) setup.
    pub fn is_multi(&self) -> bool {
        !self.subprojects.is_empty()
    }

    /// Look up the subproject that owns a given file URI.
    ///
    /// Uses longest-prefix matching on the URI string.  Returns `None`
    /// for files outside any subproject (e.g. loose files at the
    /// workspace root).
    pub fn subproject_for_uri(&self, uri: &str) -> Option<&SubProject> {
        // Subprojects are sorted by prefix length descending, so the
        // first match is the longest (most specific) prefix.
        self.subprojects
            .iter()
            .find(|sp| uri.starts_with(&sp.uri_prefix))
    }

    /// Get the PHP version for a file URI.
    ///
    /// Returns the subproject's version if the file belongs to one,
    /// otherwise falls back to the root PHP version.
    pub fn php_version_for(&self, uri: &str) -> types::PhpVersion {
        self.subproject_for_uri(uri)
            .map(|sp| sp.php_version)
            .unwrap_or(self.root_php_version)
    }

    /// Get the configuration for a file URI.
    ///
    /// Returns the subproject's config if the file belongs to one,
    /// otherwise falls back to the root config.
    pub fn config_for(&self, uri: &str) -> &config::Config {
        self.subproject_for_uri(uri)
            .map(|sp| &sp.config)
            .unwrap_or(&self.root_config)
    }

    /// Get the subproject root for a file URI.
    ///
    /// Returns the subproject's root directory (for use as working
    /// directory when invoking external tools like PHPStan or PHPCS).
    /// Returns `None` for files outside any subproject.
    pub fn project_root_for(&self, uri: &str) -> Option<&Path> {
        self.subproject_for_uri(uri).map(|sp| sp.root.as_path())
    }

    /// Return the root (fallback) PHP version.
    pub fn root_php_version(&self) -> types::PhpVersion {
        self.root_php_version
    }

    /// Return a reference to the root (fallback) config.
    pub fn root_config(&self) -> &config::Config {
        &self.root_config
    }

    /// Return all subprojects.
    pub fn subprojects(&self) -> &[SubProject] {
        &self.subprojects
    }
}

// Default is derived via the field defaults (Vec::new, Config::default, PhpVersion::default).

#[cfg(test)]
mod tests {
    use super::*;

    fn make_subproject(root: &str, php_major: u8, php_minor: u8) -> SubProject {
        let root = PathBuf::from(root);
        let uri_prefix = format!(
            "file://{}{}",
            root.display(),
            if root.to_string_lossy().ends_with('/') {
                ""
            } else {
                "/"
            }
        );
        SubProject {
            root,
            uri_prefix,
            config: config::Config::default(),
            php_version: types::PhpVersion {
                major: php_major,
                minor: php_minor,
            },
            vendor_dir: "vendor".to_string(),
        }
    }

    #[test]
    fn single_workspace_returns_root_for_any_uri() {
        let version = types::PhpVersion { major: 8, minor: 3 };
        let ws = WorkspaceMap::single(config::Config::default(), version);

        assert_eq!(ws.php_version_for("file:///any/path/foo.php"), version);
        assert!(ws.subproject_for_uri("file:///any/path/foo.php").is_none());
        assert!(ws.project_root_for("file:///any/path/foo.php").is_none());
        assert!(!ws.is_multi());
    }

    #[test]
    fn multi_workspace_matches_subproject_by_prefix() {
        let sp_a = make_subproject("/workspace/packages/app-a", 8, 4);
        let sp_b = make_subproject("/workspace/packages/legacy", 7, 4);
        let root_version = types::PhpVersion { major: 8, minor: 5 };

        let ws = WorkspaceMap::multi(vec![sp_a, sp_b], config::Config::default(), root_version);

        assert!(ws.is_multi());

        // File in app-a
        let v = ws.php_version_for("file:///workspace/packages/app-a/src/Foo.php");
        assert_eq!(v, types::PhpVersion { major: 8, minor: 4 });

        // File in legacy
        let v = ws.php_version_for("file:///workspace/packages/legacy/src/Bar.php");
        assert_eq!(v, types::PhpVersion { major: 7, minor: 4 });

        // File outside any subproject — falls back to root
        let v = ws.php_version_for("file:///workspace/scripts/deploy.php");
        assert_eq!(v, root_version);
    }

    #[test]
    fn longest_prefix_wins_for_nested_paths() {
        let sp_outer = make_subproject("/workspace/packages", 8, 3);
        let sp_inner = make_subproject("/workspace/packages/nested", 8, 4);
        let root_version = types::PhpVersion { major: 8, minor: 5 };

        let ws = WorkspaceMap::multi(
            vec![sp_outer, sp_inner],
            config::Config::default(),
            root_version,
        );

        // File in nested — should match the inner (longer prefix), not outer
        let v = ws.php_version_for("file:///workspace/packages/nested/src/Foo.php");
        assert_eq!(v, types::PhpVersion { major: 8, minor: 4 });

        // File in packages but not in nested — should match outer
        let v = ws.php_version_for("file:///workspace/packages/other/src/Bar.php");
        assert_eq!(v, types::PhpVersion { major: 8, minor: 3 });
    }

    #[test]
    fn project_root_for_returns_subproject_root() {
        let sp = make_subproject("/workspace/packages/app", 8, 4);
        let ws = WorkspaceMap::multi(
            vec![sp],
            config::Config::default(),
            types::PhpVersion::default(),
        );

        let root = ws.project_root_for("file:///workspace/packages/app/src/Main.php");
        assert_eq!(root, Some(Path::new("/workspace/packages/app")));

        let root = ws.project_root_for("file:///workspace/other/file.php");
        assert_eq!(root, None);
    }
}
