//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    build_histogram, build_mcv, distinct_count, Arc, BTreeMap, CatalogFacade, CatalogIndexRow,
    ColumnStatsInput, DocId, DocumentStore, Engine, Ordering, RelationIdentity, SQLError,
    StorageBackendError, StorageBackendResult, TableState, Value, ViewRow,
};
use uqa_execution::ScalarExpr;
use uqa_planner::{QueryPlan, RelationalPlan, SourcePlan};

type AnalyzeValues = BTreeMap<String, Vec<Value>>;
type AnalyzeNullCounts = BTreeMap<String, u64>;

fn canonical_virtual_relation_reference(reference: &str) -> Option<String> {
    let (schema, relation) = RelationIdentity::parse_reference(reference).ok()?;
    let relation = relation.to_ascii_lowercase();
    let schema = schema.map(|schema| schema.to_ascii_lowercase());
    let information_schema = matches!(
        relation.as_str(),
        "schemata"
            | "tables"
            | "columns"
            | "views"
            | "routines"
            | "sequences"
            | "table_constraints"
            | "key_column_usage"
    );
    let pg_catalog = matches!(
        relation.as_str(),
        "pg_namespace"
            | "pg_class"
            | "pg_attribute"
            | "pg_attrdef"
            | "pg_constraint"
            | "pg_index"
            | "pg_tables"
            | "pg_views"
            | "pg_indexes"
            | "pg_type"
            | "pg_proc"
            | "pg_database"
            | "pg_roles"
            | "pg_user"
            | "pg_settings"
            | "pg_description"
            | "pg_matviews"
            | "pg_sequences"
    );
    match schema.as_deref() {
        Some("information_schema") if information_schema => {
            Some(format!("information_schema.{relation}"))
        }
        Some("pg_catalog") | None if pg_catalog => Some(format!("pg_catalog.{relation}")),
        _ => None,
    }
}

fn sequence_function_reference_mut(expression: &mut ScalarExpr) -> Option<&mut String> {
    let ScalarExpr::Func { name, args, .. } = expression else {
        return None;
    };
    let lower = name.to_ascii_lowercase();
    let local = lower.strip_prefix("pg_catalog.").unwrap_or(&lower);
    if !matches!(local, "nextval" | "currval" | "setval")
        || (lower.contains('.') && !lower.starts_with("pg_catalog."))
    {
        return None;
    }
    regclass_literal_mut(args.first_mut()?)
}

fn regclass_literal_mut(expression: &mut ScalarExpr) -> Option<&mut String> {
    match expression {
        ScalarExpr::Literal(Value::Str(reference)) => Some(reference),
        ScalarExpr::Cast { expr, ty }
            if ty
                .rsplit_once('.')
                .map_or(ty.as_str(), |(_, local)| local)
                .eq_ignore_ascii_case("regclass") =>
        {
            regclass_literal_mut(expr)
        }
        _ => None,
    }
}

fn bind_query_plan_sequence_references<E>(
    plan: &mut QueryPlan,
    resolve: &mut impl FnMut(&str) -> Result<String, E>,
) -> Result<(), E> {
    let mut error = None;
    plan.rewrite_scalar_expressions(&mut |expression| {
        if error.is_some() {
            return;
        }
        let Some(reference) = sequence_function_reference_mut(expression) else {
            return;
        };
        match resolve(reference) {
            Ok(canonical) => *reference = canonical,
            Err(binding_error) => error = Some(binding_error),
        }
    });
    error.map_or(Ok(()), Err)
}

fn bind_query_plan_relations<E>(
    plan: &mut QueryPlan,
    inherited_ctes: &std::collections::BTreeSet<String>,
    resolve: &mut impl FnMut(&str) -> Result<String, E>,
) -> Result<(), E> {
    // Non-recursive CTEs see outer and preceding CTEs. A recursive CTE also
    // sees its own name while its body is bound. This mirrors materialization
    // order and prevents a real table with the same local name from being
    // mistaken for a CTE (or vice versa).
    let mut visible_ctes = inherited_ctes.clone();
    for cte in &mut plan.ctes {
        let mut body_ctes = visible_ctes.clone();
        if cte.recursive {
            body_ctes.insert(cte.name.clone());
        }
        bind_query_plan_relations(&mut cte.query, &body_ctes, resolve)?;
        visible_ctes.insert(cte.name.clone());
    }
    bind_relational_plan_relations(&mut plan.root, &visible_ctes, resolve)
}

fn bind_relational_plan_relations<E>(
    plan: &mut RelationalPlan,
    visible_ctes: &std::collections::BTreeSet<String>,
    resolve: &mut impl FnMut(&str) -> Result<String, E>,
) -> Result<(), E> {
    match plan {
        RelationalPlan::QueryBlock(block) => {
            if let Some(source) = &mut block.from {
                bind_source_plan_relations(source, visible_ctes, resolve)?;
            }
            for subquery in &mut block.subqueries {
                bind_query_plan_relations(subquery, visible_ctes, resolve)?;
            }
        }
        RelationalPlan::SetOp {
            left,
            right,
            subqueries,
            ..
        } => {
            bind_query_plan_relations(left, visible_ctes, resolve)?;
            bind_query_plan_relations(right, visible_ctes, resolve)?;
            for subquery in subqueries {
                bind_query_plan_relations(subquery, visible_ctes, resolve)?;
            }
        }
        RelationalPlan::Values { subqueries, .. } => {
            for subquery in subqueries {
                bind_query_plan_relations(subquery, visible_ctes, resolve)?;
            }
        }
    }
    Ok(())
}

fn bind_source_plan_relations<E>(
    source: &mut SourcePlan,
    visible_ctes: &std::collections::BTreeSet<String>,
    resolve: &mut impl FnMut(&str) -> Result<String, E>,
) -> Result<(), E> {
    match source {
        SourcePlan::Table { name, .. } => {
            let is_cte =
                RelationIdentity::parse_reference(name)
                    .ok()
                    .is_some_and(|(schema, relation)| {
                        schema.is_none() && visible_ctes.contains(&relation)
                    });
            if !is_cte {
                *name = resolve(name)?;
            }
        }
        SourcePlan::Join { left, right, .. } => {
            bind_source_plan_relations(left, visible_ctes, resolve)?;
            bind_source_plan_relations(right, visible_ctes, resolve)?;
        }
        SourcePlan::Subquery { body, .. } => {
            bind_query_plan_relations(body, visible_ctes, resolve)?;
        }
        SourcePlan::Values { .. } | SourcePlan::Function { .. } => {}
    }
    Ok(())
}

