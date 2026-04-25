//! Find References (`textDocument/references`).
//!
//! When the user invokes "Find All References" on a symbol, the LSP
//! collects every occurrence of that symbol across the project.
//!
//! **Same-file references** are answered from the precomputed
//! [`SymbolMap`] — we iterate all spans and collect those that match
//! the symbol under the cursor.
//!
//! **Cross-file references** iterate every `SymbolMap` stored in
//! `self.symbol_maps` (one per opened / parsed file).  For files that
//! are in the workspace but have not been opened yet, we lazily parse
//! them on demand (via the classmap, PSR-4, and workspace scan).
//!
//! **Variable references** (including `$this`) are strictly scoped to
//! the enclosing function / method / closure body within the current
//! file.
//!
//! **Member references** (methods, properties, constants) are filtered
//! by the class hierarchy of the target member.  When the user triggers
//! "Find References" on `MyClass::save()`, only accesses where the
//! subject resolves to a class in the same inheritance tree are returned.
//! Accesses on unrelated classes that happen to have a member with the
//! same name are excluded.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::UNIX_EPOCH;

use etcetera::BaseStrategy;
use tower_lsp::lsp_types::{Location, Position, Range, Url};

use crate::Backend;
use crate::completion::resolver::Loaders;
use crate::symbol_map::{SelfStaticParentKind, SymbolKind, SymbolMap};
use crate::types::{ClassInfo, MAX_INHERITANCE_DEPTH, ResolvedType};
use crate::util::{
    build_fqn, collect_php_files_gitignore, find_class_at_offset, offset_to_position,
    position_to_offset, resolve_to_fqn, strip_fqn_prefix,
};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum ReferenceIndexKey {
    Class(String),
    Function(String),
    Constant(String),
    Member { name: String, is_static: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReferenceIndexEntry {
    pub uri: String,
    pub start: u32,
    pub end: u32,
    pub range: Range,
    pub is_declaration: bool,
    /// Pre-resolved subject FQN(s) for `MemberAccess` /
    /// `MemberDeclaration` spans whose owning class is statically
    /// known at index-build time (`$this` / `self` / `static` /
    /// `parent` / bare class name, or the enclosing class for a
    /// declaration). `None` means "fall back to runtime resolution"
    /// — used for variable subjects whose type may depend on
    /// cross-file state that has changed since this file was last
    /// parsed.
    pub subject_fqns: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ReferenceIndex {
    by_key: HashMap<ReferenceIndexKey, Vec<ReferenceIndexEntry>>,
    by_uri: HashMap<String, Vec<(ReferenceIndexKey, ReferenceIndexEntry)>>,
}

impl ReferenceIndex {
    pub(crate) fn remove_uri(&mut self, uri: &str) {
        let Some(old_entries) = self.by_uri.remove(uri) else {
            return;
        };

        for (key, old_entry) in old_entries {
            if let Some(entries) = self.by_key.get_mut(&key) {
                entries.retain(|entry| entry != &old_entry);
                if entries.is_empty() {
                    self.by_key.remove(&key);
                }
            }
        }
    }

    fn insert(&mut self, key: ReferenceIndexKey, entry: ReferenceIndexEntry) {
        self.by_key
            .entry(key.clone())
            .or_default()
            .push(entry.clone());
        self.by_uri
            .entry(entry.uri.clone())
            .or_default()
            .push((key, entry));
    }

    fn entries(&self, key: &ReferenceIndexKey) -> Vec<ReferenceIndexEntry> {
        self.by_key.get(key).cloned().unwrap_or_default()
    }
}

impl Backend {
    /// Entry point for `textDocument/references`.
    ///
    /// Returns all locations where the symbol under the cursor is
    /// referenced.  When `include_declaration` is true the declaration
    /// site itself is included in the results.
    pub fn find_references(
        &self,
        uri: &str,
        content: &str,
        position: Position,
        include_declaration: bool,
    ) -> Option<Vec<Location>> {
        // Consult the precomputed symbol map for the current file
        // (retries one byte earlier for end-of-token edge cases).
        let symbol = self.lookup_symbol_at_position(uri, content, position);

        // When the cursor is on a symbol span, dispatch by kind.
        if let Some(ref sym) = symbol {
            let locations = self.dispatch_symbol_references(
                &sym.kind,
                uri,
                content,
                sym.start,
                include_declaration,
            );
            return if locations.is_empty() {
                None
            } else {
                Some(locations)
            };
        }

        None
    }

    /// Dispatch a symbol-map hit to the appropriate reference finder.
    fn dispatch_symbol_references(
        &self,
        kind: &SymbolKind,
        uri: &str,
        content: &str,
        span_start: u32,
        include_declaration: bool,
    ) -> Vec<Location> {
        match kind {
            SymbolKind::Variable { name } => {
                // Property declarations use Variable spans (so GTD can
                // jump to the type hint), but Find References should
                // search for member accesses, not local variable uses.
                if let Some(crate::symbol_map::VarDefKind::Property) =
                    self.lookup_var_def_kind_at(uri, name, span_start)
                {
                    // Properties are never static in the Variable span
                    // context ($this->prop).  Static properties use
                    // MemberAccess spans at their usage sites with
                    // is_static=true, but the declaration-site Variable
                    // span doesn't encode static-ness.  Check the
                    // ast_map to determine the correct flag.
                    let is_static = self
                        .get_classes_for_uri(uri)
                        .iter()
                        .flat_map(|classes| classes.iter())
                        .flat_map(|c| c.properties.iter())
                        .any(|p| {
                            let p_name = p.name.strip_prefix('$').unwrap_or(&p.name);
                            p_name == name && p.is_static
                        });

                    // Resolve the enclosing class to scope the search.
                    let hierarchy = self.resolve_member_declaration_hierarchy(uri, span_start);
                    return self.find_member_references(
                        name,
                        is_static,
                        include_declaration,
                        hierarchy.as_ref(),
                    );
                }
                self.find_variable_references(uri, content, name, span_start, include_declaration)
            }
            SymbolKind::ClassReference { name, is_fqn, .. } => {
                let ctx = self.file_context(uri);
                let fqn = if *is_fqn {
                    name.clone()
                } else {
                    ctx.resolve_name_at(name, span_start)
                };
                self.find_class_references(&fqn, include_declaration)
            }
            SymbolKind::ClassDeclaration { name } => {
                let ctx = self.file_context(uri);
                let fqn = build_fqn(name, ctx.namespace.as_deref());
                self.find_class_references(&fqn, include_declaration)
            }
            SymbolKind::MemberAccess {
                subject_text,
                member_name,
                is_static,
                ..
            } => {
                // Resolve the subject to determine the class hierarchy
                // so we only return references on related classes.
                let hierarchy =
                    self.resolve_member_access_hierarchy(uri, subject_text, *is_static, span_start);
                self.find_member_references(
                    member_name,
                    *is_static,
                    include_declaration,
                    hierarchy.as_ref(),
                )
            }
            SymbolKind::FunctionCall { name, .. } => {
                let ctx = self.file_context(uri);
                let fqn = ctx.resolve_name_at(name, span_start);
                self.find_function_references(&fqn, name, include_declaration)
            }
            SymbolKind::ConstantReference { name } => {
                self.find_constant_references(name, include_declaration)
            }
            SymbolKind::MemberDeclaration { name, is_static } => {
                // Resolve the enclosing class to scope the search.
                let hierarchy = self.resolve_member_declaration_hierarchy(uri, span_start);
                self.find_member_references(
                    name,
                    *is_static,
                    include_declaration,
                    hierarchy.as_ref(),
                )
            }
            SymbolKind::SelfStaticParent(ssp_kind) => {
                // `$this` is a file-local variable, not a cross-file class search.
                if *ssp_kind == SelfStaticParentKind::This {
                    return self.find_this_references(
                        uri,
                        content,
                        span_start,
                        include_declaration,
                    );
                }

                // For real self/static/parent keywords, resolve to the class FQN.
                let ctx = self.file_context(uri);
                let current_class = crate::util::find_class_at_offset(&ctx.classes, span_start);
                let fqn = match ssp_kind {
                    SelfStaticParentKind::Parent => {
                        current_class.and_then(|cc| cc.parent_class.map(|a| a.to_string()))
                    }
                    _ => current_class.map(|cc| build_fqn(&cc.name, ctx.namespace.as_deref())),
                };
                if let Some(fqn) = fqn {
                    self.find_class_references(&fqn, include_declaration)
                } else {
                    Vec::new()
                }
            }
            SymbolKind::NamespaceDeclaration { .. } => Vec::new(),
        }
    }

    /// Find all references to a variable within its enclosing scope.
    ///
    /// Variables are file-local and scope-local — a `$user` in method A
    /// must not match `$user` in method B.
    fn find_variable_references(
        &self,
        uri: &str,
        content: &str,
        var_name: &str,
        cursor_offset: u32,
        include_declaration: bool,
    ) -> Vec<Location> {
        let mut locations = Vec::new();

        let maps = self.symbol_maps.read();
        let symbol_map = match maps.get(uri) {
            Some(m) => m,
            None => return locations,
        };

        // Determine the effective scope for this variable.
        //
        // `find_variable_scope` handles the tricky cases where the
        // cursor is on a parameter (physically before the `{`) or on
        // a docblock `@param $var` mention, returning the body scope
        // those tokens logically belong to.
        let scope_start = symbol_map.find_variable_scope(var_name, cursor_offset);

        let parsed_uri = match Url::parse(uri) {
            Ok(u) => u,
            Err(_) => return locations,
        };

        for span in &symbol_map.spans {
            if let SymbolKind::Variable { name } = &span.kind {
                if name != var_name {
                    continue;
                }
                // Check that this variable is in the same scope.
                // `find_variable_scope` correctly handles parameter
                // spans and docblock `@param` mentions that sit before
                // the body `{`.
                let span_scope = symbol_map.find_variable_scope(name, span.start);
                if span_scope != scope_start {
                    continue;
                }
                // Optionally skip declaration sites.
                if !include_declaration && symbol_map.var_def_kind_at(name, span.start).is_some() {
                    continue;
                }
                let start = offset_to_position(content, span.start as usize);
                let end = offset_to_position(content, span.end as usize);
                locations.push(Location {
                    uri: parsed_uri.clone(),
                    range: Range { start, end },
                });
            }
        }

        // Also include var_def sites if include_declaration is set,
        // since some definition tokens (parameters, foreach bindings)
        // may not have a corresponding Variable span in the spans vec
        // with the exact same offset.
        if include_declaration {
            let mut seen_offsets: HashSet<u32> = locations
                .iter()
                .map(|loc| position_to_offset(content, loc.range.start))
                .collect();

            for def in &symbol_map.var_defs {
                if def.name == var_name
                    && def.scope_start == scope_start
                    && seen_offsets.insert(def.offset)
                {
                    let start = offset_to_position(content, def.offset as usize);
                    // The token is `$` + name.
                    let end_offset = def.offset as usize + 1 + def.name.len();
                    let end = offset_to_position(content, end_offset);
                    locations.push(Location {
                        uri: parsed_uri.clone(),
                        range: Range { start, end },
                    });
                }
            }
        }

        // Sort by position for stable output.
        locations.sort_by(|a, b| {
            a.range
                .start
                .line
                .cmp(&b.range.start.line)
                .then(a.range.start.character.cmp(&b.range.start.character))
        });

        locations
    }

    /// Find all references to `$this` within the enclosing class body.
    ///
    /// `$this` is scoped to the enclosing class — it must not match
    /// `$this` in a different class or top-level function.  Unlike
    /// regular variables, `$this` is **not** scoped to the enclosing
    /// method: `$this` in method A and `$this` in method B inside the
    /// same class both refer to the same object, so they should all
    /// appear in the results.
    fn find_this_references(
        &self,
        uri: &str,
        content: &str,
        cursor_offset: u32,
        include_declaration: bool,
    ) -> Vec<Location> {
        let _ = include_declaration; // $this has no "declaration site"
        let mut locations = Vec::new();

        let maps = self.symbol_maps.read();
        let symbol_map = match maps.get(uri) {
            Some(m) => m,
            None => return locations,
        };

        // Determine the class body the cursor is in.
        let ctx_classes: Vec<Arc<ClassInfo>> =
            self.ast_map.read().get(uri).cloned().unwrap_or_default();
        let current_class = crate::util::find_class_at_offset(&ctx_classes, cursor_offset);
        let (class_start, class_end) = match current_class {
            Some(cc) => (cc.start_offset, cc.end_offset),
            None => return locations,
        };

        let parsed_uri = match Url::parse(uri) {
            Ok(u) => u,
            Err(_) => return locations,
        };

        for span in &symbol_map.spans {
            // Only consider spans within the same class body.
            if span.start < class_start || span.start > class_end {
                continue;
            }

            let is_this = matches!(
                &span.kind,
                SymbolKind::SelfStaticParent(SelfStaticParentKind::This)
            );

            if is_this {
                let start = offset_to_position(content, span.start as usize);
                let end = offset_to_position(content, span.end as usize);
                locations.push(Location {
                    uri: parsed_uri.clone(),
                    range: Range { start, end },
                });
            }
        }

        locations.sort_by(|a, b| {
            a.range
                .start
                .line
                .cmp(&b.range.start.line)
                .then(a.range.start.character.cmp(&b.range.start.character))
        });

        locations
    }

    /// Snapshot all symbol maps for user (non-vendor, non-stub) files.
    ///
    /// Ensures the workspace is indexed first, then returns a cloned
    /// snapshot of every symbol map whose URI does not fall under the
    /// vendor directory or the internal stub scheme.  All four cross-file
    /// reference scanners use this to restrict results to user code.
    fn user_file_symbol_maps(&self) -> Vec<(String, Arc<SymbolMap>)> {
        self.ensure_workspace_indexed();

        let vendor_prefixes = self.vendor_uri_prefixes.lock().clone();

        let maps = self.symbol_maps.read();
        maps.iter()
            .filter(|(uri, _)| {
                !uri.starts_with("phpantom-stub://")
                    && !uri.starts_with("phpantom-stub-fn://")
                    && !vendor_prefixes.iter().any(|p| uri.starts_with(p.as_str()))
            })
            .map(|(uri, map)| (uri.clone(), Arc::clone(map)))
            .collect()
    }

    /// Rebuild the inverted reference-index entries for one parsed file.
    ///
    /// Called after `update_ast` has refreshed `symbol_maps`,
    /// `resolved_names`, `use_map`, and `namespace_map` for the URI.
    pub(crate) fn reindex_references_for_uri(&self, uri: &str, content: &str) {
        let mut index = self.reference_index.write();
        index.remove_uri(uri);

        if !self.should_index_references_for_uri(uri) {
            return;
        }

        let symbol_map = match self.symbol_maps.read().get(uri).cloned() {
            Some(map) => map,
            None => return,
        };
        let resolved_names = self.resolved_names.read().get(uri).cloned();
        let namespace = self.namespace_map.read().get(uri).cloned().flatten();
        let use_map = self.use_map.read().get(uri).cloned().unwrap_or_default();
        let classes: Vec<Arc<ClassInfo>> =
            self.ast_map.read().get(uri).cloned().unwrap_or_default();
        let ctx = crate::types::FileContext {
            classes,
            use_map: use_map.clone(),
            namespace: namespace.clone(),
            resolved_names: resolved_names.clone(),
        };
        let line_starts = line_starts(content);

        for span in &symbol_map.spans {
            let start_position = offset_to_position_with_lines(content, &line_starts, span.start);
            let end_position = offset_to_position_with_lines(content, &line_starts, span.end);
            let entry = ReferenceIndexEntry {
                uri: uri.to_string(),
                start: span.start,
                end: span.end,
                range: Range {
                    start: start_position,
                    end: end_position,
                },
                is_declaration: matches!(
                    span.kind,
                    SymbolKind::ClassDeclaration { .. }
                        | SymbolKind::FunctionCall {
                            is_definition: true,
                            ..
                        }
                        | SymbolKind::MemberDeclaration { .. }
                ),
                subject_fqns: None,
            };

            match &span.kind {
                SymbolKind::ClassReference { name, is_fqn, .. } => {
                    let resolved = if *is_fqn {
                        name.clone()
                    } else if let Some(fqn) =
                        resolved_names.as_ref().and_then(|rn| rn.get(span.start))
                    {
                        fqn.to_string()
                    } else {
                        resolve_to_fqn(name, &use_map, &namespace)
                    };
                    index.insert(ReferenceIndexKey::Class(normalize_fqn(&resolved)), entry);
                }
                SymbolKind::ClassDeclaration { name } => {
                    index.insert(
                        ReferenceIndexKey::Class(build_fqn(name, namespace.as_deref())),
                        entry,
                    );
                }
                SymbolKind::SelfStaticParent(kind) => {
                    if *kind == SelfStaticParentKind::This {
                        continue;
                    }
                    if let Some(fqn) =
                        self.resolve_keyword_to_fqn(kind, uri, &namespace, span.start)
                    {
                        index.insert(ReferenceIndexKey::Class(normalize_fqn(&fqn)), entry);
                    }
                }
                SymbolKind::FunctionCall {
                    name,
                    is_definition: _,
                } => {
                    let resolved = if let Some(fqn) =
                        resolved_names.as_ref().and_then(|rn| rn.get(span.start))
                    {
                        fqn.to_string()
                    } else {
                        resolve_to_fqn(name, &use_map, &namespace)
                    };
                    index.insert(
                        ReferenceIndexKey::Function(normalize_fqn(&resolved)),
                        entry.clone(),
                    );
                    index.insert(
                        ReferenceIndexKey::Function(name.clone()),
                        ReferenceIndexEntry {
                            uri: uri.to_string(),
                            start: span.start,
                            end: span.end,
                            range: entry.range,
                            is_declaration: entry.is_declaration,
                            subject_fqns: None,
                        },
                    );
                }
                SymbolKind::ConstantReference { name } => {
                    index.insert(ReferenceIndexKey::Constant(name.clone()), entry);
                }
                SymbolKind::MemberAccess {
                    subject_text,
                    member_name,
                    is_static,
                    ..
                } => {
                    let subject_fqns =
                        cacheable_member_subject_fqns(subject_text, *is_static, &ctx, span.start);
                    let member_entry = ReferenceIndexEntry {
                        subject_fqns,
                        ..entry
                    };
                    index.insert(
                        ReferenceIndexKey::Member {
                            name: member_name.clone(),
                            is_static: *is_static,
                        },
                        member_entry,
                    );
                }
                SymbolKind::MemberDeclaration {
                    name: member_name,
                    is_static,
                } => {
                    let subject_fqns = find_class_at_offset(&ctx.classes, span.start)
                        .map(|cc| vec![normalize_fqn(cc.fqn().as_ref())]);
                    let member_entry = ReferenceIndexEntry {
                        subject_fqns,
                        ..entry
                    };
                    index.insert(
                        ReferenceIndexKey::Member {
                            name: member_name.clone(),
                            is_static: *is_static,
                        },
                        member_entry,
                    );
                }
                _ => {}
            }
        }
    }

    fn should_index_references_for_uri(&self, uri: &str) -> bool {
        if uri.starts_with("phpantom-stub://") || uri.starts_with("phpantom-stub-fn://") {
            return false;
        }

        let vendor_prefixes = self.vendor_uri_prefixes.lock().clone();
        !vendor_prefixes.iter().any(|p| uri.starts_with(p.as_str()))
    }

    fn reference_index_entries_prefiltered(
        &self,
        key: ReferenceIndexKey,
        include_declaration: bool,
        needles: &[&str],
    ) -> Vec<ReferenceIndexEntry> {
        self.ensure_reference_candidates_indexed(needles);
        self.reference_index_entries_without_indexing(key, include_declaration)
    }

    fn reference_index_entries_without_indexing(
        &self,
        key: ReferenceIndexKey,
        include_declaration: bool,
    ) -> Vec<ReferenceIndexEntry> {
        let entries = self.reference_index.read().entries(&key);
        entries
            .into_iter()
            .filter(|entry| include_declaration || !entry.is_declaration)
            .filter(|entry| self.should_index_references_for_uri(&entry.uri))
            .collect()
    }

    fn locations_from_index_entries(&self, entries: &[ReferenceIndexEntry]) -> Vec<Location> {
        let mut locations = Vec::with_capacity(entries.len());

        for entry in entries {
            let parsed_uri = match Url::parse(&entry.uri) {
                Ok(uri) => uri,
                Err(_) => continue,
            };
            push_unique_location(
                &mut locations,
                &parsed_uri,
                entry.range.start,
                entry.range.end,
            );
        }

        locations.sort_by(|a, b| {
            a.uri
                .as_str()
                .cmp(b.uri.as_str())
                .then(a.range.start.line.cmp(&b.range.start.line))
                .then(a.range.start.character.cmp(&b.range.start.character))
        });
        locations
    }

    /// Find all references to a class/interface/trait/enum across all files.
    ///
    /// Matches `ClassReference` spans whose resolved FQN equals `target_fqn`,
    /// and optionally `ClassDeclaration` spans at the declaration site.
    fn find_class_references(&self, target_fqn: &str, include_declaration: bool) -> Vec<Location> {
        let mut locations = Vec::new();

        // Normalise: strip leading backslash if present.
        let target = strip_fqn_prefix(target_fqn);
        let target_short = crate::util::short_name(target);

        let class_needles = [target_short];
        let mut entries = self.reference_index_entries_prefiltered(
            ReferenceIndexKey::Class(target.to_string()),
            include_declaration,
            &class_needles,
        );
        if target_short != target {
            entries.extend(self.reference_index_entries_prefiltered(
                ReferenceIndexKey::Class(target_short.to_string()),
                include_declaration,
                &class_needles,
            ));
        }
        if !entries.is_empty() {
            return self.locations_from_index_entries(&entries);
        }

        // Snapshot user-file symbol maps (excludes vendor and stubs).
        let snapshot = self.user_file_symbol_maps();

        for (file_uri, symbol_map) in &snapshot {
            // Prefer mago-names resolved_names for FQN resolution (byte-offset
            // based, applies PHP's full name resolution rules).  Falls back to
            // the legacy use_map lazily for identifiers not tracked by
            // mago-names (e.g. docblock-sourced references).
            let resolved_names = self.resolved_names.read().get(file_uri).cloned();
            let file_namespace = self.namespace_map.read().get(file_uri).cloned().flatten();
            let file_use_map = std::cell::OnceCell::new();

            let parsed_uri = match Url::parse(file_uri) {
                Ok(u) => u,
                Err(_) => continue,
            };

            let content = match self.get_file_content_arc(file_uri) {
                Some(c) => c,
                None => continue,
            };

            for span in &symbol_map.spans {
                match &span.kind {
                    SymbolKind::ClassReference { name, is_fqn, .. } => {
                        let resolved = if *is_fqn {
                            name.clone()
                        } else if let Some(fqn) =
                            resolved_names.as_ref().and_then(|rn| rn.get(span.start))
                        {
                            fqn.to_string()
                        } else {
                            // Fallback for offsets not tracked by mago-names
                            // (e.g. docblock-sourced ClassReference spans).
                            let use_map = file_use_map.get_or_init(|| {
                                self.use_map
                                    .read()
                                    .get(file_uri)
                                    .cloned()
                                    .unwrap_or_default()
                            });
                            Self::resolve_to_fqn(name, use_map, &file_namespace)
                        };
                        // Input boundary: resolve_to_fqn may return a leading `\`.
                        let resolved_normalized = strip_fqn_prefix(&resolved);
                        if !class_names_match(resolved_normalized, target, target_short) {
                            continue;
                        }
                        let start = offset_to_position(&content, span.start as usize);
                        let end = offset_to_position(&content, span.end as usize);
                        locations.push(Location {
                            uri: parsed_uri.clone(),
                            range: Range { start, end },
                        });
                    }
                    SymbolKind::ClassDeclaration { name } if include_declaration => {
                        let fqn = build_fqn(name, file_namespace.as_deref());
                        if !class_names_match(&fqn, target, target_short) {
                            continue;
                        }
                        let start = offset_to_position(&content, span.start as usize);
                        let end = offset_to_position(&content, span.end as usize);
                        locations.push(Location {
                            uri: parsed_uri.clone(),
                            range: Range { start, end },
                        });
                    }
                    SymbolKind::SelfStaticParent(ssp_kind) => {
                        // self/static/parent resolve to the current class —
                        // include them if they resolve to the target FQN.
                        //
                        // Skip `$this` — it is handled as a variable, not a
                        // class reference.
                        if *ssp_kind == SelfStaticParentKind::This {
                            continue;
                        }
                        if let Some(fqn) = self.resolve_keyword_to_fqn(
                            ssp_kind,
                            file_uri,
                            &file_namespace,
                            span.start,
                        ) && class_names_match(&fqn, target, target_short)
                        {
                            let start = offset_to_position(&content, span.start as usize);
                            let end = offset_to_position(&content, span.end as usize);
                            locations.push(Location {
                                uri: parsed_uri.clone(),
                                range: Range { start, end },
                            });
                        }
                    }
                    _ => {}
                }
            }
        }

        // Sort: by URI, then by position.
        locations.sort_by(|a, b| {
            a.uri
                .as_str()
                .cmp(b.uri.as_str())
                .then(a.range.start.line.cmp(&b.range.start.line))
                .then(a.range.start.character.cmp(&b.range.start.character))
        });

        locations
    }

    /// Find all references to a member (method, property, or constant)
    /// across all files.
    ///
    /// When `hierarchy` is `Some`, only references where the subject
    /// resolves to a class in the given set of FQNs are returned.  When
    /// the subject cannot be resolved (e.g. a complex expression or an
    /// untyped variable), the reference is conservatively included.
    ///
    /// When `hierarchy` is `None`, all references with a matching member
    /// name and static-ness are returned (the v1 behaviour, kept as a
    /// fallback when the target class cannot be determined).
    fn find_member_references(
        &self,
        target_member: &str,
        target_is_static: bool,
        include_declaration: bool,
        hierarchy: Option<&HashSet<String>>,
    ) -> Vec<Location> {
        let mut locations = Vec::new();

        let member_needles = [target_member];
        let entries = self.reference_index_entries_prefiltered(
            ReferenceIndexKey::Member {
                name: target_member.to_string(),
                is_static: target_is_static,
            },
            include_declaration,
            &member_needles,
        );
        if !entries.is_empty() {
            // The index already filtered to entries whose key matches
            // (target_member, target_is_static), so we trust the key
            // and skip a redundant kind/name re-check. The hot work
            // per entry is the hierarchy filter, then materialising
            // a Location from the cached `entry.range`.
            //
            // `ctx_cache` is only consulted on the variable-subject
            // fallback path (entry.subject_fqns == None); for the
            // cached cases ($this/self/static/parent/bare class name)
            // the lookup never touches symbol_maps or the file
            // contents at all.
            let mut ctx_cache: HashMap<String, crate::types::FileContext> = HashMap::new();
            for entry in &entries {
                if let Some(hier) = hierarchy {
                    let keep = match &entry.subject_fqns {
                        Some(fqns) => {
                            // Empty Vec is reserved for "couldn't
                            // resolve at index time" — keep the
                            // conservative-include semantics.
                            fqns.is_empty() || fqns.iter().any(|fqn| hier.contains(fqn))
                        }
                        None => self.member_entry_subject_in_hier(entry, hier, &mut ctx_cache),
                    };
                    if !keep {
                        continue;
                    }
                }

                let Ok(parsed_uri) = Url::parse(&entry.uri) else {
                    continue;
                };
                locations.push(Location {
                    uri: parsed_uri,
                    range: entry.range,
                });
            }

            if include_declaration {
                self.add_property_declaration_references(
                    &mut locations,
                    target_member,
                    target_is_static,
                    hierarchy,
                );
            }

            locations.sort_by(|a, b| {
                a.uri
                    .as_str()
                    .cmp(b.uri.as_str())
                    .then(a.range.start.line.cmp(&b.range.start.line))
                    .then(a.range.start.character.cmp(&b.range.start.character))
            });
            return locations;
        }

        let snapshot = self.user_file_symbol_maps();

        for (file_uri, symbol_map) in &snapshot {
            let parsed_uri = match Url::parse(file_uri) {
                Ok(u) => u,
                Err(_) => continue,
            };

            let content = match self.get_file_content_arc(file_uri) {
                Some(c) => c,
                None => continue,
            };

            // Lazily resolved file context — only computed when we need
            // to check a candidate's subject against the hierarchy.
            let file_ctx_cell: std::cell::OnceCell<crate::types::FileContext> =
                std::cell::OnceCell::new();

            for span in &symbol_map.spans {
                match &span.kind {
                    SymbolKind::MemberAccess {
                        subject_text,
                        member_name,
                        is_static,
                        ..
                    } if member_name == target_member && *is_static == target_is_static => {
                        // Check if the subject belongs to the target hierarchy.
                        if let Some(hier) = hierarchy {
                            let ctx = file_ctx_cell.get_or_init(|| self.file_context(file_uri));
                            let subject_fqns = self.resolve_subject_to_fqns(
                                subject_text,
                                *is_static,
                                ctx,
                                span.start,
                                &content,
                            );
                            if !subject_fqns.is_empty()
                                && !subject_fqns.iter().any(|fqn| hier.contains(fqn))
                            {
                                // Subject resolved but none of the resolved
                                // classes are in the target hierarchy — skip.
                                continue;
                            }
                            // If subject_fqns is empty, we couldn't resolve
                            // the subject — include conservatively.
                        }

                        let start = offset_to_position(&content, span.start as usize);
                        let end = offset_to_position(&content, span.end as usize);
                        locations.push(Location {
                            uri: parsed_uri.clone(),
                            range: Range { start, end },
                        });
                    }
                    SymbolKind::MemberDeclaration { name, is_static }
                        if include_declaration
                            && name == target_member
                            && *is_static == target_is_static =>
                    {
                        // Check if the enclosing class is in the hierarchy.
                        if let Some(hier) = hierarchy {
                            let ctx = file_ctx_cell.get_or_init(|| self.file_context(file_uri));
                            if let Some(enclosing) = find_class_at_offset(&ctx.classes, span.start)
                            {
                                let fqn = enclosing.fqn().to_string();
                                if !hier.contains(&fqn) {
                                    continue;
                                }
                            }
                        }

                        let start = offset_to_position(&content, span.start as usize);
                        let end = offset_to_position(&content, span.end as usize);
                        locations.push(Location {
                            uri: parsed_uri.clone(),
                            range: Range { start, end },
                        });
                    }
                    _ => {}
                }
            }

            // Property declarations use Variable spans (not
            // MemberDeclaration) because GTD relies on the Variable
            // kind to jump to the type hint.  Scan the ast_map to
            // pick up property declaration sites.
            if include_declaration && let Some(classes) = self.get_classes_for_uri(file_uri) {
                for class in &classes {
                    // Filter by hierarchy when available.
                    if let Some(hier) = hierarchy {
                        let class_fqn = class.fqn().to_string();
                        if !hier.contains(&class_fqn) {
                            continue;
                        }
                    }

                    for prop in &class.properties {
                        let prop_name = prop.name.strip_prefix('$').unwrap_or(&prop.name);
                        let target_name = target_member.strip_prefix('$').unwrap_or(target_member);
                        if prop_name == target_name
                            && prop.is_static == target_is_static
                            && prop.name_offset != 0
                        {
                            let offset = prop.name_offset;
                            let start = offset_to_position(&content, offset as usize);
                            let end =
                                offset_to_position(&content, offset as usize + prop.name.len());
                            push_unique_location(&mut locations, &parsed_uri, start, end);
                        }
                    }
                }
            }
        }

        locations.sort_by(|a, b| {
            a.uri
                .as_str()
                .cmp(b.uri.as_str())
                .then(a.range.start.line.cmp(&b.range.start.line))
                .then(a.range.start.character.cmp(&b.range.start.character))
        });

        locations
    }

    /// Variable-subject fallback for the cached find-references path.
    ///
    /// Only invoked when `entry.subject_fqns` is `None` — i.e. the
    /// subject was a typed variable whose hierarchy can't be safely
    /// pre-resolved at index-build time. Pulls `subject_text` and
    /// `is_static` from the canonical symbol map for that URI/span,
    /// then runs `resolve_subject_to_fqns` against the live
    /// `FileContext` (cached per-URI by the caller).
    ///
    /// Returns `true` when the entry should be kept (subject resolves
    /// into the hierarchy, or couldn't be resolved at all — the
    /// historical conservative-include semantics).
    fn member_entry_subject_in_hier(
        &self,
        entry: &ReferenceIndexEntry,
        hier: &HashSet<String>,
        ctx_cache: &mut HashMap<String, crate::types::FileContext>,
    ) -> bool {
        let Some(content) = self.get_file_content_arc(&entry.uri) else {
            return false;
        };
        let Some(symbol_map) = self.symbol_maps.read().get(&entry.uri).cloned() else {
            return false;
        };
        let Some(span) = symbol_map
            .spans
            .iter()
            .find(|s| s.start == entry.start && s.end == entry.end)
        else {
            return false;
        };
        let SymbolKind::MemberAccess {
            subject_text,
            is_static,
            ..
        } = &span.kind
        else {
            return false;
        };

        let ctx = ctx_cache
            .entry(entry.uri.clone())
            .or_insert_with(|| self.file_context(&entry.uri));
        let subject_fqns =
            self.resolve_subject_to_fqns(subject_text, *is_static, ctx, span.start, &content);

        // Conservative-include: when the subject can't be resolved
        // (e.g. complex expression), keep the reference rather than
        // silently hiding it.
        if subject_fqns.is_empty() {
            return true;
        }
        subject_fqns.iter().any(|fqn| hier.contains(fqn))
    }

    fn add_property_declaration_references(
        &self,
        locations: &mut Vec<Location>,
        target_member: &str,
        target_is_static: bool,
        hierarchy: Option<&HashSet<String>>,
    ) {
        let ast_snapshot: Vec<(String, Vec<Arc<ClassInfo>>)> = self
            .ast_map
            .read()
            .iter()
            .filter(|(uri, _)| self.should_index_references_for_uri(uri))
            .map(|(uri, classes)| (uri.clone(), classes.clone()))
            .collect();

        for (file_uri, classes) in ast_snapshot {
            let parsed_uri = match Url::parse(&file_uri) {
                Ok(uri) => uri,
                Err(_) => continue,
            };
            let content = match self.get_file_content_arc(&file_uri) {
                Some(content) => content,
                None => continue,
            };

            for class in &classes {
                if let Some(hier) = hierarchy {
                    let class_fqn = class.fqn().to_string();
                    if !hier.contains(&class_fqn) {
                        continue;
                    }
                }

                for prop in &class.properties {
                    let prop_name = prop.name.strip_prefix('$').unwrap_or(&prop.name);
                    let target_name = target_member.strip_prefix('$').unwrap_or(target_member);
                    if prop_name == target_name
                        && prop.is_static == target_is_static
                        && prop.name_offset != 0
                    {
                        let offset = prop.name_offset;
                        let start = offset_to_position(&content, offset as usize);
                        let end = offset_to_position(&content, offset as usize + prop.name.len());
                        push_unique_location(locations, &parsed_uri, start, end);
                    }
                }
            }
        }
    }

    /// Find all references to a function across all files.
    fn find_function_references(
        &self,
        target_fqn: &str,
        target_short: &str,
        include_declaration: bool,
    ) -> Vec<Location> {
        let mut locations = Vec::new();

        // Input boundary: callers may pass FQNs with a leading `\`.
        let target = strip_fqn_prefix(target_fqn);
        let target_short_name = crate::util::short_name(target);

        let function_needles = [target_short_name, target_short];
        let mut entries = self.reference_index_entries_prefiltered(
            ReferenceIndexKey::Function(target.to_string()),
            include_declaration,
            &function_needles,
        );
        if target_short_name != target {
            entries.extend(self.reference_index_entries_prefiltered(
                ReferenceIndexKey::Function(target_short_name.to_string()),
                include_declaration,
                &function_needles,
            ));
        }
        if target_short_name != target_short {
            entries.extend(self.reference_index_entries_prefiltered(
                ReferenceIndexKey::Function(target_short.to_string()),
                include_declaration,
                &function_needles,
            ));
        }
        if !entries.is_empty() {
            return self.locations_from_index_entries(&entries);
        }

        let snapshot = self.user_file_symbol_maps();

        for (file_uri, symbol_map) in &snapshot {
            // Prefer mago-names resolved_names; lazy-load use_map only
            // when an offset is not tracked (e.g. docblock references).
            let resolved_names = self.resolved_names.read().get(file_uri).cloned();
            let file_namespace = self.namespace_map.read().get(file_uri).cloned().flatten();
            let file_use_map = std::cell::OnceCell::new();

            let parsed_uri = match Url::parse(file_uri) {
                Ok(u) => u,
                Err(_) => continue,
            };

            let content = match self.get_file_content_arc(file_uri) {
                Some(c) => c,
                None => continue,
            };

            for span in &symbol_map.spans {
                if let SymbolKind::FunctionCall {
                    name,
                    is_definition,
                } = &span.kind
                {
                    if *is_definition && !include_declaration {
                        continue;
                    }
                    let resolved = if let Some(fqn) =
                        resolved_names.as_ref().and_then(|rn| rn.get(span.start))
                    {
                        fqn.to_string()
                    } else {
                        let use_map = file_use_map.get_or_init(|| {
                            self.use_map
                                .read()
                                .get(file_uri)
                                .cloned()
                                .unwrap_or_default()
                        });
                        Self::resolve_to_fqn(name, use_map, &file_namespace)
                    };
                    // Input boundary: resolve_to_fqn may return a leading `\`.
                    let resolved_normalized = strip_fqn_prefix(&resolved);
                    if resolved_normalized != target
                        && crate::util::short_name(resolved_normalized)
                            != crate::util::short_name(target)
                    {
                        // Also try matching by short name when the
                        // namespaces don't line up (common for global
                        // functions referenced from within a namespace).
                        if name != target_short {
                            continue;
                        }
                    }
                    let start = offset_to_position(&content, span.start as usize);
                    let end = offset_to_position(&content, span.end as usize);
                    locations.push(Location {
                        uri: parsed_uri.clone(),
                        range: Range { start, end },
                    });
                }
            }
        }

        locations.sort_by(|a, b| {
            a.uri
                .as_str()
                .cmp(b.uri.as_str())
                .then(a.range.start.line.cmp(&b.range.start.line))
                .then(a.range.start.character.cmp(&b.range.start.character))
        });

        locations
    }

    /// Find all references to a constant across all files.
    fn find_constant_references(
        &self,
        target_name: &str,
        include_declaration: bool,
    ) -> Vec<Location> {
        let mut locations = Vec::new();

        let constant_needles = [target_name];
        let entries = self.reference_index_entries_prefiltered(
            ReferenceIndexKey::Constant(target_name.to_string()),
            include_declaration,
            &constant_needles,
        );
        if !entries.is_empty() {
            return self.locations_from_index_entries(&entries);
        }

        let snapshot = self.user_file_symbol_maps();

        for (file_uri, symbol_map) in &snapshot {
            let parsed_uri = match Url::parse(file_uri) {
                Ok(u) => u,
                Err(_) => continue,
            };

            let content = match self.get_file_content_arc(file_uri) {
                Some(c) => c,
                None => continue,
            };

            for span in &symbol_map.spans {
                if let SymbolKind::ConstantReference { name } = &span.kind {
                    if name != target_name {
                        continue;
                    }
                    let start = offset_to_position(&content, span.start as usize);
                    let end = offset_to_position(&content, span.end as usize);
                    locations.push(Location {
                        uri: parsed_uri.clone(),
                        range: Range { start, end },
                    });
                }
                // Include MemberDeclaration for constant declarations
                // when they match (class constants use MemberDeclaration).
                if include_declaration
                    && let SymbolKind::MemberDeclaration { name, is_static } = &span.kind
                    && name == target_name
                    && *is_static
                {
                    let start = offset_to_position(&content, span.start as usize);
                    let end = offset_to_position(&content, span.end as usize);
                    push_unique_location(&mut locations, &parsed_uri, start, end);
                }
            }
        }

        locations.sort_by(|a, b| {
            a.uri
                .as_str()
                .cmp(b.uri.as_str())
                .then(a.range.start.line.cmp(&b.range.start.line))
                .then(a.range.start.character.cmp(&b.range.start.character))
        });

        locations
    }

    fn resolve_keyword_to_fqn(
        &self,
        ssp_kind: &SelfStaticParentKind,
        uri: &str,
        namespace: &Option<String>,
        offset: u32,
    ) -> Option<String> {
        let classes: Vec<Arc<ClassInfo>> =
            self.ast_map.read().get(uri).cloned().unwrap_or_default();

        let current_class = crate::util::find_class_at_offset(&classes, offset)?;

        match ssp_kind {
            SelfStaticParentKind::Parent => current_class.parent_class.map(|a| a.to_string()),
            _ => {
                // self / static → current class FQN
                Some(build_fqn(&current_class.name, namespace.as_deref()))
            }
        }
    }

    // ─── Class hierarchy resolution for member references ───────────────────

    /// Resolve the class hierarchy for a `MemberAccess` subject.
    ///
    /// Returns `Some(set_of_fqns)` when the subject can be resolved to at
    /// least one class, or `None` when resolution fails entirely.
    fn resolve_member_access_hierarchy(
        &self,
        uri: &str,
        subject_text: &str,
        is_static: bool,
        span_start: u32,
    ) -> Option<HashSet<String>> {
        let ctx = self.file_context(uri);
        let content = self.get_file_content(uri)?;
        let fqns =
            self.resolve_subject_to_fqns(subject_text, is_static, &ctx, span_start, &content);
        if fqns.is_empty() {
            return None;
        }
        Some(self.collect_hierarchy_for_fqns(&fqns))
    }

    /// Resolve the class hierarchy for a `MemberDeclaration` at a given offset.
    ///
    /// Finds the enclosing class and builds the hierarchy set from it.
    fn resolve_member_declaration_hierarchy(
        &self,
        uri: &str,
        offset: u32,
    ) -> Option<HashSet<String>> {
        let classes: Vec<Arc<ClassInfo>> =
            self.ast_map.read().get(uri).cloned().unwrap_or_default();
        let current_class = find_class_at_offset(&classes, offset)?;
        let fqn = current_class.fqn().to_string();
        Some(self.collect_hierarchy_for_fqns(&[fqn]))
    }

    /// Resolve a member access subject to zero or more class FQNs.
    ///
    /// This is a lightweight resolution path used during reference scanning.
    /// It handles the common cases (`self`, `static`, `$this`, `parent`,
    /// bare class names for static access, and typed `$variable` parameters)
    /// without the full weight of the completion resolver.
    fn resolve_subject_to_fqns(
        &self,
        subject_text: &str,
        is_static: bool,
        ctx: &crate::types::FileContext,
        access_offset: u32,
        content: &str,
    ) -> Vec<String> {
        let trimmed = subject_text.trim();

        match trimmed {
            "$this" | "self" | "static" => {
                if let Some(cls) =
                    self.find_enclosing_class_fqn(&ctx.classes, &ctx.namespace, access_offset)
                {
                    return vec![cls];
                }
                Vec::new()
            }
            "parent" => {
                if let Some(cc) = find_class_at_offset(&ctx.classes, access_offset)
                    && let Some(ref parent) = cc.parent_class
                {
                    let fqn = ctx.resolve_name_at(parent, access_offset);
                    return vec![normalize_fqn(&fqn)];
                }
                Vec::new()
            }
            _ if is_static && !trimmed.starts_with('$') => {
                // Bare class name for static access: `ClassName::method()`.
                let fqn = ctx.resolve_name_at(trimmed, access_offset);
                vec![normalize_fqn(&fqn)]
            }
            _ if trimmed.starts_with('$') => {
                // Variable — try variable type resolution.
                self.resolve_variable_to_fqns(trimmed, ctx, access_offset, content)
            }
            _ => Vec::new(),
        }
    }

    /// Try to resolve a variable to its class FQN(s) using the type
    /// resolution engine.
    fn resolve_variable_to_fqns(
        &self,
        var_name: &str,
        ctx: &crate::types::FileContext,
        cursor_offset: u32,
        content: &str,
    ) -> Vec<String> {
        let enclosing_class = find_class_at_offset(&ctx.classes, cursor_offset)
            .cloned()
            .unwrap_or_default();

        let class_loader = self.class_loader(ctx);
        let function_loader = self.function_loader(ctx);

        let resolved = ResolvedType::into_classes(
            crate::completion::variable::resolution::resolve_variable_types(
                var_name,
                &enclosing_class,
                &ctx.classes,
                content,
                cursor_offset,
                &class_loader,
                Loaders::with_function(Some(&function_loader)),
            ),
        );

        resolved
            .into_iter()
            .map(|ci| normalize_fqn(&ci.fqn()))
            .collect()
    }

    /// Find the FQN of the class enclosing a given byte offset.
    fn find_enclosing_class_fqn(
        &self,
        classes: &[Arc<ClassInfo>],
        namespace: &Option<String>,
        offset: u32,
    ) -> Option<String> {
        let cc = find_class_at_offset(classes, offset)?;
        let fqn = build_fqn(&cc.name, namespace.as_deref());
        Some(normalize_fqn(&fqn))
    }

    /// Collect the full class hierarchy (ancestors and descendants) for
    /// a set of starting FQNs.
    ///
    /// The result includes:
    /// - The starting FQNs themselves
    /// - All ancestor FQNs (parent chain, interfaces, traits)
    /// - All descendant FQNs (classes that extend/implement any class in
    ///   the hierarchy)
    fn collect_hierarchy_for_fqns(&self, seed_fqns: &[String]) -> HashSet<String> {
        let mut hierarchy = HashSet::new();
        let class_loader = |name: &str| -> Option<Arc<ClassInfo>> { self.find_or_load_class(name) };

        // Insert the seeds.
        for fqn in seed_fqns {
            hierarchy.insert(fqn.clone());
        }

        // Walk up: collect all ancestors for each seed.
        for fqn in seed_fqns {
            self.collect_ancestors(fqn, &class_loader, &mut hierarchy);
        }

        // Walk down: collect all descendants from ast_map and class_index.
        // We iterate until no new FQNs are added (transitive closure).
        let mut changed = true;
        let mut depth = 0u32;
        while changed && depth < MAX_INHERITANCE_DEPTH {
            changed = false;
            depth += 1;

            // Snapshot the current hierarchy to check against.
            let current: Vec<String> = hierarchy.iter().cloned().collect();

            // Scan all known classes for ones that extend/implement/use
            // anything in the current hierarchy.
            let all_classes: Vec<ClassInfo> = {
                let map = self.ast_map.read();
                map.values()
                    .flat_map(|classes| classes.iter().map(|c| ClassInfo::clone(c)))
                    .collect()
            };

            for cls in &all_classes {
                let cls_fqn = normalize_fqn(&cls.fqn());
                if hierarchy.contains(&cls_fqn) {
                    continue;
                }

                if self.class_is_descendant_of(cls, &current, &class_loader) {
                    hierarchy.insert(cls_fqn);
                    changed = true;
                }
            }

            // Also check class_index entries not yet in ast_map.
            let index_entries: Vec<String> = {
                let idx = self.class_index.read();
                idx.keys().cloned().collect()
            };

            for fqn in &index_entries {
                let normalized = normalize_fqn(fqn);
                if hierarchy.contains(&normalized) {
                    continue;
                }

                if let Some(cls) = class_loader(fqn)
                    && self.class_is_descendant_of(&cls, &current, &class_loader)
                {
                    hierarchy.insert(normalized);
                    changed = true;
                }
            }
        }

        hierarchy
    }

    /// Walk up the inheritance chain and collect all ancestor FQNs.
    fn collect_ancestors(
        &self,
        fqn: &str,
        class_loader: &dyn Fn(&str) -> Option<Arc<ClassInfo>>,
        hierarchy: &mut HashSet<String>,
    ) {
        let cls = match class_loader(fqn) {
            Some(c) => c,
            None => return,
        };

        // Parent class chain.
        if let Some(ref parent) = cls.parent_class {
            let parent_fqn = normalize_fqn(parent);
            if hierarchy.insert(parent_fqn.clone()) {
                self.collect_ancestors(&parent_fqn, class_loader, hierarchy);
            }
        }

        // Interfaces.
        for iface in &cls.interfaces {
            let iface_fqn = normalize_fqn(iface);
            if hierarchy.insert(iface_fqn.clone()) {
                self.collect_ancestors(&iface_fqn, class_loader, hierarchy);
            }
        }

        // Used traits.
        for trait_name in &cls.used_traits {
            let trait_fqn = normalize_fqn(trait_name);
            if hierarchy.insert(trait_fqn.clone()) {
                self.collect_ancestors(&trait_fqn, class_loader, hierarchy);
            }
        }

        // Mixins.
        for mixin in &cls.mixins {
            let mixin_fqn = normalize_fqn(mixin);
            if hierarchy.insert(mixin_fqn.clone()) {
                self.collect_ancestors(&mixin_fqn, class_loader, hierarchy);
            }
        }
    }

    /// Check whether a class directly extends, implements, or uses
    /// anything in the given set of FQNs.
    fn class_is_descendant_of(
        &self,
        cls: &ClassInfo,
        targets: &[String],
        class_loader: &dyn Fn(&str) -> Option<Arc<ClassInfo>>,
    ) -> bool {
        // Direct parent.
        if let Some(ref parent) = cls.parent_class {
            let parent_fqn = normalize_fqn(parent);
            if targets.contains(&parent_fqn) {
                return true;
            }
            // Transitive: walk the parent chain.
            if self.ancestor_in_set(&parent_fqn, targets, class_loader, 0) {
                return true;
            }
        }

        // Direct interfaces.
        for iface in &cls.interfaces {
            let iface_fqn = normalize_fqn(iface);
            if targets.contains(&iface_fqn) {
                return true;
            }
            if self.ancestor_in_set(&iface_fqn, targets, class_loader, 0) {
                return true;
            }
        }

        // Used traits.
        for trait_name in &cls.used_traits {
            let trait_fqn = normalize_fqn(trait_name);
            if targets.contains(&trait_fqn) {
                return true;
            }
        }

        // Mixins.
        for mixin in &cls.mixins {
            let mixin_fqn = normalize_fqn(mixin);
            if targets.contains(&mixin_fqn) {
                return true;
            }
        }

        false
    }

    /// Recursively check whether any ancestor of `fqn` is in the target set.
    fn ancestor_in_set(
        &self,
        fqn: &str,
        targets: &[String],
        class_loader: &dyn Fn(&str) -> Option<Arc<ClassInfo>>,
        depth: u32,
    ) -> bool {
        if depth >= MAX_INHERITANCE_DEPTH {
            return false;
        }

        let cls = match class_loader(fqn) {
            Some(c) => c,
            None => return false,
        };

        if let Some(ref parent) = cls.parent_class {
            let parent_fqn = normalize_fqn(parent);
            if targets.contains(&parent_fqn) {
                return true;
            }
            if self.ancestor_in_set(&parent_fqn, targets, class_loader, depth + 1) {
                return true;
            }
        }

        for iface in &cls.interfaces {
            let iface_fqn = normalize_fqn(iface);
            if targets.contains(&iface_fqn) {
                return true;
            }
            if self.ancestor_in_set(&iface_fqn, targets, class_loader, depth + 1) {
                return true;
            }
        }

        false
    }

    /// Ensure all workspace PHP files have been parsed and have symbol maps.
    ///
    /// This lazily parses files that are in the workspace directory but
    /// have not been opened or indexed yet.  It also covers files known
    /// via the classmap and class_index.  The vendor directory (read from
    /// `composer.json` `config.vendor-dir`, defaulting to `vendor`) is
    /// skipped during the filesystem walk.
    pub(crate) fn ensure_workspace_indexed(&self) {
        if self.reference_workspace_indexed.load(Ordering::Acquire) {
            return;
        }

        let started = std::time::Instant::now();
        // Collect URIs that already have symbol maps.
        let existing_uris: HashSet<String> = self.symbol_maps.read().keys().cloned().collect();

        // Build the vendor URI prefixes so we can skip vendor files in
        // Phase 1 (class_index may contain vendor URIs from prior
        // resolution, but we only need symbol maps for user files).
        let vendor_prefixes = self.vendor_uri_prefixes.lock().clone();

        // ── Phase 1: class_index files (user only) ─────────────────────
        // These are files we already know about from update_ast calls,
        // ensuring their symbol maps are populated.  Vendor files are
        // skipped — find references only reports user code.
        //
        // File content is read and parsed in parallel using
        // `std::thread::scope`.  Each thread reads one file from disk
        // and calls `update_ast` which acquires write locks briefly to
        // store the results.  The expensive parsing step runs without
        // any locks held.
        let index_uris: Vec<String> = self.class_index.read().values().cloned().collect();

        let phase1_uris: Vec<&String> = index_uris
            .iter()
            .filter(|uri| {
                !existing_uris.contains(*uri)
                    && !vendor_prefixes.iter().any(|p| uri.starts_with(p.as_str()))
                    && !uri.starts_with("phpantom-stub://")
                    && !uri.starts_with("phpantom-stub-fn://")
            })
            .collect();

        self.parse_files_parallel(
            phase1_uris
                .iter()
                .map(|uri| (uri.as_str(), None::<&str>))
                .collect(),
        );

        // ── Phase 2: workspace directory scan ───────────────────────────
        // Recursively discover PHP files in the workspace root that are
        // not yet indexed.  This catches files that are not in the
        // classmap, class_index, or already opened.  The vendor directory
        // is skipped — find references only reports user code.  The walk
        // respects .gitignore so that generated/cached directories (e.g.
        // storage/framework/views/, var/cache/, node_modules/) are
        // automatically excluded.
        let workspace_root = self.workspace_root.read().clone();

        if let Some(root) = workspace_root {
            // Re-read existing URIs after phase 1 may have added more.
            let existing_uris: HashSet<String> = self.symbol_maps.read().keys().cloned().collect();

            let phase2_work: Vec<(String, PathBuf)> = self
                .workspace_reference_files(&root)
                .into_iter()
                .filter_map(|(uri, path)| {
                    if existing_uris.contains(&uri) {
                        None
                    } else {
                        Some((uri, path))
                    }
                })
                .collect();

            self.parse_paths_parallel(&phase2_work);
        }

        self.reference_workspace_indexed
            .store(true, Ordering::Release);
        tracing::debug!(
            elapsed_ms = started.elapsed().as_millis(),
            files_indexed = self.symbol_maps.read().len(),
            "find-references workspace index ready"
        );
    }

    fn ensure_reference_candidates_indexed(&self, needles: &[&str]) {
        if self.reference_workspace_indexed.load(Ordering::Acquire) {
            return;
        }

        let missing_needles: Vec<String> = {
            let seen = self.reference_prefiltered_needles.read();
            needles
                .iter()
                .map(|needle| needle.trim())
                .filter(|needle| !needle.is_empty())
                .filter(|needle| !seen.contains(*needle))
                .map(ToOwned::to_owned)
                .collect()
        };

        if missing_needles.is_empty() {
            return;
        }

        let started = std::time::Instant::now();
        let matcher = NeedleMatcher::new(&missing_needles);
        let existing_uris: HashSet<String> = self.symbol_maps.read().keys().cloned().collect();
        let vendor_prefixes = self.vendor_uri_prefixes.lock().clone();

        let index_uris: Vec<String> = self.class_index.read().values().cloned().collect();
        let phase1: Vec<(String, String)> = index_uris
            .iter()
            .filter(|uri| {
                !existing_uris.contains(*uri)
                    && !vendor_prefixes.iter().any(|p| uri.starts_with(p.as_str()))
                    && !uri.starts_with("phpantom-stub://")
                    && !uri.starts_with("phpantom-stub-fn://")
            })
            .filter_map(|uri| {
                let content = self.get_file_content(uri)?;
                matcher
                    .matches_any(&content)
                    .then(|| (uri.clone(), content))
            })
            .collect();

        self.parse_files_parallel(
            phase1
                .iter()
                .map(|(uri, content)| (uri.as_str(), Some(content.as_str())))
                .collect(),
        );

        let workspace_root = self.workspace_root.read().clone();
        let mut used_identifier_index = false;
        let mut used_ripgrep = false;
        let mut parse_candidates_directly = false;
        self.wait_for_reference_identifier_index(std::time::Duration::from_millis(50), 25_000);
        if let Some(root) = workspace_root {
            let existing_uris: HashSet<String> = self.symbol_maps.read().keys().cloned().collect();
            let identifier_source =
                self.identifier_candidate_reference_files(&root, &missing_needles);
            used_identifier_index = identifier_source.is_some();
            let disk_loaded_identifier_index = used_identifier_index
                && self
                    .reference_identifier_index_disk_loaded
                    .load(Ordering::Acquire);
            let trusted_identifier_index = disk_loaded_identifier_index
                && self
                    .reference_identifier_index_trusted
                    .load(Ordering::Acquire);
            let ripgrep_source = if used_identifier_index {
                if disk_loaded_identifier_index && !trusted_identifier_index {
                    self.ripgrep_candidate_reference_files(&root, &missing_needles)
                } else {
                    None
                }
            } else {
                self.ripgrep_candidate_reference_files(&root, &missing_needles)
            };
            used_ripgrep = ripgrep_source.is_some();
            let phase2_source = match (identifier_source, ripgrep_source) {
                (Some(identifier_source), Some(ripgrep_source)) => {
                    parse_candidates_directly = true;
                    Self::merge_reference_file_candidates(identifier_source, ripgrep_source)
                }
                (Some(identifier_source), None)
                    if !disk_loaded_identifier_index || trusted_identifier_index =>
                {
                    parse_candidates_directly = true;
                    identifier_source
                }
                (Some(_), None) => self.workspace_reference_files(&root),
                (None, Some(ripgrep_source)) => {
                    parse_candidates_directly = true;
                    ripgrep_source
                }
                (None, None) => self.workspace_reference_files(&root),
            };
            let phase2_work: Vec<(String, PathBuf)> = phase2_source
                .into_iter()
                .filter_map(|(uri, path)| (!existing_uris.contains(&uri)).then_some((uri, path)))
                .collect();
            if parse_candidates_directly {
                self.parse_paths_parallel(&phase2_work);
            } else {
                self.parse_paths_parallel_prefiltered(&phase2_work, &matcher);
            }
        }

        {
            let mut seen = self.reference_prefiltered_needles.write();
            for needle in &missing_needles {
                seen.insert(needle.clone());
            }
        }

        tracing::debug!(
            elapsed_ms = started.elapsed().as_millis(),
            needles = ?missing_needles,
            indexed_files = self.symbol_maps.read().len(),
            used_identifier_index,
            used_ripgrep,
            "find-references target prefilter ready"
        );
    }

    pub(crate) fn finish_reference_identifier_index_warmup(&self) {
        let generation = self
            .reference_identifier_cache_generation
            .load(Ordering::Acquire);
        let result = (|| {
            if self.shutdown_flag.load(Ordering::Acquire) {
                return None;
            }

            let root = self.workspace_root.read().clone()?;
            let started = std::time::Instant::now();
            let files = self.workspace_reference_files(&root);

            if let Some(index) = load_reference_identifier_index_cache(&root, &files) {
                tracing::debug!(
                    elapsed_ms = started.elapsed().as_millis(),
                    files = files.len(),
                    identifiers = index.len(),
                    "find-references identifier index loaded from disk cache"
                );
                self.reference_identifier_index_disk_loaded
                    .store(true, Ordering::Release);
                self.reference_identifier_index_trusted
                    .store(true, Ordering::Release);
                return Some(index);
            }

            let index = build_identifier_file_index(&files);
            self.reference_identifier_index_disk_loaded
                .store(false, Ordering::Release);
            self.reference_identifier_index_trusted
                .store(true, Ordering::Release);

            tracing::debug!(
                elapsed_ms = started.elapsed().as_millis(),
                files = files.len(),
                identifiers = index.len(),
                "find-references identifier index ready"
            );

            spawn_save_reference_identifier_index_cache(root.clone(), files.clone(), index.clone());

            Some(index)
        })();

        if let Some(index) = result {
            if self
                .reference_identifier_cache_generation
                .load(Ordering::Acquire)
                == generation
            {
                *self.reference_identifier_index.write() = Some(index);
            } else {
                self.reference_identifier_index_disk_loaded
                    .store(false, Ordering::Release);
                self.reference_identifier_index_trusted
                    .store(false, Ordering::Release);
            }
        }
        self.reference_identifier_indexing
            .store(false, Ordering::Release);
    }

    fn wait_for_reference_identifier_index(
        &self,
        max_wait: std::time::Duration,
        min_workspace_files: usize,
    ) {
        if self.reference_identifier_index.read().is_some()
            || !self.reference_identifier_indexing.load(Ordering::Acquire)
        {
            return;
        }

        let should_wait = self
            .reference_workspace_files
            .read()
            .as_ref()
            .is_some_and(|files| files.len() >= min_workspace_files);
        if !should_wait {
            return;
        }

        let started = std::time::Instant::now();
        while started.elapsed() < max_wait {
            if self.reference_identifier_index.read().is_some()
                || !self.reference_identifier_indexing.load(Ordering::Acquire)
            {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    fn identifier_candidate_reference_files(
        &self,
        root: &std::path::Path,
        needles: &[String],
    ) -> Option<Vec<(String, PathBuf)>> {
        let index = self.reference_identifier_index.read();
        let index = index.as_ref()?;

        let files = self.workspace_reference_files(root);
        let mut file_indices = HashSet::new();
        for needle in needles {
            let normalized = needle.strip_prefix('$').unwrap_or(needle);
            if let Some(indices) = index.get(normalized) {
                file_indices.extend(indices.iter().copied());
            }
        }

        let mut file_indices: Vec<usize> = file_indices.into_iter().collect();
        file_indices.sort_unstable();

        Some(
            file_indices
                .into_iter()
                .filter_map(|idx| files.get(idx).cloned())
                .collect(),
        )
    }

    fn ripgrep_candidate_reference_files(
        &self,
        root: &std::path::Path,
        needles: &[String],
    ) -> Option<Vec<(String, PathBuf)>> {
        let mut cmd = Command::new("rg");
        cmd.current_dir(root)
            .arg("--files-with-matches")
            .arg("--null")
            .arg("--fixed-strings")
            .arg("--no-messages")
            .arg("--glob")
            .arg("*.php")
            .arg("--glob")
            .arg("!**/vendor/**");

        for vendor_path in self.vendor_dir_paths.lock().iter() {
            if let Ok(relative) = vendor_path.strip_prefix(root) {
                let relative = relative.to_string_lossy().replace('\\', "/");
                if !relative.is_empty() {
                    cmd.arg("--glob").arg(format!("!{relative}/**"));
                }
            }
        }

        for needle in needles {
            cmd.arg("-e").arg(needle);
        }
        cmd.arg(".");

        let started = std::time::Instant::now();
        let output = cmd.output().ok()?;
        if !(output.status.success() || output.status.code() == Some(1)) {
            return None;
        }

        let mut files = Vec::new();
        let mut seen = HashSet::new();
        for raw_path in output.stdout.split(|byte| *byte == 0) {
            if raw_path.is_empty() {
                continue;
            }
            let raw_path = std::str::from_utf8(raw_path).ok()?;
            let path = root.join(raw_path);
            if !seen.insert(path.clone()) {
                continue;
            }
            files.push((crate::util::path_to_uri(&path), path));
        }

        tracing::debug!(
            elapsed_ms = started.elapsed().as_millis(),
            needles = ?needles,
            files = files.len(),
            "find-references ripgrep candidates ready"
        );

        Some(files)
    }

    fn workspace_reference_files(&self, root: &std::path::Path) -> Vec<(String, PathBuf)> {
        if let Some(files) = self.reference_workspace_files.read().clone() {
            return files;
        }

        let vendor_dir_paths = self.vendor_dir_paths.lock().clone();
        let mut files: Vec<(String, PathBuf)> =
            collect_php_files_gitignore(root, &vendor_dir_paths)
                .into_iter()
                .map(|path| (crate::util::path_to_uri(&path), path))
                .collect();
        files.sort_unstable_by(|(_, a), (_, b)| a.cmp(b));

        *self.reference_workspace_files.write() = Some(files.clone());
        files
    }

    pub(crate) fn invalidate_reference_identifier_cache_state(&self, clear_workspace_files: bool) {
        self.reference_identifier_cache_generation
            .fetch_add(1, Ordering::AcqRel);
        *self.reference_identifier_index.write() = None;
        self.reference_identifier_index_disk_loaded
            .store(false, Ordering::Release);
        self.reference_identifier_index_trusted
            .store(false, Ordering::Release);
        self.reference_prefiltered_needles.write().clear();

        if clear_workspace_files {
            *self.reference_workspace_files.write() = None;
            self.reference_workspace_indexed
                .store(false, Ordering::Release);
        }
    }

    pub(crate) fn update_reference_identifier_cache_for_uri_content(
        &self,
        uri: &str,
        content: &[u8],
    ) -> bool {
        self.update_reference_identifier_cache_for_uri_content_with_previous(
            uri, None, content, true,
        )
    }

    pub(crate) fn update_reference_identifier_cache_for_uri_content_with_previous(
        &self,
        uri: &str,
        previous_content: Option<&[u8]>,
        content: &[u8],
        clear_prefiltered_needles: bool,
    ) -> bool {
        let Some(files) = self.reference_workspace_files.read().clone() else {
            return false;
        };

        let file_idx = files
            .iter()
            .position(|(candidate_uri, _)| candidate_uri == uri);
        let Some(file_idx) = file_idx else {
            return false;
        };

        let mut index = self.reference_identifier_index.write();
        let Some(index) = index.as_mut() else {
            return false;
        };

        if let Some(previous_content) = previous_content {
            let mut empty_keys = Vec::new();
            for identifier in collect_file_identifiers(previous_content) {
                if let Some(file_indices) = index.get_mut(&identifier)
                    && let Ok(pos) = file_indices.binary_search(&file_idx)
                {
                    file_indices.remove(pos);
                    if file_indices.is_empty() {
                        empty_keys.push(identifier);
                    }
                }
            }
            for identifier in empty_keys {
                index.remove(&identifier);
            }
        } else {
            let mut empty_keys = Vec::new();
            for (identifier, file_indices) in index.iter_mut() {
                if let Ok(pos) = file_indices.binary_search(&file_idx) {
                    file_indices.remove(pos);
                    if file_indices.is_empty() {
                        empty_keys.push(identifier.clone());
                    }
                }
            }
            for identifier in empty_keys {
                index.remove(&identifier);
            }
        }

        for identifier in collect_file_identifiers(content) {
            let file_indices = index.entry(identifier).or_default();
            match file_indices.binary_search(&file_idx) {
                Ok(_) => {}
                Err(pos) => file_indices.insert(pos, file_idx),
            }
        }

        self.reference_identifier_cache_generation
            .fetch_add(1, Ordering::AcqRel);
        self.reference_identifier_index_disk_loaded
            .store(false, Ordering::Release);
        self.reference_identifier_index_trusted
            .store(true, Ordering::Release);
        if clear_prefiltered_needles {
            self.reference_prefiltered_needles.write().clear();
        }
        true
    }

    fn merge_reference_file_candidates(
        left: Vec<(String, PathBuf)>,
        right: Vec<(String, PathBuf)>,
    ) -> Vec<(String, PathBuf)> {
        let mut merged = Vec::with_capacity(left.len() + right.len());
        let mut seen = HashSet::new();

        for (uri, path) in left.into_iter().chain(right) {
            if seen.insert(uri.clone()) {
                merged.push((uri, path));
            }
        }

        merged
    }

    /// Parse a batch of files in parallel using OS threads.
    ///
    /// Each entry is `(uri, optional_content)`.  When `content` is `None`,
    /// the file is loaded via [`get_file_content`].  The expensive parsing
    /// step runs without any locks held; only the brief map insertions at
    /// the end of [`update_ast`] acquire write locks.
    ///
    /// Uses [`std::thread::scope`] for structured concurrency so that all
    /// spawned threads are guaranteed to finish before this method returns.
    /// The thread count is capped at the number of available CPU cores.
    fn parse_files_parallel(&self, files: Vec<(&str, Option<&str>)>) {
        if files.is_empty() {
            return;
        }

        // For very small batches, avoid thread overhead.
        if files.len() <= 2 {
            for (uri, content) in &files {
                if let Some(c) = content {
                    self.update_ast(uri, c);
                } else if let Some(c) = self.get_file_content(uri) {
                    self.update_ast(uri, &c);
                }
            }
            return;
        }

        let n_threads = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .min(files.len());

        let chunks: Vec<Vec<(&str, Option<&str>)>> = {
            let chunk_size = files.len().div_ceil(n_threads);
            files.chunks(chunk_size).map(|c| c.to_vec()).collect()
        };

        // Use a 16 MB stack per thread.  The default 8 MB can overflow
        // when parsing deeply-nested PHP files (e.g. WordPress
        // admin-bar.php) because `extract_symbol_map` recurses through
        // the full AST via `extract_from_expression` /
        // `extract_from_statement`.  Stack overflows are fatal
        // (abort, not panic) so `catch_unwind` cannot save us.
        const PARSE_STACK_SIZE: usize = 16 * 1024 * 1024;

        std::thread::scope(|s| {
            for chunk in &chunks {
                let handle = std::thread::Builder::new()
                    .stack_size(PARSE_STACK_SIZE)
                    .spawn_scoped(s, move || {
                        for (uri, content) in chunk {
                            if let Some(c) = content {
                                self.update_ast(uri, c);
                            } else if let Some(c) = self.get_file_content(uri) {
                                self.update_ast(uri, &c);
                            }
                        }
                    });
                if let Err(e) = handle {
                    tracing::error!("failed to spawn parse thread: {e}");
                }
            }
        });
    }

    /// Parse a batch of files from disk paths in parallel.
    ///
    /// Each entry is `(uri, path)`.  The file is read from disk and
    /// parsed in a worker thread.  Uses [`std::thread::scope`] for
    /// structured concurrency.
    pub(crate) fn parse_paths_parallel(&self, files: &[(String, PathBuf)]) {
        if files.is_empty() {
            return;
        }

        // For very small batches, avoid thread overhead.
        if files.len() <= 2 {
            for (uri, path) in files {
                if let Ok(content) = std::fs::read_to_string(path) {
                    self.update_ast(uri, &content);
                }
            }
            return;
        }

        let n_threads = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .min(files.len());

        let chunks: Vec<&[(String, PathBuf)]> = {
            let chunk_size = files.len().div_ceil(n_threads);
            files.chunks(chunk_size).collect()
        };

        const PARSE_STACK_SIZE: usize = 16 * 1024 * 1024;

        std::thread::scope(|s| {
            for chunk in &chunks {
                let handle = std::thread::Builder::new()
                    .stack_size(PARSE_STACK_SIZE)
                    .spawn_scoped(s, move || {
                        for (uri, path) in *chunk {
                            if let Ok(content) = std::fs::read_to_string(path) {
                                self.update_ast(uri, &content);
                            }
                        }
                    });
                if let Err(e) = handle {
                    tracing::error!("failed to spawn parse thread: {e}");
                }
            }
        });
    }

    fn parse_paths_parallel_prefiltered(
        &self,
        files: &[(String, PathBuf)],
        matcher: &NeedleMatcher,
    ) {
        if files.is_empty() || matcher.is_empty() {
            return;
        }

        if files.len() <= 2 {
            for (uri, path) in files {
                if let Ok(content) = std::fs::read_to_string(path)
                    && matcher.matches_any(&content)
                {
                    self.update_ast(uri, &content);
                }
            }
            return;
        }

        let n_threads = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .min(files.len());

        let chunks: Vec<&[(String, PathBuf)]> = {
            let chunk_size = files.len().div_ceil(n_threads);
            files.chunks(chunk_size).collect()
        };

        const PARSE_STACK_SIZE: usize = 16 * 1024 * 1024;

        std::thread::scope(|s| {
            for chunk in &chunks {
                let handle = std::thread::Builder::new()
                    .stack_size(PARSE_STACK_SIZE)
                    .spawn_scoped(s, move || {
                        for (uri, path) in *chunk {
                            if let Ok(content) = std::fs::read_to_string(path)
                                && matcher.matches_any(&content)
                            {
                                self.update_ast(uri, &content);
                            }
                        }
                    });
                if let Err(e) = handle {
                    tracing::error!("failed to spawn parse thread: {e}");
                }
            }
        });
    }
}

/// Normalise a class FQN: strip leading `\` if present.
fn normalize_fqn(fqn: &str) -> String {
    strip_fqn_prefix(fqn).to_string()
}

/// Resolve a `MemberAccess` subject to its owning class FQN(s) using only
/// AST-local information (no cross-file type resolution). Returns `None`
/// for variable subjects whose type may depend on cross-file state that
/// could become stale before dependent files are re-parsed — those must
/// fall back to runtime resolution at query time.
fn cacheable_member_subject_fqns(
    subject_text: &str,
    is_static: bool,
    ctx: &crate::types::FileContext,
    access_offset: u32,
) -> Option<Vec<String>> {
    let trimmed = subject_text.trim();

    match trimmed {
        "$this" | "self" | "static" => {
            let cls = find_class_at_offset(&ctx.classes, access_offset)?;
            let fqn = build_fqn(&cls.name, ctx.namespace.as_deref());
            Some(vec![normalize_fqn(&fqn)])
        }
        "parent" => {
            let cc = find_class_at_offset(&ctx.classes, access_offset)?;
            let parent = cc.parent_class.as_ref()?;
            let fqn = ctx.resolve_name_at(parent, access_offset);
            Some(vec![normalize_fqn(&fqn)])
        }
        _ if is_static && !trimmed.starts_with('$') => {
            let fqn = ctx.resolve_name_at(trimmed, access_offset);
            Some(vec![normalize_fqn(&fqn)])
        }
        _ => None,
    }
}

/// Pre-compiled substring matcher for the references prefilter.
///
/// `memchr::memmem::Finder` builds a small Boyer–Moore-style table per
/// needle; building it once per query and reusing across the workspace
/// walk avoids redoing that work for every file. Owned (`'static`)
/// finders are `Send + Sync`, so a single matcher can be shared with
/// the scoped parse threads.
struct NeedleMatcher {
    finders: Vec<memchr::memmem::Finder<'static>>,
}

impl NeedleMatcher {
    fn new<I, S>(needles: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<[u8]>,
    {
        Self {
            finders: needles
                .into_iter()
                .map(|n| memchr::memmem::Finder::new(n.as_ref()).into_owned())
                .collect(),
        }
    }

    fn is_empty(&self) -> bool {
        self.finders.is_empty()
    }

    fn matches_any(&self, content: &str) -> bool {
        let bytes = content.as_bytes();
        self.finders.iter().any(|f| f.find(bytes).is_some())
    }
}

fn build_identifier_file_index(files: &[(String, PathBuf)]) -> HashMap<String, Vec<usize>> {
    if files.is_empty() {
        return HashMap::new();
    }

    if files.len() <= 4 {
        let mut index = HashMap::new();
        for (idx, (_, path)) in files.iter().enumerate() {
            if let Ok(content) = std::fs::read(path) {
                add_file_identifiers(&mut index, idx, &content);
            }
        }
        return index;
    }

    let n_threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(files.len());
    let chunk_size = files.len().div_ceil(n_threads);

    let mut merged: HashMap<String, Vec<usize>> = HashMap::new();
    std::thread::scope(|s| {
        let mut handles = Vec::new();
        for (chunk_idx, chunk) in files.chunks(chunk_size).enumerate() {
            let start_idx = chunk_idx * chunk_size;
            handles.push(s.spawn(move || {
                let mut local = HashMap::new();
                for (offset, (_, path)) in chunk.iter().enumerate() {
                    if let Ok(content) = std::fs::read(path) {
                        add_file_identifiers(&mut local, start_idx + offset, &content);
                    }
                }
                local
            }));
        }

        for handle in handles {
            match handle.join() {
                Ok(local) => {
                    for (identifier, mut indices) in local {
                        merged.entry(identifier).or_default().append(&mut indices);
                    }
                }
                Err(_) => tracing::error!("failed to join reference identifier index thread"),
            }
        }
    });

    for indices in merged.values_mut() {
        indices.sort_unstable();
        indices.dedup();
    }

    merged
}

const REFERENCE_IDENTIFIER_CACHE_VERSION: u32 = 5;
const REFERENCE_IDENTIFIER_CACHE_MAGIC: &[u8; 8] = b"PHRIFID1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ReferenceIdentifierCachedFile {
    len: u64,
    modified_ns: u64,
}

fn load_reference_identifier_index_cache(
    root: &Path,
    files: &[(String, PathBuf)],
) -> Option<HashMap<String, Vec<usize>>> {
    let path = reference_identifier_cache_path(root)?;
    let bytes = std::fs::read(path).ok()?;
    decode_reference_identifier_index_cache(root, files, &bytes)
}

fn decode_reference_identifier_index_cache(
    root: &Path,
    files: &[(String, PathBuf)],
    bytes: &[u8],
) -> Option<HashMap<String, Vec<usize>>> {
    let mut reader = ReferenceIdentifierCacheReader::new(bytes);

    if reader.read_bytes(REFERENCE_IDENTIFIER_CACHE_MAGIC.len())?
        != REFERENCE_IDENTIFIER_CACHE_MAGIC.as_slice()
    {
        return None;
    }

    if reader.read_u32()? != REFERENCE_IDENTIFIER_CACHE_VERSION {
        return None;
    }

    if reader.read_string()? != root.to_string_lossy() {
        return None;
    }

    let file_count = usize::try_from(reader.read_u32()?).ok()?;
    if file_count != files.len() {
        return None;
    }

    let mut cached_files = Vec::with_capacity(file_count);
    for (_, path) in files {
        if reader.read_string()? != reference_cache_relative_path(root, path)? {
            return None;
        }
        cached_files.push(ReferenceIdentifierCachedFile {
            len: reader.read_u64()?,
            modified_ns: reader.read_u64()?,
        });
    }

    if !validate_reference_identifier_cached_files(files, &cached_files) {
        return None;
    }

    let entry_count = usize::try_from(reader.read_u32()?).ok()?;
    let mut index = HashMap::with_capacity(entry_count);
    for _ in 0..entry_count {
        let identifier = reader.read_string()?.into_owned();
        let file_count = usize::try_from(reader.read_u32()?).ok()?;
        let mut file_indices = Vec::with_capacity(file_count);
        for _ in 0..file_count {
            let file_idx = usize::try_from(reader.read_u32()?).ok()?;
            if file_idx >= files.len() {
                return None;
            }
            file_indices.push(file_idx);
        }
        index.insert(identifier, file_indices);
    }

    reader.is_finished().then_some(index)
}

fn save_reference_identifier_index_cache(
    root: &Path,
    files: &[(String, PathBuf)],
    index: &HashMap<String, Vec<usize>>,
) {
    let Some(path) = reference_identifier_cache_path(root) else {
        return;
    };

    let Some(parent) = path.parent() else {
        return;
    };

    let Some(bytes) = encode_reference_identifier_index_cache(root, files, index) else {
        return;
    };

    if std::fs::create_dir_all(parent).is_err() {
        return;
    }

    let tmp_path = path.with_extension("bin.tmp");
    if std::fs::write(&tmp_path, bytes).is_err() {
        return;
    }

    if std::fs::rename(&tmp_path, &path).is_err() {
        let _ = std::fs::remove_file(&tmp_path);
    }
}

fn spawn_save_reference_identifier_index_cache(
    root: PathBuf,
    files: Vec<(String, PathBuf)>,
    index: HashMap<String, Vec<usize>>,
) {
    std::thread::Builder::new()
        .name("phpantom-reference-cache-save".to_string())
        .spawn(move || {
            save_reference_identifier_index_cache(&root, &files, &index);
        })
        .ok();
}

fn encode_reference_identifier_index_cache(
    root: &Path,
    files: &[(String, PathBuf)],
    index: &HashMap<String, Vec<usize>>,
) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(REFERENCE_IDENTIFIER_CACHE_MAGIC);
    write_cache_u32(&mut bytes, REFERENCE_IDENTIFIER_CACHE_VERSION);
    write_cache_string(&mut bytes, root.to_string_lossy().as_ref())?;
    write_cache_u32(&mut bytes, u32::try_from(files.len()).ok()?);

    for (_, path) in files {
        let cached = reference_identifier_cached_file(path)?;
        write_cache_string(
            &mut bytes,
            reference_cache_relative_path(root, path)?.as_ref(),
        )?;
        write_cache_u64(&mut bytes, cached.len);
        write_cache_u64(&mut bytes, cached.modified_ns);
    }

    write_cache_u32(&mut bytes, u32::try_from(index.len()).ok()?);
    let mut entries: Vec<_> = index.iter().collect();
    entries.sort_unstable_by(|(left, _), (right, _)| left.cmp(right));
    for (identifier, file_indices) in entries {
        write_cache_string(&mut bytes, identifier)?;
        write_cache_u32(&mut bytes, u32::try_from(file_indices.len()).ok()?);
        for file_idx in file_indices {
            write_cache_u32(&mut bytes, u32::try_from(*file_idx).ok()?);
        }
    }

    Some(bytes)
}

fn write_cache_u32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn write_cache_u64(bytes: &mut Vec<u8>, value: u64) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn write_cache_string(bytes: &mut Vec<u8>, value: &str) -> Option<()> {
    write_cache_u32(bytes, u32::try_from(value.len()).ok()?);
    bytes.extend_from_slice(value.as_bytes());
    Some(())
}

struct ReferenceIdentifierCacheReader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> ReferenceIdentifierCacheReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn read_bytes(&mut self, len: usize) -> Option<&'a [u8]> {
        let end = self.offset.checked_add(len)?;
        let bytes = self.bytes.get(self.offset..end)?;
        self.offset = end;
        Some(bytes)
    }

    fn read_u32(&mut self) -> Option<u32> {
        let bytes = self.read_bytes(4)?;
        Some(u32::from_le_bytes(bytes.try_into().ok()?))
    }

    fn read_u64(&mut self) -> Option<u64> {
        let bytes = self.read_bytes(8)?;
        Some(u64::from_le_bytes(bytes.try_into().ok()?))
    }

    fn read_string(&mut self) -> Option<std::borrow::Cow<'a, str>> {
        let len = usize::try_from(self.read_u32()?).ok()?;
        let bytes = self.read_bytes(len)?;
        String::from_utf8_lossy(bytes).into()
    }

    fn is_finished(&self) -> bool {
        self.offset == self.bytes.len()
    }
}

fn reference_identifier_cache_path(root: &Path) -> Option<PathBuf> {
    let strategy = etcetera::choose_base_strategy().ok()?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    root.to_string_lossy().hash(&mut hasher);
    let root_hash = hasher.finish();
    Some(
        strategy
            .cache_dir()
            .join("phpantom_lsp")
            .join("reference-identifiers-v5")
            .join(format!("{root_hash:016x}.bin")),
    )
}

fn validate_reference_identifier_cached_files(
    files: &[(String, PathBuf)],
    cached_files: &[ReferenceIdentifierCachedFile],
) -> bool {
    if files.len() != cached_files.len() {
        return false;
    }

    if files.len() <= 128 {
        return files
            .iter()
            .zip(cached_files)
            .all(|((_, path), cached)| reference_identifier_cached_file(path) == Some(*cached));
    }

    let n_threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(files.len());
    let chunk_size = files.len().div_ceil(n_threads);

    std::thread::scope(|s| {
        let mut handles = Vec::new();
        for (file_chunk, cached_chunk) in files
            .chunks(chunk_size)
            .zip(cached_files.chunks(chunk_size))
        {
            handles.push(s.spawn(move || {
                file_chunk
                    .iter()
                    .zip(cached_chunk)
                    .all(|((_, path), cached)| {
                        reference_identifier_cached_file(path) == Some(*cached)
                    })
            }));
        }

        handles
            .into_iter()
            .all(|handle| handle.join().unwrap_or(false))
    })
}

fn reference_identifier_cached_file(path: &Path) -> Option<ReferenceIdentifierCachedFile> {
    let metadata = std::fs::metadata(path).ok()?;
    Some(ReferenceIdentifierCachedFile {
        len: metadata.len(),
        modified_ns: metadata
            .modified()
            .ok()
            .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
            .map(|duration| {
                duration
                    .as_secs()
                    .saturating_mul(1_000_000_000)
                    .saturating_add(u64::from(duration.subsec_nanos()))
            })?,
    })
}

fn reference_cache_relative_path<'a>(
    root: &Path,
    path: &'a Path,
) -> Option<std::borrow::Cow<'a, str>> {
    path.strip_prefix(root)
        .ok()
        .map(|path| path.to_string_lossy())
}

