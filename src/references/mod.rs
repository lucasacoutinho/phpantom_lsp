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

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use tower_lsp::lsp_types::{Location, Position, Range, Url};

use crate::Backend;
use crate::completion::resolver::Loaders;
use crate::symbol_map::{SelfStaticParentKind, SymbolKind, SymbolMap};
use crate::types::{ClassInfo, MAX_INHERITANCE_DEPTH, ResolvedType};
use crate::util::{
    build_fqn, collect_php_files_gitignore, find_class_at_offset, offset_to_position,
    position_to_offset, strip_fqn_prefix,
};

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
        let t0 = std::time::Instant::now();

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
            let ms = t0.elapsed().as_secs_f64() * 1000.0;
            tracing::info!(
                "[find-refs] kind={:?} results={} total={ms:.1}ms thread={:?}",
                std::mem::discriminant(&sym.kind),
                locations.len(),
                std::thread::current().id(),
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
        tracing::info!(
            "[find-refs:dispatch] kind={:?} span_start={span_start} thread={:?}",
            std::mem::discriminant(kind),
            std::thread::current().id()
        );
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
                let fqn = build_fqn(name, &ctx.namespace);
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
                    SelfStaticParentKind::Parent => current_class
                        .and_then(|cc| cc.parent_class.as_ref())
                        .cloned(),
                    _ => current_class.map(|cc| build_fqn(&cc.name, &ctx.namespace)),
                };
                if let Some(fqn) = fqn {
                    self.find_class_references(&fqn, include_declaration)
                } else {
                    Vec::new()
                }
            }
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

    /// Find all references to a class/interface/trait/enum across all files.
    ///
    /// Matches `ClassReference` spans whose resolved FQN equals `target_fqn`,
    /// and optionally `ClassDeclaration` spans at the declaration site.
    fn find_class_references(&self, target_fqn: &str, include_declaration: bool) -> Vec<Location> {
        let mut locations = Vec::new();

        // Normalise: strip leading backslash if present.
        let target = strip_fqn_prefix(target_fqn);
        let target_short = crate::util::short_name(target);

        // Snapshot user-file symbol maps (excludes vendor and stubs).
        let snapshot = self.user_file_symbol_maps();

        for (file_uri, symbol_map) in &snapshot {
            // Quick pre-check: skip files that have no ClassReference,
            // ClassDeclaration, or SelfStaticParent spans whose short
            // name could match the target.  This avoids lock acquisitions
            // and (crucially) disk reads for the vast majority of files.
            let has_candidate = symbol_map.spans.iter().any(|s| match &s.kind {
                SymbolKind::ClassReference { name, .. }
                | SymbolKind::ClassDeclaration { name } => {
                    crate::util::short_name(name) == target_short
                }
                SymbolKind::SelfStaticParent(k) => *k != SelfStaticParentKind::This,
                _ => false,
            });
            if !has_candidate {
                continue;
            }

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

            // Defer file content loading: only read from disk when we
            // have a confirmed match that needs offset → position
            // conversion.  This avoids ~10k disk reads on remote mounts.
            let content_cell: std::cell::OnceCell<Option<Arc<String>>> =
                std::cell::OnceCell::new();
            let load_content = || -> Option<&Arc<String>> {
                content_cell
                    .get_or_init(|| self.get_file_content_arc(file_uri))
                    .as_ref()
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
                        let Some(content) = load_content() else {
                            break;
                        };
                        let start = offset_to_position(content, span.start as usize);
                        let end = offset_to_position(content, span.end as usize);
                        locations.push(Location {
                            uri: parsed_uri.clone(),
                            range: Range { start, end },
                        });
                    }
                    SymbolKind::ClassDeclaration { name } if include_declaration => {
                        let fqn = build_fqn(name, &file_namespace);
                        if !class_names_match(&fqn, target, target_short) {
                            continue;
                        }
                        let Some(content) = load_content() else {
                            break;
                        };
                        let start = offset_to_position(content, span.start as usize);
                        let end = offset_to_position(content, span.end as usize);
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
                            let Some(content) = load_content() else {
                                break;
                            };
                            let start = offset_to_position(content, span.start as usize);
                            let end = offset_to_position(content, span.end as usize);
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

        let snapshot = self.user_file_symbol_maps();
        let mut dbg_files_with_match = 0u32;
        let mut dbg_access_matches = 0u32;

        for (file_uri, symbol_map) in &snapshot {
            // Quick pre-check: skip files with no MemberAccess or
            // MemberDeclaration spans matching the target name.
            let has_candidate = symbol_map.spans.iter().any(|s| match &s.kind {
                SymbolKind::MemberAccess {
                    member_name,
                    is_static,
                    ..
                } => member_name == target_member && *is_static == target_is_static,
                SymbolKind::MemberDeclaration { name, is_static }
                    if include_declaration =>
                {
                    name == target_member && *is_static == target_is_static
                }
                _ => false,
            });
            if !has_candidate {
                continue;
            }
            dbg_files_with_match += 1;

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
                        dbg_access_matches += 1;
                        tracing::info!(
                            "[find-refs:access] file={file_uri} subject={subject_text:?}"
                        );
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
                            let in_hier = subject_fqns.iter().any(|fqn| hier.contains(fqn));
                            tracing::info!(
                                "[find-refs:access] resolved={subject_fqns:?} in_hierarchy={in_hier}"
                            );
                            if subject_fqns.is_empty() || !in_hier {
                                // Subject resolved to a class outside the
                                // hierarchy, or couldn't be resolved at all
                                // — skip.  Unresolvable subjects are more
                                // likely false positives than true matches
                                // when we have a known hierarchy.
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
                                let fqn = enclosing.fqn();
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

            // Property declarations use Variable spans (not MemberDeclaration)
            // because GTD relies on the Variable kind to jump to the type hint.
            // Scan the ast_map to pick up property declaration sites.
            if include_declaration && let Some(classes) = self.get_classes_for_uri(file_uri) {
                for class in &classes {
                    // Filter by hierarchy when available.
                    if let Some(hier) = hierarchy {
                        let class_fqn = class.fqn();
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

        tracing::info!(
            "[find-refs:members] target={target_member} snapshot={} files_with_match={dbg_files_with_match} access_matches={dbg_access_matches} results={}",
            snapshot.len(),
            locations.len()
        );

        locations.sort_by(|a, b| {
            a.uri
                .as_str()
                .cmp(b.uri.as_str())
                .then(a.range.start.line.cmp(&b.range.start.line))
                .then(a.range.start.character.cmp(&b.range.start.character))
        });

        locations
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

        let snapshot = self.user_file_symbol_maps();

        for (file_uri, symbol_map) in &snapshot {
            // Quick pre-check: skip files with no FunctionCall spans
            // whose short name could match the target.
            let has_candidate = symbol_map.spans.iter().any(|s| {
                matches!(&s.kind, SymbolKind::FunctionCall { name, .. }
                    if crate::util::short_name(name) == target_short)
            });
            if !has_candidate {
                continue;
            }

            // Prefer mago-names resolved_names; lazy-load use_map only
            // when an offset is not tracked (e.g. docblock references).
            let resolved_names = self.resolved_names.read().get(file_uri).cloned();
            let file_namespace = self.namespace_map.read().get(file_uri).cloned().flatten();
            let file_use_map = std::cell::OnceCell::new();

            let parsed_uri = match Url::parse(file_uri) {
                Ok(u) => u,
                Err(_) => continue,
            };

            let content_cell: std::cell::OnceCell<Option<Arc<String>>> =
                std::cell::OnceCell::new();
            let load_content = || -> Option<&Arc<String>> {
                content_cell
                    .get_or_init(|| self.get_file_content_arc(file_uri))
                    .as_ref()
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
                    let Some(content) = load_content() else {
                        break;
                    };
                    let start = offset_to_position(content, span.start as usize);
                    let end = offset_to_position(content, span.end as usize);
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

        let snapshot = self.user_file_symbol_maps();

        for (file_uri, symbol_map) in &snapshot {
            // Quick pre-check: skip files with no matching constant or
            // member-declaration spans.
            let has_candidate = symbol_map.spans.iter().any(|s| match &s.kind {
                SymbolKind::ConstantReference { name } => name == target_name,
                SymbolKind::MemberDeclaration { name, is_static } if include_declaration => {
                    name == target_name && *is_static
                }
                _ => false,
            });
            if !has_candidate {
                continue;
            }

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
            SelfStaticParentKind::Parent => current_class.parent_class.clone(),
            _ => {
                // self / static → current class FQN
                Some(build_fqn(&current_class.name, namespace))
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
            tracing::info!(
                "[find-refs:hierarchy] MemberAccess subject={subject_text:?} could not be resolved"
            );
            return None;
        }
        let hierarchy = self.collect_hierarchy_for_fqns(&fqns);
        tracing::info!(
            "[find-refs:hierarchy] MemberAccess subject={subject_text:?} resolved to {fqns:?}, hierarchy_size={}",
            hierarchy.len()
        );
        Some(hierarchy)
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
        if classes.is_empty() {
            tracing::info!(
                "[find-refs:hierarchy] no classes in ast_map for uri={uri}"
            );
            return None;
        }
        let current_class = find_class_at_offset(&classes, offset);
        if current_class.is_none() {
            tracing::info!(
                "[find-refs:hierarchy] no class at offset={offset} in uri={uri}, classes: {}",
                classes
                    .iter()
                    .map(|c| format!("{}[{}-{}]", c.name, c.start_offset, c.end_offset))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            return None;
        }
        let fqn = current_class.unwrap().fqn();
        let hierarchy = self.collect_hierarchy_for_fqns(&[fqn.clone()]);
        tracing::info!(
            "[find-refs:hierarchy] resolved {fqn}, hierarchy_size={} thread={:?}",
            hierarchy.len(),
            std::thread::current().id()
        );
        Some(hierarchy)
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
            _ if trimmed.starts_with('$') && trimmed.contains("->") => {
                // Chained property access: `$this->prop`, `$this->a->b`, etc.
                // Resolve step by step through the property chain.
                let result =
                    self.resolve_chained_access_to_fqns(trimmed, ctx, access_offset, content);
                if result.is_empty() {
                    tracing::info!("[find-refs:chain] FAILED for subject={trimmed:?}");
                }
                result
            }
            _ if trimmed.starts_with('$') => {
                // Simple variable — try variable type resolution.
                self.resolve_variable_to_fqns(trimmed, ctx, access_offset, content)
            }
            _ => Vec::new(),
        }
    }

    /// Resolve a chained property access like `$this->prop` or
    /// `$this->a->b` to the class FQN(s) of the final property's type.
    ///
    /// Splits the chain on `->`, resolves the root (`$this`, `$var`),
    /// then walks each property segment by looking up the property's
    /// type hint on the resolved class.
    fn resolve_chained_access_to_fqns(
        &self,
        subject: &str,
        ctx: &crate::types::FileContext,
        access_offset: u32,
        content: &str,
    ) -> Vec<String> {
        let segments: Vec<&str> = subject.splitn(10, "->").collect();
        if segments.len() < 2 {
            return Vec::new();
        }

        let root = segments[0].trim();

        // Resolve the root to class FQN(s).
        let mut current_fqns: Vec<String> = match root {
            "$this" | "self" | "static" => self
                .find_enclosing_class_fqn(&ctx.classes, &ctx.namespace, access_offset)
                .into_iter()
                .collect(),
            _ if root.starts_with('$') => {
                self.resolve_variable_to_fqns(root, ctx, access_offset, content)
            }
            _ => return Vec::new(),
        };

        if current_fqns.is_empty() {
            tracing::info!("[find-refs:chain] root={root:?} could not be resolved");
            return Vec::new();
        }

        // Walk each property segment.
        for &prop_name in &segments[1..] {
            let prop = prop_name.trim();
            if prop.is_empty() {
                return Vec::new();
            }

            let mut next_fqns = Vec::new();
            for fqn in &current_fqns {
                if let Some(class) = self.find_or_load_class(fqn) {
                    if let Some(prop_type) = self.find_property_type(&class, prop) {
                        let resolved = prop_type.base_name().map(|name| {
                            // If the type name already contains a namespace
                            // separator it is already fully qualified (stored
                            // that way during parsing) — use it directly.
                            // Otherwise resolve the short name via the
                            // scanned file's use_map / namespace.
                            if name.contains('\\') {
                                normalize_fqn(name)
                            } else {
                                let resolved_name = ctx.resolve_name_at(name, access_offset);
                                normalize_fqn(&resolved_name)
                            }
                        });
                        if let Some(resolved_fqn) = resolved {
                            if !next_fqns.contains(&resolved_fqn) {
                                next_fqns.push(resolved_fqn);
                            }
                        } else {
                            tracing::info!(
                                "[find-refs:chain] property {prop} on {fqn} has type {prop_type} but base_name() returned None"
                            );
                        }
                    } else {
                        tracing::info!(
                            "[find-refs:chain] property {prop} not found on {fqn} (properties: {:?})",
                            class.properties.iter().map(|p| &p.name).collect::<Vec<_>>()
                        );
                    }
                } else {
                    tracing::info!("[find-refs:chain] class {fqn} not found");
                }
            }

            if next_fqns.is_empty() {
                return Vec::new();
            }
            current_fqns = next_fqns;
        }

        current_fqns
    }

    /// Find the type hint of a property on a class, walking up the
    /// inheritance chain (parent class, traits) if needed.
    fn find_property_type(
        &self,
        class: &ClassInfo,
        property_name: &str,
    ) -> Option<crate::php_type::PhpType> {
        // Check own properties.
        for prop in &class.properties {
            if prop.name == property_name {
                return prop.type_hint.clone();
            }
        }

        // Walk parent class.
        if let Some(ref parent) = class.parent_class {
            if let Some(parent_class) = self.find_or_load_class(parent) {
                if let Some(ty) = self.find_property_type(&parent_class, property_name) {
                    return Some(ty);
                }
            }
        }

        // Walk traits.
        for trait_name in &class.used_traits {
            if let Some(trait_class) = self.find_or_load_class(trait_name) {
                if let Some(ty) = self.find_property_type(&trait_class, property_name) {
                    return Some(ty);
                }
            }
        }

        None
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
        let fqn = build_fqn(&cc.name, namespace);
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
        let t0 = std::time::Instant::now();

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

        let phase1_count = phase1_uris.len();
        self.parse_files_parallel(
            phase1_uris
                .iter()
                .map(|uri| (uri.as_str(), None::<&str>))
                .collect(),
        );
        let phase1_ms = t0.elapsed().as_secs_f64() * 1000.0;

        // ── Phase 2: workspace directory scan ───────────────────────────
        // Recursively discover PHP files in the workspace root that are
        // not yet indexed.  This catches files that are not in the
        // classmap, class_index, or already opened.  The vendor directory
        // is skipped — find references only reports user code.  The walk
        // respects .gitignore so that generated/cached directories (e.g.
        // storage/framework/views/, var/cache/, node_modules/) are
        // automatically excluded.
        //
        // ── Phase 2: workspace directory scan ───────────────────────────
        // Recursively discover PHP files in the workspace root that are
        // not yet indexed.  Guarded by a tri-state CAS so only one
        // thread (background indexer or first find-references call) runs
        // the expensive filesystem walk.
        //
        // States: 0 = NOT_STARTED, 1 = IN_PROGRESS, 2 = COMPLETE.
        const NOT_STARTED: u8 = 0;
        const IN_PROGRESS: u8 = 1;
        const COMPLETE: u8 = 2;

        let prev = self.workspace_scan_state.compare_exchange(
            NOT_STARTED,
            IN_PROGRESS,
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
        );

        match prev {
            Ok(_) => {
                // We won the race — run the walk + parse.
                let workspace_root = self.workspace_root.read().clone();

                if let Some(root) = workspace_root {
                    let vendor_dir_paths = self.vendor_dir_paths.lock().clone();

                    // Re-read existing URIs after phase 1 may have added more.
                    let existing_uris: HashSet<String> =
                        self.symbol_maps.read().keys().cloned().collect();

                    let t_walk = std::time::Instant::now();
                    let php_files = collect_php_files_gitignore(&root, &vendor_dir_paths);
                    let walk_ms = t_walk.elapsed().as_secs_f64() * 1000.0;
                    let walk_count = php_files.len();

                    let phase2_work: Vec<(String, PathBuf)> = php_files
                        .into_iter()
                        .filter_map(|path| {
                            let uri = crate::util::path_to_uri(&path);
                            if existing_uris.contains(&uri) {
                                None
                            } else {
                                Some((uri, path))
                            }
                        })
                        .collect();

                    let phase2_count = phase2_work.len();
                    self.parse_paths_parallel(&phase2_work);

                    self.workspace_scan_state
                        .store(COMPLETE, std::sync::atomic::Ordering::Release);

                    let total_ms = t0.elapsed().as_secs_f64() * 1000.0;
                    tracing::info!(
                        "[find-refs:index] phase1={phase1_count} ({phase1_ms:.1}ms) walk={walk_count} ({walk_ms:.1}ms) phase2_parse={phase2_count} total={total_ms:.1}ms"
                    );
                } else {
                    self.workspace_scan_state
                        .store(COMPLETE, std::sync::atomic::Ordering::Release);
                }
            }
            Err(IN_PROGRESS) => {
                // Background thread is running the walk — skip Phase 2.
                let total_ms = t0.elapsed().as_secs_f64() * 1000.0;
                tracing::info!(
                    "[find-refs:index] phase1={phase1_count} ({phase1_ms:.1}ms) phase2=bg_in_progress total={total_ms:.1}ms"
                );
            }
            Err(_) => {
                // COMPLETE — skip Phase 2.
                let total_ms = t0.elapsed().as_secs_f64() * 1000.0;
                if phase1_count > 0 {
                    tracing::info!(
                        "[find-refs:index] phase1={phase1_count} ({phase1_ms:.1}ms) phase2=skipped total={total_ms:.1}ms"
                    );
                }
            }
        }
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
}

/// Normalise a class FQN: strip leading `\` if present.
fn normalize_fqn(fqn: &str) -> String {
    strip_fqn_prefix(fqn).to_string()
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