fn relation_reference_matches(reference: &str, target: &RelationIdentity) -> bool {
    match RelationIdentity::parse_reference(reference) {
        Ok((Some(schema), name)) => schema == target.schema && name == target.name,
        Ok((None, name)) => name == target.name,
        // A malformed stored plan must fail closed: treating it as unrelated
        // could permit DDL to leave an unexecutable view behind.
        Err(_) => true,
    }
}

fn source_plan_references_relation(
    source: &uqa_planner::SourcePlan,
    target: &RelationIdentity,
    ctes: &std::collections::BTreeSet<String>,
) -> bool {
    match source {
        uqa_planner::SourcePlan::Table { name, .. } => {
            let is_cte = RelationIdentity::parse_reference(name)
                .ok()
                .is_some_and(|(schema, relation)| schema.is_none() && ctes.contains(&relation));
            !is_cte && relation_reference_matches(name, target)
        }
        uqa_planner::SourcePlan::Join { left, right, .. } => {
            source_plan_references_relation(left, target, ctes)
                || source_plan_references_relation(right, target, ctes)
        }
        uqa_planner::SourcePlan::Subquery { body, .. } => {
            query_plan_references_relation(body, target, ctes)
        }
        uqa_planner::SourcePlan::Values { .. } | uqa_planner::SourcePlan::Function { .. } => false,
    }
}

fn query_plan_references_relation(
    query: &uqa_planner::QueryPlan,
    target: &RelationIdentity,
    inherited_ctes: &std::collections::BTreeSet<String>,
) -> bool {
    let mut ctes = inherited_ctes.clone();
    ctes.extend(query.ctes.iter().map(|cte| cte.name.clone()));
    if query
        .ctes
        .iter()
        .any(|cte| query_plan_references_relation(&cte.query, target, &ctes))
    {
        return true;
    }
    match &query.root {
        uqa_planner::RelationalPlan::QueryBlock(block) => {
            block
                .from
                .as_ref()
                .is_some_and(|source| source_plan_references_relation(source, target, &ctes))
                || block
                    .subqueries
                    .iter()
                    .any(|query| query_plan_references_relation(query, target, &ctes))
        }
        uqa_planner::RelationalPlan::SetOp {
            left,
            right,
            subqueries,
            ..
        } => {
            query_plan_references_relation(left, target, &ctes)
                || query_plan_references_relation(right, target, &ctes)
                || subqueries
                    .iter()
                    .any(|query| query_plan_references_relation(query, target, &ctes))
        }
        uqa_planner::RelationalPlan::Values { subqueries, .. } => subqueries
            .iter()
            .any(|query| query_plan_references_relation(query, target, &ctes)),
    }
}

fn query_plan_references_sequence(plan: &QueryPlan, target: &RelationIdentity) -> bool {
    let mut plan = plan.clone();
    let mut referenced = false;
    plan.rewrite_scalar_expressions(&mut |expression| {
        if let Some(reference) = sequence_function_reference_mut(expression) {
            referenced |= relation_reference_matches(reference, target);
        }
    });
    referenced
}

fn increment_analyze_null(
    counts: &mut AnalyzeNullCounts,
    column: &str,
) -> StorageBackendResult<()> {
    let count = counts.get_mut(column).ok_or_else(|| {
        StorageBackendError::Other(format!(
            "ANALYZE lost the null counter for column `{column}`"
        ))
    })?;
    *count = count
        .checked_add(1)
        .ok_or_else(|| StorageBackendError::Other("ANALYZE null count overflow".into()))?;
    Ok(())
}

fn collect_analyze_values(
    snapshot: &dyn DocumentStore,
    doc_ids: &[DocId],
    columns: &[String],
) -> StorageBackendResult<(AnalyzeValues, AnalyzeNullCounts)> {
    let mut values = AnalyzeValues::new();
    let mut nulls = AnalyzeNullCounts::new();
    for column in columns {
        values.insert(column.clone(), Vec::new());
        nulls.insert(column.clone(), 0);
    }
    for doc_id in doc_ids {
        let Some(document) = snapshot.get(*doc_id)? else {
            for column in columns {
                increment_analyze_null(&mut nulls, column)?;
            }
            continue;
        };
        for column in columns {
            match document.get(column) {
                None | Some(Value::Null) => increment_analyze_null(&mut nulls, column)?,
                Some(value) => values
                    .get_mut(column)
                    .ok_or_else(|| {
                        StorageBackendError::Other(format!(
                            "ANALYZE lost the value buffer for column `{column}`"
                        ))
                    })?
                    .push(value.clone()),
            }
        }
    }
    Ok((values, nulls))
}

fn parse_search_path_list(value: &str) -> Result<Vec<String>, SQLError> {
    let chars = value.chars().collect::<Vec<_>>();
    let mut index = 0;
    let mut schemas = Vec::new();
    while index < chars.len() {
        while index < chars.len() && chars[index].is_whitespace() {
            index += 1;
        }
        if index == chars.len() {
            break;
        }
        let mut schema = String::new();
        if matches!(chars[index], '"' | '\'') {
            let quote = chars[index];
            index += 1;
            let mut terminated = false;
            while index < chars.len() {
                if chars[index] != quote {
                    schema.push(chars[index]);
                    index += 1;
                } else if chars.get(index + 1) == Some(&quote) {
                    schema.push(quote);
                    index += 2;
                } else {
                    index += 1;
                    terminated = true;
                    break;
                }
            }
            if !terminated {
                return Err(SQLError::TypeMismatch(format!(
                    "unterminated quoted schema in search_path `{value}`"
                )));
            }
        } else {
            while index < chars.len() && chars[index] != ',' {
                schema.push(chars[index]);
                index += 1;
            }
            schema = schema.trim().to_string();
        }
        while index < chars.len() && chars[index].is_whitespace() {
            index += 1;
        }
        if schema.is_empty() || (index < chars.len() && chars[index] != ',') {
            return Err(SQLError::TypeMismatch(format!(
                "invalid schema list in search_path `{value}`"
            )));
        }
        schemas.push(schema);
        if index < chars.len() {
            index += 1;
            if index == chars.len() {
                return Err(SQLError::TypeMismatch(format!(
                    "trailing comma in search_path `{value}`"
                )));
            }
        }
    }
    Ok(schemas)
}