fn add_file_identifiers(index: &mut HashMap<String, Vec<usize>>, file_idx: usize, content: &[u8]) {
    for identifier in collect_file_identifiers(content) {
        index.entry(identifier).or_default().push(file_idx);
    }
}

fn collect_file_identifiers(content: &[u8]) -> HashSet<String> {
    let mut identifiers = HashSet::new();
    let mut i = 0;

    while i < content.len() {
        let byte = content[i];
        if is_identifier_start(byte) {
            let start = i;
            i += 1;
            while i < content.len() && is_identifier_continue(content[i]) {
                i += 1;
            }
            if let Ok(identifier) = std::str::from_utf8(&content[start..i]) {
                identifiers.insert(identifier.to_string());
            }
        } else {
            i += 1;
        }
    }

    identifiers
}

fn is_identifier_start(byte: u8) -> bool {
    byte == b'_' || byte.is_ascii_alphabetic() || byte >= 0x80
}

fn is_identifier_continue(byte: u8) -> bool {
    is_identifier_start(byte) || byte.is_ascii_digit()
}

fn line_starts(content: &str) -> Vec<usize> {
    let mut starts = vec![0];
    for (idx, byte) in content.bytes().enumerate() {
        if byte == b'\n' {
            starts.push(idx + 1);
        }
    }
    starts
}

fn offset_to_position_with_lines(content: &str, line_starts: &[usize], offset: u32) -> Position {
    let offset = (offset as usize).min(content.len());
    let line_idx = line_starts.partition_point(|start| *start <= offset) - 1;
    let line_start = line_starts[line_idx];
    let character = content[line_start..offset]
        .chars()
        .map(|ch| ch.len_utf16() as u32)
        .sum();

    Position {
        line: line_idx as u32,
        character,
    }
}

/// Check whether a resolved class name matches the target FQN.
///
/// Two names match if their fully-qualified forms are equal, or if both
/// are unqualified and their short names match.
fn class_names_match(resolved: &str, target: &str, target_short: &str) -> bool {
    if resolved == target {
        return true;
    }
    // When neither name is qualified, compare short names.
    if !resolved.contains('\\') && !target.contains('\\') {
        return resolved == target_short;
    }
    // When the resolved name is unqualified but the target is
    // namespace-qualified, the resolved name might be a short-name
    // reference to the target class (e.g. `Request` referencing
    // `Illuminate\Http\Request` via a `use` import that was not
    // tracked in the resolved-names map).  Accept the match only
    // when the short names agree.
    //
    // The reverse (resolved is qualified, target is unqualified) is
    // NOT accepted: `App\Helper` is a different class from a global
    // `Helper`, so matching by short name alone would produce false
    // positives.
    if !resolved.contains('\\') && target.contains('\\') {
        return resolved == target_short;
    }
    false
}

/// Push a location only if it is not already present (deduplication).
fn push_unique_location(locations: &mut Vec<Location>, uri: &Url, start: Position, end: Position) {
    let already_present = locations.iter().any(|l| {
        l.uri == *uri
            && l.range.start.line == start.line
            && l.range.start.character == start.character
    });
    if !already_present {
        locations.push(Location {
            uri: uri.clone(),
            range: Range { start, end },
        });
    }
}

#[cfg(test)]
mod tests;