impl Engine {
    fn bind_view_plan_for_create(&self, plan: &mut QueryPlan) -> Result<(), SQLError> {
        bind_query_plan_relations(plan, &std::collections::BTreeSet::new(), &mut |reference| {
            // Catalog relations win for their supported spellings just
            // as they do in FROM execution (notably unqualified
            // `pg_class`). Explicit user schemas remain ordinary catalog
            // identities.
            if let Some(canonical) = canonical_virtual_relation_reference(reference) {
                return Ok(canonical);
            }
            match self.try_resolve_relation_kind(reference).map_err(|error| {
                SQLError::Internal(format!("resolve CREATE VIEW source `{reference}`: {error}"))
            })? {
                Some((canonical, "table" | "view" | "foreign table")) => Ok(canonical),
                Some((canonical, kind)) => Err(SQLError::Unsupported(format!(
                    "CREATE VIEW source `{canonical}` is a {kind}, not a row relation"
                ))),
                None => Err(SQLError::Unsupported(format!(
                    "CREATE VIEW source relation `{reference}` does not exist"
                ))),
            }
        })?;
        bind_query_plan_sequence_references(plan, &mut |reference| {
            self.resolve_sequence_reference_for_binding(reference)
                .map_err(|error| {
                    SQLError::Unsupported(format!(
                        "CREATE VIEW sequence reference `{reference}`: {error}"
                    ))
                })
        })
    }

    fn bind_stored_view_plan(
        &self,
        plan: &mut QueryPlan,
        relations: &std::collections::BTreeSet<RelationIdentity>,
    ) -> StorageBackendResult<()> {
        bind_query_plan_relations(plan, &std::collections::BTreeSet::new(), &mut |reference| {
            if let Some(canonical) = canonical_virtual_relation_reference(reference) {
                return Ok(canonical);
            }
            let (schema, local_name) =
                RelationIdentity::parse_reference(reference).map_err(|error| {
                    StorageBackendError::Other(format!(
                        "invalid stored view source `{reference}`: {error}"
                    ))
                })?;
            if let Some(schema) = schema {
                let candidate = RelationIdentity::new(schema, local_name);
                if relations.contains(&candidate) {
                    return Ok(candidate.qualified_name());
                }
            } else {
                let candidates = relations
                    .iter()
                    .filter(|candidate| candidate.name == local_name)
                    .map(RelationIdentity::qualified_name)
                    .collect::<Vec<_>>();
                match candidates.as_slice() {
                    [candidate] => return Ok(candidate.clone()),
                    [] => {}
                    _ => {
                        return Err(StorageBackendError::Other(format!(
                            "ambiguous stored view source `{reference}` matches {}",
                            candidates.join(", ")
                        )));
                    }
                }
            }
            Err(StorageBackendError::Other(format!(
                "stored view source relation `{reference}` does not exist"
            )))
        })?;
        bind_query_plan_sequence_references(plan, &mut |reference| {
            self.resolve_stored_sequence_reference(reference)
        })
    }

    pub fn register_view(
        &self,
        name: &str,
        body: uqa_sql::ast::SelectStmt,
    ) -> Result<(), SQLError> {
        self.with_implicit_transaction(move |engine| {
            let plan = uqa_planner::UnifiedPlan::Query(Box::new(
                uqa_planner::QueryPlan::lower_with(body, &|aggregate: &str| {
                    engine.has_registered_aggregate_function(aggregate)
                }),
            ));
            let plan = crate::sql::optimize_engine_plan(engine, plan)?;
            let uqa_planner::UnifiedPlan::Query(plan) = plan else {
                return Err(SQLError::Internal(
                    "view lowering produced a non-query plan".into(),
                ));
            };
            engine.register_view_plan_inner(name, *plan)
        })
    }

    pub(crate) fn register_view_plan(
        &self,
        name: &str,
        plan: uqa_planner::QueryPlan,
    ) -> Result<(), SQLError> {
        self.with_implicit_transaction(move |engine| engine.register_view_plan_inner(name, plan))
    }

    fn register_view_plan_inner(
        &self,
        name: &str,
        mut plan: uqa_planner::QueryPlan,
    ) -> Result<(), SQLError> {
        self.synchronize_catalog_registries()
            .map_err(|err| SQLError::Internal(format!("refresh view catalog: {err}")))?;
        let name = self
            .try_relation_name_for_create(name)
            .map_err(SQLError::Unsupported)?;
        let relation = RelationIdentity::from_legacy_name(&name)
            .map_err(|err| SQLError::Internal(format!("invalid canonical view name: {err}")))?;
        if let Some(kind) = self
            .relation_kind_at(&name)
            .map_err(|err| SQLError::Internal(format!("resolve relation `{name}`: {err}")))?
        {
            if kind != "view" {
                return Err(SQLError::Unsupported(format!(
                    "relation `{name}` already exists as {kind}"
                )));
            }
        }
        self.bind_view_plan_for_create(&mut plan)?;
        let mut views = self.views.write();
        if let Some(catalog) = self.catalog.as_ref() {
            let definition_json = serde_json::to_string(&plan)
                .map_err(|err| SQLError::Internal(format!("serialize view `{name}`: {err}")))?;
            catalog
                .save_view(&ViewRow {
                    relation: relation.clone(),
                    definition_json,
                })
                .map_err(|err| SQLError::Internal(format!("persist view `{name}`: {err}")))?;
        }
        views.insert(relation, plan);
        drop(views);
        self.note_catalog_registry_changed();
        Ok(())
    }

    pub fn drop_view(&self, name: &str) -> Result<bool, SQLError> {
        self.with_implicit_transaction(|engine| {
            match engine
                .try_resolve_relation_kind(name)
                .map_err(|err| SQLError::Internal(format!("refresh view catalog: {err}")))?
            {
                Some((canonical, "view")) => {
                    engine.drop_views_inner(&[canonical])?;
                    Ok(true)
                }
                Some((canonical, kind)) => Err(SQLError::Unsupported(format!(
                    "DROP VIEW: relation `{canonical}` is a {kind}, not a view"
                ))),
                None => Ok(false),
            }
        })
    }

    pub(crate) fn drop_views(&self, names: &[String]) -> Result<(), SQLError> {
        self.with_implicit_transaction(|engine| engine.drop_views_inner(names))
    }

    fn drop_views_inner(&self, names: &[String]) -> Result<(), SQLError> {
        let drop_set = names
            .iter()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        for name in names {
            let dependents = self
                .views_depending_on_relation(name)
                .map_err(|err| SQLError::Internal(format!("inspect view dependencies: {err}")))?
                .into_iter()
                .filter(|dependent| !drop_set.contains(dependent))
                .collect::<Vec<_>>();
            if !dependents.is_empty() {
                return Err(SQLError::Unsupported(format!(
                    "DROP VIEW `{name}` rejected: dependent view(s) `{}` still reference it",
                    dependents.join("`, `")
                )));
            }
        }
        for name in names {
            self.drop_view_state_inner(name)?;
        }
        Ok(())
    }

    fn drop_view_state_inner(&self, name: &str) -> Result<(), SQLError> {
        let relation = RelationIdentity::from_legacy_name(name)
            .map_err(|err| SQLError::Internal(format!("invalid canonical view name: {err}")))?;
        let mut views = self.views.write();
        let removed = if let Some(catalog) = self.catalog.as_ref() {
            catalog
                .drop_view(&relation)
                .map_err(|err| SQLError::Internal(format!("drop view `{name}`: {err}")))?
        } else {
            views.contains_key(&relation)
        };
        if removed {
            views.remove(&relation);
        }
        drop(views);
        if removed {
            self.note_catalog_registry_changed();
        }
        if removed {
            Ok(())
        } else {
            Err(SQLError::Internal(format!(
                "view `{name}` disappeared after dependency preflight"
            )))
        }
    }

    pub fn view(&self, name: &str) -> Result<Option<uqa_planner::QueryPlan>, SQLError> {
        let Some(resolved) = self
            .try_resolve_view_name(name)
            .map_err(|err| SQLError::Internal(format!("refresh view catalog: {err}")))?
        else {
            return Ok(None);
        };
        let relation = Self::resolved_relation_identity(&resolved)
            .map_err(|err| SQLError::Internal(format!("resolve view `{resolved}`: {err}")))?;
        Ok(self.views.read().get(&relation).cloned())
    }

    pub(crate) fn view_plan(&self, name: &str) -> Result<Option<uqa_planner::QueryPlan>, SQLError> {
        self.view(name)
    }

    pub fn list_views(&self) -> Result<Vec<String>, SQLError> {
        self.synchronize_catalog_registries()
            .map_err(|err| SQLError::Internal(format!("refresh view catalog: {err}")))?;
        let mut out: Vec<String> = self
            .views
            .read()
            .keys()
            .map(RelationIdentity::qualified_name)
            .collect();
        out.sort_unstable();
        Ok(out)
    }

    /// Return stored views whose plan is bound to `canonical_name`.
    ///
    /// New definitions persist canonical source identities. Legacy plans are
    /// canonicalized during restore only when an unqualified name has exactly
    /// one catalog candidate, so normal dependency checks are exact. The
    /// matcher remains conservative for malformed in-memory plans and fails
    /// closed rather than permitting dangling DDL.
    pub(crate) fn views_depending_on_relation(
        &self,
        canonical_name: &str,
    ) -> StorageBackendResult<Vec<String>> {
        self.synchronize_catalog_registries()?;
        let target = RelationIdentity::from_legacy_name(canonical_name)
            .map_err(StorageBackendError::Other)?;
        let empty_ctes = std::collections::BTreeSet::new();
        let mut dependents = self
            .views
            .read()
            .iter()
            .filter(|(relation, plan)| {
                *relation != &target && query_plan_references_relation(plan, &target, &empty_ctes)
            })
            .map(|(relation, _)| relation.qualified_name())
            .collect::<Vec<_>>();
        dependents.sort_unstable();
        Ok(dependents)
    }

    /// Return stored views with a literal `nextval`, `currval`, or `setval`
    /// dependency on the canonical sequence name.
    pub(crate) fn views_depending_on_sequence(
        &self,
        canonical_name: &str,
    ) -> StorageBackendResult<Vec<String>> {
        self.synchronize_catalog_registries()?;
        let target = RelationIdentity::from_legacy_name(canonical_name)
            .map_err(StorageBackendError::Other)?;
        let mut dependents = self
            .views
            .read()
            .iter()
            .filter(|(_, plan)| query_plan_references_sequence(plan, &target))
            .map(|(relation, _)| relation.qualified_name())
            .collect::<Vec<_>>();
        dependents.sort_unstable();
        Ok(dependents)
    }

    pub(crate) fn restore_views_from_catalog(
        &self,
        catalog: &dyn CatalogFacade,
    ) -> StorageBackendResult<()> {
        let rows = catalog.load_views()?;
        let mut relations = self
            .tables
            .read()
            .keys()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        relations.extend(self.foreign_tables.read().keys().cloned());
        relations.extend(rows.iter().map(|row| row.relation.clone()));

        let mut views = BTreeMap::new();
        for row in rows {
            let view_name = row.relation.qualified_name();
            let mut plan = serde_json::from_str::<uqa_planner::QueryPlan>(&row.definition_json)?;
            self.bind_stored_view_plan(&mut plan, &relations)
                .map_err(|error| {
                    StorageBackendError::Other(format!("restore view `{view_name}`: {error}"))
                })?;
            views.insert(row.relation, plan);
        }
        *self.views.write() = views;
        Ok(())
    }

    pub fn list_catalog_indexes(&self) -> StorageBackendResult<Vec<CatalogIndexRow>> {
        self.synchronize_catalog_registries()?;
        let mut out: Vec<CatalogIndexRow> = self.catalog_indexes.read().values().cloned().collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    /// Create a durable schema catalog object. Returns `true` when a new
    /// schema was created and `false` only for `IF NOT EXISTS`.
    pub fn register_schema(&self, name: &str, if_not_exists: bool) -> StorageBackendResult<bool> {
        self.with_implicit_storage_transaction(|engine| {
            engine.register_schema_inner(name, if_not_exists)
        })
    }

    fn register_schema_inner(&self, name: &str, if_not_exists: bool) -> StorageBackendResult<bool> {
        self.synchronize_catalog_registries()?;
        Self::validate_schema_name(name)?;
        let mut schemas = self.schemas.write();
        if schemas.contains(name) {
            if if_not_exists {
                return Ok(false);
            }
            return Err(StorageBackendError::Other(format!(
                "schema `{name}` already exists"
            )));
        }
        if let Some(catalog) = self.catalog.as_ref() {
            catalog.save_schema(name)?;
        }
        schemas.insert(name.to_string());
        drop(schemas);
        self.note_catalog_registry_changed();
        Ok(true)
    }

    /// Drop an empty durable schema. `public` and the virtual system
    /// namespaces cannot be removed.
    pub fn drop_schema(&self, name: &str) -> StorageBackendResult<bool> {
        self.with_implicit_storage_transaction(|engine| engine.drop_schema_inner(name))
    }

    pub(crate) fn preflight_drop_schema(&self, name: &str) -> StorageBackendResult<bool> {
        self.synchronize_catalog_registries()?;
        if matches!(name, "public" | "pg_catalog" | "information_schema") {
            return Err(StorageBackendError::Other(format!(
                "schema `{name}` cannot be dropped"
            )));
        }
        if !self.schemas.read().contains(name) {
            return Ok(false);
        }
        if !self.schema_is_empty(name) {
            return Err(StorageBackendError::Other(format!(
                "schema `{name}` is not empty"
            )));
        }
        Ok(true)
    }

    fn drop_schema_inner(&self, name: &str) -> StorageBackendResult<bool> {
        if !self.preflight_drop_schema(name)? {
            return Ok(false);
        }
        let mut schemas = self.schemas.write();
        if let Some(catalog) = self.catalog.as_ref() {
            catalog.drop_schema(name)?;
        }
        let removed = schemas.remove(name);
        drop(schemas);
        if removed {
            self.note_catalog_registry_changed();
        }
        Ok(removed)
    }

    pub fn has_schema(&self, name: &str) -> StorageBackendResult<bool> {
        self.synchronize_catalog_registries()?;
        Ok(self.schemas.read().contains(name))
    }

    pub(crate) fn validate_schema_name(name: &str) -> StorageBackendResult<()> {
        if name.is_empty() {
            return Err(StorageBackendError::Other(format!(
                "invalid schema name `{name}`"
            )));
        }
        if matches!(name, "pg_catalog" | "information_schema") {
            return Err(StorageBackendError::Other(format!(
                "schema name `{name}` is reserved"
            )));
        }
        Ok(())
    }

    fn schema_is_empty(&self, schema: &str) -> bool {
        !self
            .tables
            .read()
            .keys()
            .any(|relation| relation.schema == schema)
            && !self
                .views
                .read()
                .keys()
                .any(|relation| relation.schema == schema)
            && !self
                .sequences
                .read()
                .keys()
                .any(|relation| relation.schema == schema)
            && !self
                .foreign_tables
                .read()
                .keys()
                .any(|relation| relation.schema == schema)
            && !self.sql_user_functions.read().keys().any(|name| {
                RelationIdentity::from_legacy_name(name)
                    .map_or(true, |relation| relation.schema == schema)
            })
    }

    /// Sorted list of every registered schema. Mirrors the canonical UQA implementation's
    /// `Engine._tables.schemas`.
    pub fn list_schemas(&self) -> StorageBackendResult<Vec<String>> {
        self.synchronize_catalog_registries()?;
        Ok(self.schemas.read().iter().cloned().collect())
    }

    /// Local names of tables whose structural relation identity is owned by
    /// `schema`. No string-prefix inference participates in this lookup.
    pub fn tables_in_schema(&self, schema: &str) -> StorageBackendResult<Vec<String>> {
        self.synchronize_table_catalog()?;
        let mut out: Vec<String> = Vec::new();
        for relation in self.tables.read().keys() {
            if relation.schema == schema {
                out.push(relation.name.clone());
            }
        }
        out.sort_unstable();
        Ok(out)
    }

    pub fn list_sequences(&self) -> StorageBackendResult<Vec<String>> {
        self.refresh_sequences_from_catalog()?;
        let mut out: Vec<String> = self
            .sequences
            .read()
            .keys()
            .map(RelationIdentity::qualified_name)
            .collect();
        out.sort_unstable();
        Ok(out)
    }

    /// Current `search_path`. Mirrors the canonical UQA implementation's
    /// `Engine._tables.search_path`.
    pub fn search_path(&self) -> Vec<String> {
        self.search_path.read().clone()
    }

    /// First existing schema on this logical session's explicit search path.
    pub fn current_schema_name(&self) -> StorageBackendResult<Option<String>> {
        self.synchronize_catalog_registries()?;
        let schemas = self.schemas.read();
        Ok(self
            .search_path
            .read()
            .iter()
            .find(|name| schemas.contains(name.as_str()))
            .cloned())
    }

    /// Existing schemas visible through this logical session's search path.
    /// `PostgreSQL` implicitly searches `pg_catalog` unless it is already named
    /// explicitly; the flag controls whether that implicit entry is returned.
    pub fn current_schema_names(
        &self,
        include_implicit: bool,
    ) -> StorageBackendResult<Vec<String>> {
        self.synchronize_catalog_registries()?;
        let schemas = self.schemas.read();
        let path = self.search_path.read();
        let mut out = Vec::new();
        if include_implicit && !path.iter().any(|name| name == "pg_catalog") {
            out.push("pg_catalog".to_string());
        }
        for name in path.iter() {
            if (schemas.contains(name.as_str())
                || matches!(name.as_str(), "pg_catalog" | "information_schema"))
                && !out.contains(name)
            {
                out.push(name.clone());
            }
        }
        Ok(out)
    }

    /// Draw one value in `[0, 1)` from this logical session's PRNG.
    pub fn next_random_value(&self) -> f64 {
        let mut state = self.random_state.lock();
        let mut value = *state;
        // xorshift64*; every stored state is non-zero.
        value ^= value >> 12;
        value ^= value << 25;
        value ^= value >> 27;
        *state = value;
        let sample = value.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 11;
        sample as f64 * (1.0 / ((1_u64 << 53) as f64))
    }

    /// Reseed this logical session's PRNG. Equal seeds produce equal streams
    /// without changing any sibling session.
    pub fn set_random_seed(&self, seed: f64) -> Result<(), String> {
        if !seed.is_finite() || !(-1.0..=1.0).contains(&seed) {
            return Err(format!(
                "setseed parameter {seed} is out of allowed range [-1,1]"
            ));
        }
        let normalized = if seed == 0.0 { 0 } else { seed.to_bits() };
        let mut state = normalized ^ 0x9e37_79b9_7f4a_7c15;
        // SplitMix64 avalanche so nearby floating-point seeds do not create
        // correlated xorshift states.
        state = (state ^ (state >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        state = (state ^ (state >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        state ^= state >> 31;
        if state == 0 {
            state = 0x2545_f491_4f6c_dd1d;
        }
        *self.random_state.lock() = state;
        Ok(())
    }

    /// Replace the `search_path`. Empty input falls back to `["public"]`.
    pub fn set_search_path(&self, path: Vec<String>) {
        let mut value = path;
        if value.is_empty() {
            value.push("public".to_string());
        }
        *self.search_path.write() = value;
        self.clear_sql_statement_cache();
    }

    /// Apply `SET <name> [TO|=] <value>`. Honours `search_path`
    /// directly; every other parameter is stored in the session-vars
    /// map so a subsequent `SHOW <name>` can echo it back. Mirrors
    /// the canonical UQA implementation's session-variable behaviour.
    pub fn set_variable(&self, name: &str, value: &str) -> Result<(), SQLError> {
        if !super::is_known_runtime_parameter(name) {
            return Err(SQLError::Routine {
                sqlstate: "42704".into(),
                message: format!("unrecognized configuration parameter \"{name}\""),
            });
        }
        if !super::is_mutable_runtime_parameter(name) {
            return Err(SQLError::Routine {
                sqlstate: "55P02".into(),
                message: format!("parameter \"{name}\" cannot be changed"),
            });
        }
        if name.eq_ignore_ascii_case("work_mem") {
            Self::parse_work_mem_bytes(value)?;
        }
        if name.eq_ignore_ascii_case("search_path") {
            let parts = parse_search_path_list(value)?;
            self.set_search_path(parts);
            self.session_vars
                .write()
                .insert(name.to_string(), value.to_string());
            return Ok(());
        }
        self.session_vars
            .write()
            .insert(name.to_string(), value.to_string());
        Ok(())
    }

    /// Read back a session variable. `search_path` always resolves to
    /// the current resolution order; every other key looks up the
    /// session-vars map, then PostgreSQL-compatible runtime defaults,
    /// and finally the registered runtime default. Unknown parameters are
    /// errors rather than successful empty strings.
    pub fn show_variable(&self, name: &str) -> Result<String, SQLError> {
        if name.eq_ignore_ascii_case("search_path") {
            return Ok(self.search_path().join(","));
        }
        let session_vars = self.session_vars.read();
        if let Some(value) = session_vars.get(name) {
            return Ok(value.clone());
        }
        if let Some((_, value)) = session_vars
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
        {
            return Ok(value.clone());
        }
        super::default_runtime_parameter(name)
            .map(str::to_string)
            .ok_or_else(|| SQLError::Routine {
                sqlstate: "42704".into(),
                message: format!("unrecognized configuration parameter \"{name}\""),
            })
    }

    pub(crate) fn work_mem_bytes(&self) -> Result<usize, SQLError> {
        Self::parse_work_mem_bytes(&self.show_variable("work_mem")?)
    }

    fn parse_work_mem_bytes(raw: &str) -> Result<usize, SQLError> {
        let compact = raw
            .trim()
            .chars()
            .filter(|character| !character.is_ascii_whitespace())
            .collect::<String>();
        let digits = compact.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            return Err(SQLError::TypeMismatch(format!(
                "work_mem must be a positive byte size, got {raw:?}"
            )));
        }
        let amount = compact[..digits].parse::<usize>().map_err(|_| {
            SQLError::TypeMismatch(format!("work_mem is outside the supported range: {raw:?}"))
        })?;
        if amount == 0 {
            return Err(SQLError::TypeMismatch(
                "work_mem must be greater than zero".into(),
            ));
        }
        let unit = compact[digits..].to_ascii_lowercase();
        let exponent = match unit.as_str() {
            // PostgreSQL interprets a bare work_mem integer as kilobytes.
            "b" => 0,
            "" | "k" | "kb" | "kib" => 1,
            "m" | "mb" | "mib" => 2,
            "g" | "gb" | "gib" => 3,
            "t" | "tb" | "tib" => 4,
            _ => {
                return Err(SQLError::TypeMismatch(format!(
                    "unsupported work_mem unit in {raw:?}"
                )))
            }
        };
        let multiplier = 1024_usize.checked_pow(exponent).ok_or_else(|| {
            SQLError::TypeMismatch(format!("work_mem is outside the supported range: {raw:?}"))
        })?;
        amount.checked_mul(multiplier).ok_or_else(|| {
            SQLError::TypeMismatch(format!("work_mem is outside the supported range: {raw:?}"))
        })
    }

    /// Apply `DISCARD <target>`. Mirrors the canonical UQA implementation's `_compile_discard`:
    /// `ALL` resets every kind of session state; the narrower
    /// variants are scoped accordingly.
    pub fn discard(&self, target: uqa_sql::ast::DiscardTarget) -> Result<(), SQLError> {
        use uqa_sql::ast::DiscardTarget;
        match target {
            DiscardTarget::All => {
                self.session_vars.write().clear();
                self.prepared.write().clear();
                self.sequence_currvals.write().clear();
                self.clear_sql_statement_cache();
                self.set_search_path(vec!["public".to_string()]);
            }
            DiscardTarget::Plans => {
                self.prepared.write().clear();
                self.clear_sql_statement_cache();
            }
            DiscardTarget::Sequences => {
                self.sequence_currvals.write().clear();
            }
            DiscardTarget::Temp => {
                return Err(SQLError::Unsupported(
                    "DISCARD TEMP requires temporary-table support".to_string(),
                ));
            }
        }
        Ok(())
    }

    /// Refresh per-column statistics for a single table or every
    /// table when `table` is `None`. Mirrors `Table.analyze` in
    /// the canonical UQA behavior: scans every document, collects per-
    /// column distinct count / null count / min / max / equi-depth
    /// histogram (100 buckets) / MCV list (top 10 above-average
    /// frequency), and stores the result on the per-table state so the
    /// cardinality estimator can read it on subsequent queries.
    pub fn run_analyze(&self, table: Option<&str>) -> StorageBackendResult<()> {
        self.with_implicit_storage_transaction(|engine| engine.run_analyze_inner(table))
    }

    fn run_analyze_inner(&self, table: Option<&str>) -> StorageBackendResult<()> {
        if let Some(name) = table {
            let Some(canonical_name) = self.try_resolve_table_name(name)? else {
                return Err(StorageBackendError::Other(format!(
                    "ANALYZE target table `{name}` does not exist"
                )));
            };
            let Some(table) = self.try_table(&canonical_name)? else {
                return Err(StorageBackendError::Other(format!(
                    "ANALYZE target table `{name}` does not exist"
                )));
            };
            self.analyze_table(&canonical_name, &table, true)?;
        } else {
            // The catalog can change between collecting the names and opening
            // each table in another session. Missing entries are only benign
            // for the catalog-wide form; an explicitly named table above is
            // always an error.
            let names: Vec<String> = self
                .tables
                .read()
                .keys()
                .map(RelationIdentity::qualified_name)
                .collect();
            for name in names {
                let Some(table) = self.table(&name)? else {
                    continue;
                };
                self.analyze_table(&name, &table, true)?;
            }
        }
        // Persisted statistics participate in DPccp join ordering and every
        // cached optimized statement. Publish the same commit-delayed data
        // generation even when ANALYZE did not change document contents.
        self.note_table_data_changed();
        Ok(())
    }

    pub(crate) fn mark_column_stats_dirty(
        &self,
        canonical_table_name: &str,
        table: &Arc<TableState>,
    ) -> StorageBackendResult<()> {
        if !table.column_stats_dirty.load(Ordering::Acquire) {
            if let Some(catalog) = self.catalog.as_ref() {
                catalog.delete_column_stats(canonical_table_name)?;
            }
        }
        table.doc_count_dirty.store(true, Ordering::Release);
        table.column_stats_dirty.store(true, Ordering::Release);
        self.note_table_data_changed();
        Ok(())
    }

    fn analyze_table(
        &self,
        canonical_table_name: &str,
        t: &Arc<TableState>,
        persist: bool,
    ) -> StorageBackendResult<()> {
        let snapshot = t.document_store.read().snapshot()?;
        let doc_ids: Vec<DocId> = {
            let mut v = snapshot.doc_ids()?;
            v.sort_unstable();
            v
        };
        let n = u64::try_from(doc_ids.len())
            .map_err(|_| StorageBackendError::Other("ANALYZE document count exceeds u64".into()))?;
        let columns: Vec<String> = t.columns.read().iter().map(|c| c.name.clone()).collect();

        let (mut col_values, mut col_nulls) =
            collect_analyze_values(snapshot.as_ref(), &doc_ids, &columns)?;

        let mut stats_out: BTreeMap<String, uqa_planner::ColumnStats> = BTreeMap::new();
        for col in &columns {
            let values = col_values.remove(col).ok_or_else(|| {
                StorageBackendError::Other(format!(
                    "ANALYZE lost the value buffer for column `{col}`"
                ))
            })?;
            let null_count = col_nulls.remove(col).ok_or_else(|| {
                StorageBackendError::Other(format!(
                    "ANALYZE lost the null counter for column `{col}`"
                ))
            })?;
            let distinct = distinct_count(&values)?;
            let comparable: Vec<&Value> = values
                .iter()
                .filter(|v| {
                    matches!(
                        v,
                        Value::Int(_) | Value::Float(_) | Value::Str(_) | Value::Bool(_)
                    )
                })
                .collect();
            let min_val = comparable.iter().min().map(|v| (*v).clone());
            let max_val = comparable.iter().max().map(|v| (*v).clone());

            let histogram = build_histogram(&comparable);
            let (mcv_values, mcv_frequencies) = build_mcv(&values, n);

            stats_out.insert(
                col.clone(),
                uqa_planner::ColumnStats {
                    distinct_count: distinct,
                    null_count,
                    min_value: min_val,
                    max_value: max_val,
                    row_count: n,
                    histogram,
                    mcv_values,
                    mcv_frequencies,
                },
            );
        }

        if persist {
            if let Some(catalog) = self.catalog.as_ref() {
                Self::persist_column_stats(catalog.as_ref(), canonical_table_name, &stats_out)?;
            }
        }
        *t.column_stats.write() = stats_out;
        t.column_stats_loaded.store(true, Ordering::Release);
        t.column_stats_dirty.store(false, Ordering::Release);
        Ok(())
    }

    fn persist_column_stats(
        catalog: &dyn CatalogFacade,
        table_name: &str,
        stats: &BTreeMap<String, uqa_planner::ColumnStats>,
    ) -> StorageBackendResult<()> {
        struct EncodedColumnStats {
            column_name: String,
            distinct_count: i64,
            null_count: i64,
            min_json: Option<String>,
            max_json: Option<String>,
            row_count: i64,
            histogram_json: String,
            mcv_values_json: String,
            mcv_frequencies_json: String,
        }

        let mut encoded = Vec::with_capacity(stats.len());
        for (col_name, cs) in stats {
            let min_json = cs
                .min_value
                .as_ref()
                .map(serde_json::to_string)
                .transpose()?;
            let max_json = cs
                .max_value
                .as_ref()
                .map(serde_json::to_string)
                .transpose()?;
            let histogram_json = serde_json::to_string(&cs.histogram)?;
            let mcv_values_json = serde_json::to_string(&cs.mcv_values)?;
            let mcv_frequencies_json = serde_json::to_string(&cs.mcv_frequencies)?;
            encoded.push(EncodedColumnStats {
                column_name: col_name.clone(),
                distinct_count: Self::u64_to_i64("distinct count", cs.distinct_count)?,
                null_count: Self::u64_to_i64("null count", cs.null_count)?,
                min_json,
                max_json,
                row_count: Self::u64_to_i64("row count", cs.row_count)?,
                histogram_json,
                mcv_values_json,
                mcv_frequencies_json,
            });
        }
        let rows = encoded
            .iter()
            .map(|stats| ColumnStatsInput {
                table_name,
                column_name: &stats.column_name,
                distinct_count: stats.distinct_count,
                null_count: stats.null_count,
                min_value: stats.min_json.as_deref(),
                max_value: stats.max_json.as_deref(),
                row_count: stats.row_count,
                histogram_json: &stats.histogram_json,
                mcv_values_json: &stats.mcv_values_json,
                mcv_frequencies_json: &stats.mcv_frequencies_json,
            })
            .collect::<Vec<_>>();
        catalog.replace_column_stats(table_name, &rows)
    }

    fn u64_to_i64(kind: &str, value: u64) -> StorageBackendResult<i64> {
        i64::try_from(value).map_err(|_| {
            StorageBackendError::Other(format!(
                "ANALYZE {kind} {value} exceeds the persistent i64 range"
            ))
        })
    }

    /// Snapshot of the cardinality estimator's per-column statistics
    /// for `table`. Dirty stats are recomputed lazily so callers do not
    /// need to issue `ANALYZE` after every data change.
    pub fn column_stats(
        &self,
        table: &str,
    ) -> StorageBackendResult<BTreeMap<String, uqa_planner::ColumnStats>> {
        self.try_column_stats(table)
    }

    pub fn try_column_stats(
        &self,
        table: &str,
    ) -> StorageBackendResult<BTreeMap<String, uqa_planner::ColumnStats>> {
        // Lazy analysis must be linearizable with direct table mutations. A
        // stale scan must not publish `column_stats_dirty = false` after a
        // concurrent writer marked the table dirty. The gate is re-entrant
        // for optimizer calls already executing inside Engine::sql.
        let _statement = self.statement_gate.lock();
        self.synchronize_table_data()?;
        let canonical_name = self
            .try_resolve_table_name(table)?
            .ok_or_else(|| StorageBackendError::Other(format!("table `{table}` does not exist")))?;
        let t = self
            .try_table(&canonical_name)?
            .ok_or_else(|| StorageBackendError::Other(format!("table `{table}` does not exist")))?;
        if t.column_stats_dirty.load(Ordering::Acquire) {
            self.analyze_table(&canonical_name, &t, false)?;
        }
        let stats = t.column_stats.read().clone();
        Ok(stats)
    }
}

#[cfg(test)]
mod tests {
    use super::{Engine, QueryPlan, RelationIdentity, RelationalPlan, SourcePlan};

    fn lower_query(sql: &str) -> QueryPlan {
        let statement = uqa_sql::compile(sql).unwrap().remove(0);
        let uqa_planner::UnifiedPlan::Query(plan) = uqa_planner::UnifiedPlan::lower(statement)
        else {
            panic!("expected a query plan");
        };
        *plan
    }

    fn root_table_name(plan: &QueryPlan) -> &str {
        let RelationalPlan::QueryBlock(block) = &plan.root else {
            panic!("expected a query block");
        };
        let Some(SourcePlan::Table { name, .. }) = block.from.as_ref() else {
            panic!("expected a table source");
        };
        name
    }

    #[test]
    fn analyze_counter_conversion_rejects_values_above_sqlite_range() {
        let max_i64 = u64::try_from(i64::MAX).unwrap();
        assert_eq!(Engine::u64_to_i64("row count", max_i64).unwrap(), i64::MAX);
        let error = Engine::u64_to_i64("row count", max_i64 + 1).unwrap_err();
        assert!(error
            .to_string()
            .contains("exceeds the persistent i64 range"));
    }

    #[test]
    fn legacy_view_source_binding_requires_one_catalog_identity() {
        let engine = Engine::new();
        let mut unique = lower_query("SELECT * FROM items");
        engine
            .bind_stored_view_plan(
                &mut unique,
                &std::collections::BTreeSet::from([RelationIdentity::new("app", "items")]),
            )
            .unwrap();
        assert_eq!(root_table_name(&unique), "app.items");

        let mut ambiguous = lower_query("SELECT * FROM items");
        let error = engine
            .bind_stored_view_plan(
                &mut ambiguous,
                &std::collections::BTreeSet::from([
                    RelationIdentity::new("app", "items"),
                    RelationIdentity::new("public", "items"),
                ]),
            )
            .unwrap_err();
        assert!(error.to_string().contains("ambiguous stored view source"));

        let mut missing = lower_query("SELECT * FROM items");
        let error = engine
            .bind_stored_view_plan(&mut missing, &std::collections::BTreeSet::new())
            .unwrap_err();
        assert!(error.to_string().contains("does not exist"));
    }

    #[test]
    fn stored_view_binding_preserves_cte_sources() {
        let engine = Engine::new();
        let mut plan = lower_query("WITH items AS (VALUES (1)) SELECT * FROM items");
        engine
            .bind_stored_view_plan(&mut plan, &std::collections::BTreeSet::new())
            .unwrap();
        assert_eq!(root_table_name(&plan), "items");
    }

    #[test]
    fn legacy_view_sequence_binding_requires_one_catalog_identity() {
        let engine = Engine::new();
        engine.sql("CREATE SCHEMA app", &[]).unwrap();
        engine.sql("CREATE SEQUENCE app.ids", &[]).unwrap();

        let mut unique = lower_query("SELECT nextval('ids')");
        engine
            .bind_stored_view_plan(&mut unique, &std::collections::BTreeSet::new())
            .unwrap();
        let mut references = Vec::new();
        unique.rewrite_scalar_expressions(&mut |expression| {
            if let Some(reference) = super::sequence_function_reference_mut(expression) {
                references.push(reference.clone());
            }
        });
        assert_eq!(references, ["app.ids"]);

        engine.sql("CREATE SEQUENCE public.ids", &[]).unwrap();
        let mut ambiguous = lower_query("SELECT currval('ids')");
        let error = engine
            .bind_stored_view_plan(&mut ambiguous, &std::collections::BTreeSet::new())
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("ambiguous persisted sequence reference"));

        let missing_engine = Engine::new();
        let mut missing = lower_query("SELECT setval('ids', 1)");
        let error = missing_engine
            .bind_stored_view_plan(&mut missing, &std::collections::BTreeSet::new())
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("dangling persisted sequence reference"));
    }
}
