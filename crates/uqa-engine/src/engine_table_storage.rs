//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    AnalyzerPhase, Arc, BTreeMap, DocId, Document, Engine, FieldName, IVFIndexParams,
    RelationIdentity, SQLError, StorageBackendError, StorageBackendResult, TableState, Value,
};
use crate::CatalogIndexRow;

/// Answer of the value-index conflict probe in [`Engine::find_conflict`].
enum IndexConflictProbe {
    /// No conflict column has a usable value index; fall back to the
    /// evaluated document scan.
    Unanswerable,
    /// The index answered: no existing row matches the conflict target.
    NoConflict,
    /// The index answered: this existing row matches the conflict target.
    Conflict(DocId),
}

fn table_not_found(table: &str) -> StorageBackendError {
    StorageBackendError::Other(format!("table `{table}` does not exist"))
}

fn column_not_found(table: &str, column: &str) -> StorageBackendError {
    StorageBackendError::Other(format!(
        "column `{column}` does not exist on table `{table}`"
    ))
}

fn stored_relation_reference_matches(reference: &str, target: &RelationIdentity) -> bool {
    match RelationIdentity::parse_reference(reference) {
        Ok((Some(schema), name)) => schema == target.schema && name == target.name,
        Ok((None, name)) => name == target.name,
        // Corrupt legacy metadata is never evidence that a dependency is
        // absent. DDL must fail closed rather than leave it dangling.
        Err(_) => true,
    }
}

fn walk_schema_expr_mut(
    expression: &mut uqa_sql::ast::Expr,
    visit: &mut impl FnMut(&mut uqa_sql::ast::Expr) -> StorageBackendResult<()>,
) -> StorageBackendResult<()> {
    use uqa_sql::ast::{Expr, FrameBound};

    visit(expression)?;
    match expression {
        Expr::Func {
            args,
            order_by,
            filter,
            ..
        } => {
            for argument in args {
                walk_schema_expr_mut(argument, visit)?;
            }
            for order in order_by {
                walk_schema_expr_mut(&mut order.expr, visit)?;
            }
            if let Some(filter) = filter {
                walk_schema_expr_mut(filter, visit)?;
            }
        }
        Expr::Array(items) | Expr::And(items) | Expr::Or(items) => {
            for item in items {
                walk_schema_expr_mut(item, visit)?;
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            walk_schema_expr_mut(lhs, visit)?;
            walk_schema_expr_mut(rhs, visit)?;
        }
        Expr::Not(inner) | Expr::IsNull { expr: inner, .. } | Expr::Cast { expr: inner, .. } => {
            walk_schema_expr_mut(inner, visit)?;
        }
        Expr::Between { expr, low, high } => {
            walk_schema_expr_mut(expr, visit)?;
            walk_schema_expr_mut(low, visit)?;
            walk_schema_expr_mut(high, visit)?;
        }
        Expr::InList { expr, list, .. } => {
            walk_schema_expr_mut(expr, visit)?;
            for item in list {
                walk_schema_expr_mut(item, visit)?;
            }
        }
        Expr::WindowCall { args, spec, .. } => {
            for argument in args {
                walk_schema_expr_mut(argument, visit)?;
            }
            for partition in &mut spec.partition_by {
                walk_schema_expr_mut(partition, visit)?;
            }
            for order in &mut spec.order_by {
                walk_schema_expr_mut(&mut order.expr, visit)?;
            }
            if let Some(frame) = &mut spec.frame {
                for bound in [&mut frame.start, &mut frame.end] {
                    match bound {
                        FrameBound::Preceding(expression) | FrameBound::Following(expression) => {
                            walk_schema_expr_mut(expression, visit)?;
                        }
                        FrameBound::UnboundedPreceding
                        | FrameBound::UnboundedFollowing
                        | FrameBound::CurrentRow => {}
                    }
                }
            }
        }
        Expr::Case {
            base,
            when,
            else_branch,
        } => {
            if let Some(base) = base {
                walk_schema_expr_mut(base, visit)?;
            }
            for (condition, result) in when {
                walk_schema_expr_mut(condition, visit)?;
                walk_schema_expr_mut(result, visit)?;
            }
            if let Some(else_branch) = else_branch {
                walk_schema_expr_mut(else_branch, visit)?;
            }
        }
        Expr::ScalarSubquery(_) | Expr::Exists { .. } | Expr::InSubquery { .. } => {
            return Err(StorageBackendError::Other(
                "schema expression contains a subquery whose dependencies cannot be rewritten safely"
                    .into(),
            ));
        }
        Expr::Star
        | Expr::Column(_)
        | Expr::QualifiedColumn { .. }
        | Expr::Literal(_)
        | Expr::Param(_) => {}
    }
    Ok(())
}

fn rewrite_sequence_function_references(
    expression: &mut uqa_sql::ast::Expr,
    visit: &mut impl FnMut(&mut String) -> StorageBackendResult<()>,
) -> StorageBackendResult<()> {
    walk_schema_expr_mut(expression, &mut |node| {
        let uqa_sql::ast::Expr::Func { name, args, .. } = node else {
            return Ok(());
        };
        let lower = name.to_ascii_lowercase();
        let local = lower.strip_prefix("pg_catalog.").unwrap_or(&lower);
        if !matches!(local, "nextval" | "currval" | "setval")
            || (lower.contains('.') && !lower.starts_with("pg_catalog."))
        {
            return Ok(());
        }
        let Some(reference) = args.first_mut().and_then(regclass_literal_mut) else {
            // A dynamically computed text argument deliberately retains
            // late-binding semantics. Only a literal regclass spelling is an
            // early-bound catalog dependency.
            return Ok(());
        };
        visit(reference)
    })
}

fn regclass_literal_mut(expression: &mut uqa_sql::ast::Expr) -> Option<&mut String> {
    match expression {
        uqa_sql::ast::Expr::Literal(Value::Str(reference)) => Some(reference),
        uqa_sql::ast::Expr::Cast { expr, ty }
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

fn schema_expr_references_column(expression: &uqa_sql::ast::Expr, column: &str) -> bool {
    let mut expression = expression.clone();
    let mut referenced = false;
    let result = walk_schema_expr_mut(&mut expression, &mut |node| {
        referenced |= match node {
            uqa_sql::ast::Expr::Star => true,
            uqa_sql::ast::Expr::Column(name)
            | uqa_sql::ast::Expr::QualifiedColumn { column: name, .. } => name == column,
            _ => false,
        };
        Ok(())
    });
    result.is_err() || referenced
}

fn rename_schema_expr_column(
    expression: &mut uqa_sql::ast::Expr,
    from: &str,
    to: &str,
) -> StorageBackendResult<()> {
    walk_schema_expr_mut(expression, &mut |node| {
        match node {
            uqa_sql::ast::Expr::Star => {
                return Err(StorageBackendError::Other(
                    "schema expression contains `*` and cannot be rewritten safely".into(),
                ));
            }
            uqa_sql::ast::Expr::Column(name) if name == from => *name = to.to_string(),
            uqa_sql::ast::Expr::QualifiedColumn {
                qualifier,
                column,
                key,
            } if column == from => {
                *column = to.to_string();
                *key = format!("{qualifier}.{to}");
            }
            _ => {}
        }
        Ok(())
    })
}

fn schema_expr_references_relation(
    expression: &uqa_sql::ast::Expr,
    target: &RelationIdentity,
) -> bool {
    let mut expression = expression.clone();
    let mut referenced = false;
    let result = walk_schema_expr_mut(&mut expression, &mut |node| {
        if let uqa_sql::ast::Expr::QualifiedColumn { qualifier, .. } = node {
            referenced |= stored_relation_reference_matches(qualifier, target);
        }
        Ok(())
    });
    result.is_err() || referenced
}

fn rename_schema_expr_relation(
    expression: &mut uqa_sql::ast::Expr,
    from: &RelationIdentity,
    to: &str,
) -> StorageBackendResult<()> {
    walk_schema_expr_mut(expression, &mut |node| {
        if let uqa_sql::ast::Expr::QualifiedColumn {
            qualifier,
            column,
            key,
        } = node
        {
            if stored_relation_reference_matches(qualifier, from) {
                *qualifier = to.to_string();
                *key = format!("{to}.{column}");
            }
        }
        Ok(())
    })
}

fn rename_schema_expr_qualified_column(
    expression: &mut uqa_sql::ast::Expr,
    table: &RelationIdentity,
    from: &str,
    to: &str,
) -> StorageBackendResult<()> {
    walk_schema_expr_mut(expression, &mut |node| {
        if let uqa_sql::ast::Expr::QualifiedColumn {
            qualifier,
            column,
            key,
        } = node
        {
            if column == from && stored_relation_reference_matches(qualifier, table) {
                *column = to.to_string();
                *key = format!("{qualifier}.{to}");
            }
        }
        Ok(())
    })
}

impl Engine {
    fn resolve_table_ddl_target(
        &self,
        name: &str,
        action: &str,
    ) -> StorageBackendResult<Option<String>> {
        match self.try_resolve_relation_kind(name)? {
            Some((canonical, "table")) => Ok(Some(canonical)),
            Some((canonical, kind)) => Err(StorageBackendError::Other(format!(
                "{action}: relation `{canonical}` is a {kind}, not a table"
            ))),
            None => Ok(None),
        }
    }

    fn catalog_index_columns(row: &CatalogIndexRow) -> StorageBackendResult<Vec<String>> {
        serde_json::from_str(&row.columns_json).map_err(StorageBackendError::from)
    }

    fn catalog_index_references_column(
        row: &CatalogIndexRow,
        column: &str,
    ) -> StorageBackendResult<bool> {
        Ok(Self::catalog_index_columns(row)?
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(column)))
    }

    fn catalog_index_with_renamed_column(
        mut row: CatalogIndexRow,
        from: &str,
        to: &str,
    ) -> StorageBackendResult<CatalogIndexRow> {
        let mut columns = Self::catalog_index_columns(&row)?;
        let mut changed = false;
        for column in &mut columns {
            if column.eq_ignore_ascii_case(from) {
                *column = to.to_string();
                changed = true;
            }
        }
        if changed {
            row.columns_json =
                serde_json::to_string(&columns).map_err(StorageBackendError::from)?;
        }
        Ok(row)
    }

    fn remove_catalog_indexes_for_column(
        &self,
        table: &str,
        column: &str,
    ) -> StorageBackendResult<()> {
        let mut rows = self.catalog_indexes.write();
        let mut removals = Vec::new();
        for (name, row) in rows.iter() {
            if row.table_name == table && Self::catalog_index_references_column(row, column)? {
                removals.push(name.clone());
            }
        }
        for name in removals {
            rows.remove(&name);
        }
        Ok(())
    }

    fn rename_catalog_index_table_refs(&self, from: &str, to: &str) {
        for row in self.catalog_indexes.write().values_mut() {
            if row.table_name == from {
                row.table_name = to.to_string();
            }
        }
    }

    fn rename_catalog_index_column_refs(
        &self,
        table: &str,
        from: &str,
        to: &str,
    ) -> StorageBackendResult<()> {
        let mut rows = self.catalog_indexes.write();
        let mut updates = Vec::new();
        for (name, row) in rows.iter() {
            if row.table_name == table && Self::catalog_index_references_column(row, from)? {
                let renamed = Self::catalog_index_with_renamed_column(row.clone(), from, to)?;
                updates.push((name.clone(), renamed.columns_json));
            }
        }
        for (name, columns_json) in updates {
            if let Some(row) = rows.get_mut(&name) {
                row.columns_json = columns_json;
            }
        }
        Ok(())
    }

    fn ensure_no_dependent_views(
        &self,
        action: &str,
        canonical_name: &str,
    ) -> StorageBackendResult<()> {
        let dependents = self.views_depending_on_relation(canonical_name)?;
        if dependents.is_empty() {
            return Ok(());
        }
        Err(StorageBackendError::Other(format!(
            "{action} `{canonical_name}` rejected: dependent view(s) `{}` use stored relation names that cannot be rewritten safely",
            dependents.join("`, `")
        )))
    }

    fn table_entries(&self) -> Vec<(String, Arc<TableState>)> {
        self.tables
            .read()
            .iter()
            .map(|(relation, state)| (relation.qualified_name(), state.clone()))
            .collect()
    }

    fn foreign_key_targets(
        foreign_key: &uqa_sql::ast::ForeignKey,
        target: &RelationIdentity,
    ) -> bool {
        stored_relation_reference_matches(&foreign_key.ref_table, target)
    }

    fn canonical_foreign_key_target(&self, reference: &str) -> StorageBackendResult<String> {
        self.try_resolve_table_name(reference)?
            .ok_or_else(|| table_not_found(reference))
    }

    fn canonical_stored_foreign_key_target(&self, reference: &str) -> StorageBackendResult<String> {
        let (schema, local_name) =
            RelationIdentity::parse_reference(reference).map_err(|error| {
                StorageBackendError::Other(format!(
                    "invalid persisted foreign-key target `{reference}`: {error}"
                ))
            })?;
        let tables = self.tables.read();
        if let Some(schema) = schema {
            let target = RelationIdentity::new(schema, local_name);
            if tables.contains_key(&target) {
                return Ok(target.qualified_name());
            }
            return Err(StorageBackendError::Other(format!(
                "dangling persisted foreign-key target `{reference}`"
            )));
        }

        let candidates = tables
            .keys()
            .filter(|candidate| candidate.name == local_name)
            .map(RelationIdentity::qualified_name)
            .collect::<Vec<_>>();
        match candidates.as_slice() {
            [target] => Ok(target.clone()),
            [] => Err(StorageBackendError::Other(format!(
                "dangling persisted foreign-key target `{reference}`"
            ))),
            _ => Err(StorageBackendError::Other(format!(
                "ambiguous persisted foreign-key target `{reference}` matches {}",
                candidates.join(", ")
            ))),
        }
    }

    fn bind_sequence_references_in_expr(
        &self,
        expression: &mut uqa_sql::ast::Expr,
    ) -> StorageBackendResult<()> {
        rewrite_sequence_function_references(expression, &mut |reference| {
            *reference = self.resolve_sequence_reference_for_binding(reference)?;
            Ok(())
        })
    }

    fn resolve_stored_sequence_references_in_expr(
        &self,
        expression: &mut uqa_sql::ast::Expr,
    ) -> StorageBackendResult<()> {
        rewrite_sequence_function_references(expression, &mut |reference| {
            *reference = self.resolve_stored_sequence_reference(reference)?;
            Ok(())
        })
    }

    fn stored_sequence_targets_in_expr(
        &self,
        expression: &uqa_sql::ast::Expr,
    ) -> StorageBackendResult<std::collections::BTreeSet<String>> {
        let mut expression = expression.clone();
        let mut targets = std::collections::BTreeSet::new();
        rewrite_sequence_function_references(&mut expression, &mut |reference| {
            let canonical = self.resolve_stored_sequence_reference(reference)?;
            targets.insert(canonical.clone());
            *reference = canonical;
            Ok(())
        })?;
        Ok(targets)
    }

    pub(crate) fn ensure_no_sequence_default_dependencies(
        &self,
        sequence: &str,
    ) -> StorageBackendResult<()> {
        self.synchronize_table_catalog()?;
        let mut dependents = Vec::new();
        for (table_name, table) in self.table_entries() {
            for column in table.columns.read().iter() {
                let Some(default) = &column.default else {
                    continue;
                };
                if self
                    .stored_sequence_targets_in_expr(default)?
                    .contains(sequence)
                {
                    dependents.push(format!("{table_name}.{}", column.name));
                }
            }
        }
        if dependents.is_empty() {
            return Ok(());
        }
        Err(StorageBackendError::Other(format!(
            "column default(s) `{}` depend on sequence `{sequence}`",
            dependents.join("`, `")
        )))
    }

    fn table_schema_references_relation(table: &TableState, target: &RelationIdentity) -> bool {
        table.columns.read().iter().any(|column| {
            column
                .default
                .as_ref()
                .is_some_and(|expr| schema_expr_references_relation(expr, target))
                || column
                    .check
                    .as_ref()
                    .is_some_and(|expr| schema_expr_references_relation(expr, target))
        }) || table
            .table_checks
            .read()
            .iter()
            .any(|check| schema_expr_references_relation(&check.expr, target))
    }

    fn persist_constraint_candidate(
        &self,
        name: &str,
        table: &TableState,
        columns: &[uqa_sql::ast::ColumnDef],
        checks: &[uqa_sql::ast::TableCheck],
        foreign_keys: &[uqa_sql::ast::ForeignKey],
        key_constraints: &[uqa_sql::ast::TableKeyConstraint],
    ) -> StorageBackendResult<()> {
        let constraints = uqa_sql::ast::TableConstraintSet {
            checks: checks.to_vec(),
            foreign_keys: foreign_keys.to_vec(),
            key_constraints: key_constraints.to_vec(),
        };
        self.try_save_table_schema_with_components(name, table, columns, &constraints)
    }

    fn rewrite_table_rename_dependencies(&self, from: &str, to: &str) -> StorageBackendResult<()> {
        self.ensure_no_dependent_views("ALTER TABLE RENAME", from)?;
        let from_relation = Self::resolved_relation_identity(from)?;
        let mut updates = Vec::new();
        for (table_name, table) in self.table_entries() {
            let mut columns = table.columns.read().clone();
            let mut checks = table.table_checks.read().clone();
            let mut foreign_keys = table.foreign_keys.read().clone();
            let key_constraints = table.key_constraints.read().clone();
            let mut changed = false;

            for column in &mut columns {
                for expression in [&mut column.default, &mut column.check]
                    .into_iter()
                    .flatten()
                {
                    if schema_expr_references_relation(expression, &from_relation) {
                        rename_schema_expr_relation(expression, &from_relation, to)?;
                        changed = true;
                    }
                }
                if let Some(reference) = &mut column.references {
                    if stored_relation_reference_matches(&reference.table, &from_relation) {
                        reference.table = to.to_string();
                        changed = true;
                    }
                }
            }
            for check in &mut checks {
                if schema_expr_references_relation(&check.expr, &from_relation) {
                    rename_schema_expr_relation(&mut check.expr, &from_relation, to)?;
                    changed = true;
                }
            }
            for foreign_key in &mut foreign_keys {
                if Self::foreign_key_targets(foreign_key, &from_relation) {
                    foreign_key.ref_table = to.to_string();
                    changed = true;
                }
            }
            if changed {
                self.persist_constraint_candidate(
                    &table_name,
                    &table,
                    &columns,
                    &checks,
                    &foreign_keys,
                    &key_constraints,
                )?;
                updates.push((table, columns, checks, foreign_keys));
            }
        }
        for (table, columns, checks, foreign_keys) in updates {
            *table.columns.write() = columns;
            *table.table_checks.write() = checks;
            *table.foreign_keys.write() = foreign_keys;
        }
        Ok(())
    }

    fn rewrite_column_rename_dependencies(
        &self,
        table_name: &str,
        from: &str,
        to: &str,
    ) -> StorageBackendResult<()> {
        self.ensure_no_dependent_views("ALTER TABLE RENAME COLUMN", table_name)?;
        let target = Self::resolved_relation_identity(table_name)?;
        let mut updates = Vec::new();
        for (candidate_name, table) in self.table_entries() {
            let is_target = candidate_name == table_name;
            let mut columns = table.columns.read().clone();
            let mut checks = table.table_checks.read().clone();
            let mut foreign_keys = table.foreign_keys.read().clone();
            let key_constraints = table.key_constraints.read().clone();
            let mut changed = false;

            for column in &mut columns {
                for expression in [&mut column.default, &mut column.check]
                    .into_iter()
                    .flatten()
                {
                    if is_target && schema_expr_references_column(expression, from) {
                        rename_schema_expr_column(expression, from, to)?;
                        changed = true;
                    } else if !is_target && schema_expr_references_relation(expression, &target) {
                        rename_schema_expr_qualified_column(expression, &target, from, to)?;
                        changed = true;
                    }
                }
                if let Some(reference) = &mut column.references {
                    if stored_relation_reference_matches(&reference.table, &target)
                        && reference.column == from
                    {
                        reference.column = to.to_string();
                        changed = true;
                    }
                }
            }
            for check in &mut checks {
                if is_target && schema_expr_references_column(&check.expr, from) {
                    rename_schema_expr_column(&mut check.expr, from, to)?;
                    changed = true;
                } else if !is_target && schema_expr_references_relation(&check.expr, &target) {
                    rename_schema_expr_qualified_column(&mut check.expr, &target, from, to)?;
                    changed = true;
                }
            }
            for foreign_key in &mut foreign_keys {
                if is_target {
                    for column in &mut foreign_key.local_columns {
                        if column == from {
                            *column = to.to_string();
                            changed = true;
                        }
                    }
                    for column in &mut foreign_key.on_delete_set_columns {
                        if column == from {
                            *column = to.to_string();
                            changed = true;
                        }
                    }
                }
                if Self::foreign_key_targets(foreign_key, &target) {
                    for column in &mut foreign_key.ref_columns {
                        if column == from {
                            *column = to.to_string();
                            changed = true;
                        }
                    }
                }
            }
            if changed {
                self.persist_constraint_candidate(
                    &candidate_name,
                    &table,
                    &columns,
                    &checks,
                    &foreign_keys,
                    &key_constraints,
                )?;
                updates.push((table, columns, checks, foreign_keys));
            }
        }
        for (table, columns, checks, foreign_keys) in updates {
            *table.columns.write() = columns;
            *table.table_checks.write() = checks;
            *table.foreign_keys.write() = foreign_keys;
        }
        Ok(())
    }

    fn preflight_drop_column_dependencies(
        &self,
        table_name: &str,
        column: &str,
    ) -> StorageBackendResult<()> {
        self.ensure_no_dependent_views("ALTER TABLE DROP COLUMN", table_name)?;
        let target = Self::resolved_relation_identity(table_name)?;
        let entries = self.table_entries();
        let target_state = entries
            .iter()
            .find(|(name, _)| name == table_name)
            .map(|(_, state)| state)
            .ok_or_else(|| table_not_found(table_name))?;

        for candidate in target_state.columns.read().iter() {
            if candidate.name == column {
                continue;
            }
            if candidate
                .default
                .as_ref()
                .is_some_and(|expr| schema_expr_references_column(expr, column))
                || candidate
                    .check
                    .as_ref()
                    .is_some_and(|expr| schema_expr_references_column(expr, column))
            {
                return Err(StorageBackendError::Other(format!(
                    "ALTER TABLE DROP COLUMN `{table_name}`.`{column}` rejected: column `{}` has a dependent DEFAULT/CHECK expression",
                    candidate.name
                )));
            }
        }
        if target_state
            .table_checks
            .read()
            .iter()
            .any(|check| schema_expr_references_column(&check.expr, column))
        {
            return Err(StorageBackendError::Other(format!(
                "ALTER TABLE DROP COLUMN `{table_name}`.`{column}` rejected: a CHECK constraint depends on the column"
            )));
        }

        let mut inbound = Vec::new();
        for (candidate_name, table) in &entries {
            for foreign_key in table.foreign_keys.read().iter() {
                let local_dependency = candidate_name == table_name
                    && (foreign_key.local_columns.iter().any(|name| name == column)
                        || foreign_key
                            .on_delete_set_columns
                            .iter()
                            .any(|name| name == column));
                let referenced_dependency = Self::foreign_key_targets(foreign_key, &target)
                    && foreign_key.ref_columns.iter().any(|name| name == column);
                if referenced_dependency && !local_dependency {
                    inbound.push(candidate_name.clone());
                }
            }
            for candidate in table.columns.read().iter() {
                if candidate_name == table_name && candidate.name == column {
                    continue;
                }
                if candidate.references.as_ref().is_some_and(|reference| {
                    stored_relation_reference_matches(&reference.table, &target)
                        && reference.column == column
                }) {
                    inbound.push(candidate_name.clone());
                }
            }
        }
        inbound.sort_unstable();
        inbound.dedup();
        if !inbound.is_empty() {
            return Err(StorageBackendError::Other(format!(
                "ALTER TABLE DROP COLUMN `{table_name}`.`{column}` rejected: referenced by foreign key(s) on `{}`",
                inbound.join("`, `")
            )));
        }
        // Parse every owned index before any mutation so malformed catalog
        // metadata cannot turn a failed drop into a partial in-memory change.
        for row in self.catalog_indexes.read().values() {
            if row.table_name == table_name {
                let _ = Self::catalog_index_references_column(row, column)?;
            }
        }
        Ok(())
    }

    fn ivf_catalog_params_for_column(
        &self,
        table: &str,
        column: &str,
    ) -> StorageBackendResult<Option<IVFIndexParams>> {
        for row in self.catalog_indexes.read().values() {
            let is_vector_index = row.index_type.eq_ignore_ascii_case("ivf")
                || row.index_type.eq_ignore_ascii_case("hnsw");
            if row.table_name == table
                && is_vector_index
                && Self::catalog_index_references_column(row, column)?
            {
                let parameters: BTreeMap<String, String> =
                    serde_json::from_str(&row.parameters_json)
                        .map_err(StorageBackendError::from)?;
                return Ok(Some(IVFIndexParams::from_catalog_map(&parameters)?));
            }
        }
        Ok(None)
    }

    fn vector_catalog_index_names_for_column(
        &self,
        table: &str,
        column: &str,
    ) -> StorageBackendResult<Vec<String>> {
        let mut names = Vec::new();
        for row in self.catalog_indexes.read().values() {
            if row.table_name == table
                && (row.index_type.eq_ignore_ascii_case("ivf")
                    || row.index_type.eq_ignore_ascii_case("hnsw"))
                && Self::catalog_index_references_column(row, column)?
            {
                names.push(row.name.clone());
            }
        }
        Ok(names)
    }

    pub(crate) fn rebind_persistent_table_stores(
        &self,
        table_name: &str,
        table: &TableState,
    ) -> StorageBackendResult<()> {
        let Some(backend) = self.backend.as_ref() else {
            return Ok(());
        };
        let analyzer = table.analyzer.read().clone();
        *table.document_store.write() = backend.document_store(table_name);
        table
            .doc_count_dirty
            .store(true, std::sync::atomic::Ordering::Release);
        Self::value_indexes_clear(table);
        *table.inverted_index.write() = backend.inverted_index(table_name, analyzer);

        let analyzer_rows: Vec<(String, String, String)> = self
            .table_field_analyzers
            .read()
            .iter()
            .filter(|((table, _), _)| table == table_name)
            .map(|((_, field), (analyzer, phase))| (field.clone(), analyzer.clone(), phase.clone()))
            .collect();
        for (field, analyzer_name, phase) in analyzer_rows {
            let analyzer = self
                .resolve_analyzer(&analyzer_name)
                .map_err(StorageBackendError::Other)?;
            let phase = if phase.eq_ignore_ascii_case("index") {
                AnalyzerPhase::Index
            } else if phase.eq_ignore_ascii_case("search") {
                AnalyzerPhase::Search
            } else {
                AnalyzerPhase::Both
            };
            table
                .inverted_index
                .write()
                .set_field_analyzer(&field, analyzer, phase)
                .map_err(StorageBackendError::Other)?;
        }

        let vector_fields: Vec<(String, u32)> = table
            .vector_indexes
            .read()
            .iter()
            .map(|(field, idx)| (field.clone(), idx.dimensions()))
            .collect();
        let mut rebound = BTreeMap::new();
        for (field, dimensions) in vector_fields {
            let idx = if let Some(params) =
                self.ivf_catalog_params_for_column(table_name, &field)?
            {
                self.build_vector_index_for_restore(table_name, &field, dimensions, params)
            } else {
                self.build_vector_index_with_initialize(table_name, &field, dimensions, None, false)
            };
            rebound.insert(field, idx);
        }
        *table.vector_indexes.write() = rebound;
        Ok(())
    }

    pub(crate) fn field_index_vectors(
        table: &TableState,
        field: &str,
        value: &Value,
    ) -> Result<Option<Vec<Vec<f32>>>, SQLError> {
        if matches!(value, Value::Null) {
            return Ok(None);
        }
        let ty = table
            .columns
            .read()
            .iter()
            .find(|column| column.name == field)
            .map(|column| column.ty.clone());
        match ty {
            Some(uqa_sql::ast::ColumnType::Tensor(dim)) => {
                let tensor = uqa_sql::expr::value_to_tensor(value)?;
                for vector in &tensor {
                    crate::sql::validate_vector_dimensions(dim, vector.len())?;
                }
                Ok(Some(tensor))
            }
            Some(uqa_sql::ast::ColumnType::Vector(dim)) => {
                let vector = uqa_sql::expr::value_to_vector(value)?;
                crate::sql::validate_vector_dimensions(dim, vector.len())?;
                Ok(Some(vec![vector]))
            }
            _ => Ok(Some(vec![uqa_sql::expr::value_to_vector(value)?])),
        }
    }

    /// Derive a complete replacement snapshot for every registered vector
    /// field from the document. Missing/NULL fields are represented by an
    /// empty vector list so replacement clears stale index entries instead of
    /// accidentally preserving them.
    fn document_vector_values(
        table: &TableState,
        document: &Document,
    ) -> Result<BTreeMap<FieldName, Vec<Vec<f32>>>, SQLError> {
        let fields = table
            .vector_indexes
            .read()
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        let mut vectors = BTreeMap::new();
        for field in fields {
            let values = match document.get(&field) {
                Some(value) => Self::field_index_vectors(table, &field, value)?.unwrap_or_default(),
                None => Vec::new(),
            };
            vectors.insert(field, values);
        }
        Ok(vectors)
    }

    /// Drop a table from the catalog and release its in-memory state.
    /// Returns `true` if the table existed.
    pub fn drop_table(&self, name: &str) -> StorageBackendResult<bool> {
        self.try_drop_table(name)
    }

    pub(crate) fn try_drop_table(&self, name: &str) -> StorageBackendResult<bool> {
        self.with_implicit_storage_transaction(|engine| {
            let Some(name) = engine.resolve_table_ddl_target(name, "DROP TABLE")? else {
                return Ok(false);
            };
            engine.try_drop_tables_inner(&[name], false)?;
            Ok(true)
        })
    }

    pub(crate) fn try_drop_tables(
        &self,
        names: &[String],
        cascade: bool,
    ) -> StorageBackendResult<()> {
        self.with_implicit_storage_transaction(|engine| {
            engine.try_drop_tables_inner(names, cascade)
        })
    }

    fn canonical_drop_table_names(&self, names: &[String]) -> StorageBackendResult<Vec<String>> {
        let mut canonical_names = Vec::with_capacity(names.len());
        for name in names {
            canonical_names.push(
                self.resolve_table_ddl_target(name, "DROP TABLE")?
                    .ok_or_else(|| table_not_found(name))?,
            );
        }
        canonical_names.sort_unstable();
        canonical_names.dedup();
        Ok(canonical_names)
    }

    fn try_drop_tables_inner(&self, names: &[String], cascade: bool) -> StorageBackendResult<()> {
        let canonical_names = self.canonical_drop_table_names(names)?;
        let targets = canonical_names
            .iter()
            .map(|name| Self::resolved_relation_identity(name))
            .collect::<StorageBackendResult<Vec<_>>>()?;
        let target_names = canonical_names
            .iter()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();

        // Finish every dependency check before mutating a referrer or target.
        for name in &canonical_names {
            self.ensure_no_dependent_views("DROP TABLE", name)?;
        }
        let entries = self.table_entries();
        for (candidate_name, table) in &entries {
            if target_names.contains(candidate_name) {
                continue;
            }
            if let Some(target) = targets
                .iter()
                .find(|target| Self::table_schema_references_relation(table, target))
            {
                return Err(StorageBackendError::Other(format!(
                    "DROP TABLE `{}` rejected: schema expression on `{candidate_name}` may depend on it and cannot be rewritten safely",
                    target.qualified_name()
                )));
            }
        }

        let mut inbound = Vec::new();
        let mut updates = Vec::new();
        for (candidate_name, table) in entries {
            if target_names.contains(&candidate_name) {
                continue;
            }
            let mut columns = table.columns.read().clone();
            let checks = table.table_checks.read().clone();
            let mut foreign_keys = table.foreign_keys.read().clone();
            let key_constraints = table.key_constraints.read().clone();
            let previous_fk_len = foreign_keys.len();
            foreign_keys.retain(|foreign_key| {
                !targets
                    .iter()
                    .any(|target| Self::foreign_key_targets(foreign_key, target))
            });
            let mut changed = previous_fk_len != foreign_keys.len();
            for column in &mut columns {
                if column.references.as_ref().is_some_and(|reference| {
                    targets
                        .iter()
                        .any(|target| stored_relation_reference_matches(&reference.table, target))
                }) {
                    column.references = None;
                    changed = true;
                }
            }
            if changed {
                inbound.push(candidate_name.clone());
                updates.push((
                    candidate_name,
                    table,
                    columns,
                    checks,
                    foreign_keys,
                    key_constraints,
                ));
            }
        }
        if !cascade && !inbound.is_empty() {
            inbound.sort_unstable();
            inbound.dedup();
            return Err(StorageBackendError::Other(format!(
                "DROP TABLE rejected: still referenced by foreign key(s) on `{}`; use CASCADE",
                inbound.join("`, `")
            )));
        }
        if cascade {
            for (name, table, columns, checks, foreign_keys, key_constraints) in &updates {
                self.persist_constraint_candidate(
                    name,
                    table,
                    columns,
                    checks,
                    foreign_keys,
                    key_constraints,
                )?;
            }
            for (_, table, columns, checks, foreign_keys, _) in updates {
                *table.columns.write() = columns;
                *table.table_checks.write() = checks;
                *table.foreign_keys.write() = foreign_keys;
            }
        }
        for name in canonical_names {
            self.drop_table_state_inner(&name)?;
        }
        Ok(())
    }

    fn drop_table_state_inner(&self, name: &str) -> StorageBackendResult<()> {
        let relation = Self::resolved_relation_identity(name)?;
        if !self.tables.read().contains_key(&relation) {
            return Err(table_not_found(name));
        }
        if let Some(catalog) = self.catalog.as_ref() {
            catalog.drop_table_and_data(name)?;
            self.note_table_catalog_changed();
        }
        self.tables.write().remove(&relation);
        // Sweep every related per-table registry so catalog state
        // does not outlive the table.
        self.table_field_analyzers
            .write()
            .retain(|(t, _), _| t != name);
        self.catalog_indexes
            .write()
            .retain(|_, row| row.table_name != name);
        Ok(())
    }

    pub fn has_table(&self, name: &str) -> StorageBackendResult<bool> {
        self.try_has_table(name)
    }

    pub fn try_has_table(&self, name: &str) -> StorageBackendResult<bool> {
        Ok(self.try_resolve_table_name(name)?.is_some())
    }

    /// All schema-declared columns for `table`, in declaration order.
    pub fn table_columns(&self, table: &str) -> StorageBackendResult<Vec<String>> {
        self.try_table_columns(table)
    }

    pub fn try_table_columns(&self, table: &str) -> StorageBackendResult<Vec<String>> {
        let table_state = self
            .try_table(table)?
            .ok_or_else(|| table_not_found(table))?;
        let columns = table_state
            .columns
            .read()
            .iter()
            .map(|column| column.name.clone())
            .collect();
        Ok(columns)
    }

    pub fn table_has_column(&self, table: &str, column: &str) -> StorageBackendResult<bool> {
        self.try_table_has_column(table, column)
    }

    pub fn try_table_has_column(&self, table: &str, column: &str) -> StorageBackendResult<bool> {
        let t = self
            .try_table(table)?
            .ok_or_else(|| table_not_found(table))?;
        let cols = t.columns.read();
        Ok(cols.iter().any(|c| c.name == column))
    }

    pub(crate) fn column_type(
        &self,
        table: &str,
        column: &str,
    ) -> StorageBackendResult<Option<uqa_sql::ast::ColumnType>> {
        let t = self
            .try_table(table)?
            .ok_or_else(|| table_not_found(table))?;
        let cols = t.columns.read();
        Ok(cols.iter().find(|c| c.name == column).map(|c| c.ty.clone()))
    }

    /// Return the SERIAL/BIGSERIAL column name for `table`, if any.
    pub(crate) fn auto_increment_column(
        &self,
        table: &str,
    ) -> StorageBackendResult<Option<String>> {
        let t = self
            .try_table(table)?
            .ok_or_else(|| table_not_found(table))?;
        let cols = t.columns.read();
        Ok(cols
            .iter()
            .find(|c| c.auto_increment)
            .map(|c| c.name.clone()))
    }

    /// Sorted list of every registered table name.
    pub fn table_names(&self) -> StorageBackendResult<Vec<String>> {
        self.synchronize_table_catalog()?;
        Ok(self
            .tables
            .read()
            .keys()
            .map(RelationIdentity::qualified_name)
            .collect())
    }

    /// Snapshot the column schema of `table`. Returns `None` when no
    /// table by that name is registered.
    pub fn describe_table(
        &self,
        table: &str,
    ) -> StorageBackendResult<Option<Vec<uqa_sql::ast::ColumnDef>>> {
        self.try_describe_table(table)
    }

    pub fn try_describe_table(
        &self,
        table: &str,
    ) -> StorageBackendResult<Option<Vec<uqa_sql::ast::ColumnDef>>> {
        let Some(table) = self.try_table(table)? else {
            return Ok(None);
        };
        let mut columns = table.columns.read().clone();
        for column in &mut columns {
            if let Some(default) = &mut column.default {
                self.resolve_stored_sequence_references_in_expr(default)?;
            }
        }
        Ok(Some(columns))
    }

    /// DEFAULT expression for `column` on `table`, when one was
    /// declared via `... <col> <type> DEFAULT <expr>`.
    pub fn column_default_expr(
        &self,
        table: &str,
        column: &str,
    ) -> StorageBackendResult<Option<uqa_sql::ast::Expr>> {
        self.try_column_default_expr(table, column)
    }

    pub fn try_column_default_expr(
        &self,
        table: &str,
        column: &str,
    ) -> StorageBackendResult<Option<uqa_sql::ast::Expr>> {
        let t = self
            .try_table(table)?
            .ok_or_else(|| table_not_found(table))?;
        let cols = t.columns.read();
        let mut default = cols
            .iter()
            .find(|c| c.name == column)
            .and_then(|c| c.default.clone());
        drop(cols);
        if let Some(default) = &mut default {
            self.resolve_stored_sequence_references_in_expr(default)?;
        }
        Ok(default)
    }

    pub fn set_column_default(
        &self,
        table: &str,
        column: &str,
        default: Option<uqa_sql::ast::Expr>,
    ) -> StorageBackendResult<bool> {
        self.with_implicit_storage_transaction(|engine| {
            engine.set_column_default_inner(table, column, default)
        })
    }

    fn set_column_default_inner(
        &self,
        table: &str,
        column: &str,
        mut default: Option<uqa_sql::ast::Expr>,
    ) -> StorageBackendResult<bool> {
        let table_name = self
            .try_resolve_table_name(table)?
            .ok_or_else(|| table_not_found(table))?;
        let t = self
            .try_table(&table_name)?
            .ok_or_else(|| table_not_found(&table_name))?;
        if let Some(default) = &mut default {
            self.bind_sequence_references_in_expr(default)?;
        }
        let mut columns = t.columns.write();
        let mut next = columns.clone();
        let col = next
            .iter_mut()
            .find(|col| col.name == column)
            .ok_or_else(|| column_not_found(&table_name, column))?;
        col.default = default;
        self.mark_column_stats_dirty(&table_name, &t)?;
        if self.is_persistent() {
            self.try_save_table_schema_with_columns(&table_name, &t, &next)?;
        }
        *columns = next;
        Ok(true)
    }

    pub fn set_column_not_null(
        &self,
        table: &str,
        column: &str,
        not_null: bool,
    ) -> StorageBackendResult<bool> {
        self.with_implicit_storage_transaction(|engine| {
            engine.set_column_not_null_inner(table, column, not_null)
        })
    }

    fn set_column_not_null_inner(
        &self,
        table: &str,
        column: &str,
        not_null: bool,
    ) -> StorageBackendResult<bool> {
        let table_name = self
            .try_resolve_table_name(table)?
            .ok_or_else(|| table_not_found(table))?;
        let t = self
            .try_table(&table_name)?
            .ok_or_else(|| table_not_found(&table_name))?;
        let mut columns = t.columns.write();
        let mut next = columns.clone();
        let col = next
            .iter_mut()
            .find(|col| col.name == column)
            .ok_or_else(|| column_not_found(&table_name, column))?;
        col.not_null = not_null;
        self.mark_column_stats_dirty(&table_name, &t)?;
        if self.is_persistent() {
            self.try_save_table_schema_with_columns(&table_name, &t, &next)?;
        }
        *columns = next;
        Ok(true)
    }

    pub fn set_column_type(
        &self,
        table: &str,
        column: &str,
        ty: &uqa_sql::ast::ColumnType,
    ) -> StorageBackendResult<bool> {
        self.with_implicit_storage_transaction(|engine| {
            engine.set_column_type_inner(table, column, ty)
        })
    }

    fn set_column_type_inner(
        &self,
        table: &str,
        column: &str,
        ty: &uqa_sql::ast::ColumnType,
    ) -> StorageBackendResult<bool> {
        let table_name = self
            .try_resolve_table_name(table)?
            .ok_or_else(|| table_not_found(table))?;
        let t = self
            .try_table(&table_name)?
            .ok_or_else(|| table_not_found(&table_name))?;
        let mut columns = t.columns.write();
        let mut next = columns.clone();
        let col = next
            .iter_mut()
            .find(|col| col.name == column)
            .ok_or_else(|| column_not_found(&table_name, column))?;
        col.ty.clone_from(ty);
        self.mark_column_stats_dirty(&table_name, &t)?;
        if self.is_persistent() {
            self.try_save_table_schema_with_columns(&table_name, &t, &next)?;
        }
        *columns = next;
        Ok(true)
    }

    /// Register table-level CHECK, FK, PRIMARY KEY, and UNIQUE constraints. Called by the
    /// SQL `CREATE TABLE` path after the columns are in place.
    pub fn register_table_constraints(
        &self,
        table: &str,
        checks: Vec<uqa_sql::ast::TableCheck>,
        foreign_keys: Vec<uqa_sql::ast::ForeignKey>,
        key_constraints: Vec<uqa_sql::ast::TableKeyConstraint>,
    ) -> StorageBackendResult<()> {
        self.with_implicit_storage_transaction(|engine| {
            engine.register_table_constraints_inner(table, checks, foreign_keys, key_constraints)
        })
    }

    fn register_table_constraints_inner(
        &self,
        table: &str,
        checks: Vec<uqa_sql::ast::TableCheck>,
        mut foreign_keys: Vec<uqa_sql::ast::ForeignKey>,
        key_constraints: Vec<uqa_sql::ast::TableKeyConstraint>,
    ) -> StorageBackendResult<()> {
        let Some(table_name) = self.try_resolve_table_name(table)? else {
            return Err(StorageBackendError::Other(format!(
                "unknown table `{table}` while registering constraints"
            )));
        };
        let Some(t) = self.try_table(&table_name)? else {
            return Err(StorageBackendError::Other(format!(
                "unknown table `{table_name}` while registering constraints"
            )));
        };
        for foreign_key in &mut foreign_keys {
            foreign_key.ref_table = self.canonical_foreign_key_target(&foreign_key.ref_table)?;
        }
        let constraints = uqa_sql::ast::TableConstraintSet {
            checks,
            foreign_keys,
            key_constraints,
        };
        if self.is_persistent() {
            let columns = t.columns.read().clone();
            self.try_save_table_schema_with_components(&table_name, &t, &columns, &constraints)?;
        }
        *t.table_checks.write() = constraints.checks;
        *t.foreign_keys.write() = constraints.foreign_keys;
        *t.key_constraints.write() = constraints.key_constraints;
        Ok(())
    }

    /// Snapshot of every CHECK constraint that applies to `table`,
    /// merging the column-level CHECKs into the table-level list.
    /// Returns `(name, expr)` pairs where `name` is the constraint
    /// name when one was supplied (synthesised as `<col>_check` for
    /// column-level constraints).
    pub fn check_constraints(
        &self,
        table: &str,
    ) -> StorageBackendResult<Vec<(Option<String>, uqa_sql::ast::Expr)>> {
        self.try_check_constraints(table)
    }

    pub fn try_check_constraints(
        &self,
        table: &str,
    ) -> StorageBackendResult<Vec<(Option<String>, uqa_sql::ast::Expr)>> {
        let t = self
            .try_table(table)?
            .ok_or_else(|| table_not_found(table))?;
        let mut out: Vec<(Option<String>, uqa_sql::ast::Expr)> = Vec::new();
        for col in t.columns.read().iter() {
            if let Some(expr) = col.check.clone() {
                out.push((Some(format!("{}_check", col.name)), expr));
            }
        }
        for c in t.table_checks.read().iter() {
            out.push((c.name.clone(), c.expr.clone()));
        }
        Ok(out)
    }

    /// Snapshot of every FOREIGN KEY constraint that applies to
    /// `table`. Column-level `REFERENCES` are lifted to single-column
    /// `ForeignKey` entries.
    pub fn foreign_keys(&self, table: &str) -> StorageBackendResult<Vec<uqa_sql::ast::ForeignKey>> {
        self.try_foreign_keys(table)
    }

    pub fn try_foreign_keys(
        &self,
        table: &str,
    ) -> StorageBackendResult<Vec<uqa_sql::ast::ForeignKey>> {
        let t = self
            .try_table(table)?
            .ok_or_else(|| table_not_found(table))?;
        let mut out: Vec<uqa_sql::ast::ForeignKey> = t.foreign_keys.read().clone();
        for col in t.columns.read().iter() {
            if let Some(reference) = col.references.clone() {
                out.push(uqa_sql::ast::ForeignKey {
                    name: Some(format!("{}_fkey", col.name)),
                    local_columns: vec![col.name.clone()],
                    ref_table: reference.table,
                    ref_columns: vec![reference.column],
                    on_update: reference.on_update,
                    on_delete: reference.on_delete,
                    on_delete_set_columns: Vec::new(),
                    match_type: reference.match_type,
                });
            }
        }
        for foreign_key in &mut out {
            foreign_key.ref_table =
                self.canonical_stored_foreign_key_target(&foreign_key.ref_table)?;
        }
        Ok(out)
    }

    /// Tables that hold a FOREIGN KEY pointing at `table`. Used by
    /// DELETE / DROP CASCADE to refuse the operation when a referrer
    /// has at least one row matching the target value.
    pub fn referrers_to(
        &self,
        table: &str,
    ) -> StorageBackendResult<Vec<(String, uqa_sql::ast::ForeignKey)>> {
        self.try_referrers_to(table)
    }

    pub fn try_referrers_to(
        &self,
        table: &str,
    ) -> StorageBackendResult<Vec<(String, uqa_sql::ast::ForeignKey)>> {
        let table = self
            .try_resolve_table_name(table)?
            .ok_or_else(|| table_not_found(table))?;
        let target = Self::resolved_relation_identity(&table)?;
        self.try_table(&table)?
            .ok_or_else(|| table_not_found(&table))?;
        let mut out: Vec<(String, uqa_sql::ast::ForeignKey)> = Vec::new();
        let names: Vec<String> = self
            .tables
            .read()
            .keys()
            .map(RelationIdentity::qualified_name)
            .collect();
        for other in names {
            for fk in self.try_foreign_keys(&other)? {
                if Self::foreign_key_targets(&fk, &target) {
                    out.push((other.clone(), fk));
                }
            }
        }
        Ok(out)
    }

    /// Names of columns with a `UNIQUE` or `PRIMARY KEY` constraint
    /// declared on the table. Auto-increment columns are excluded
    /// because the engine guarantees their uniqueness through the
    /// monotonic id watermark, so re-checking is redundant.
    pub fn unique_columns(&self, table: &str) -> StorageBackendResult<Vec<String>> {
        self.try_unique_columns(table)
    }

    pub fn try_unique_columns(&self, table: &str) -> StorageBackendResult<Vec<String>> {
        let t = self
            .try_table(table)?
            .ok_or_else(|| table_not_found(table))?;
        let cols = t.columns.read();
        let auto_increment: std::collections::BTreeSet<String> = cols
            .iter()
            .filter(|column| column.auto_increment)
            .map(|column| column.name.clone())
            .collect();
        drop(cols);
        Ok(self
            .try_key_constraints(table)?
            .into_iter()
            .filter(|constraint| constraint.columns.len() == 1)
            .map(|constraint| constraint.columns[0].clone())
            .filter(|column| !auto_increment.contains(column))
            .collect())
    }

    /// Every PRIMARY KEY / UNIQUE tuple declared on `table`. Legacy
    /// column metadata is lifted into scalar constraints so pre-v16 and API-
    /// created tables retain their existing behavior.
    pub fn key_constraints(
        &self,
        table: &str,
    ) -> StorageBackendResult<Vec<uqa_sql::ast::TableKeyConstraint>> {
        self.try_key_constraints(table)
    }

    pub fn try_key_constraints(
        &self,
        table: &str,
    ) -> StorageBackendResult<Vec<uqa_sql::ast::TableKeyConstraint>> {
        let t = self
            .try_table(table)?
            .ok_or_else(|| table_not_found(table))?;
        let mut constraints = t.key_constraints.read().clone();
        for column in t.columns.read().iter() {
            let kind = if column.primary_key {
                Some(uqa_sql::ast::TableKeyConstraintKind::PrimaryKey)
            } else if column.unique {
                Some(uqa_sql::ast::TableKeyConstraintKind::Unique)
            } else {
                None
            };
            let Some(kind) = kind else {
                continue;
            };
            if constraints.iter().any(|constraint| {
                constraint.kind == kind
                    && constraint.columns.as_slice() == std::slice::from_ref(&column.name)
            }) {
                continue;
            }
            constraints.push(uqa_sql::ast::TableKeyConstraint {
                name: None,
                kind,
                columns: vec![column.name.clone()],
                nulls_not_distinct: false,
            });
        }
        Ok(constraints)
    }

    /// Allocate the next id from the per-table watermark, returning the
    /// allocated value. Updates the watermark in place.
    pub(crate) fn allocate_next_id(&self, table: &str) -> Result<u64, SQLError> {
        let t = self
            .try_table(table)
            .map_err(|error| SQLError::Internal(format!("resolve table `{table}`: {error}")))?
            .ok_or_else(|| SQLError::Internal(format!("unknown table `{table}`")))?;
        let mut g = t.next_id.lock();
        let id = u64::try_from(*g).map_err(|_| {
            SQLError::Internal(format!(
                "document id space for table `{table}` is exhausted"
            ))
        })?;
        *g += 1;
        Ok(id)
    }

    /// Move the watermark past `doc_id` if needed (called after a manual
    /// id assignment so the next allocation does not collide).
    pub(crate) fn advance_next_id(&self, table: &str, doc_id: DocId) -> StorageBackendResult<()> {
        let t = self
            .try_table(table)?
            .ok_or_else(|| table_not_found(table))?;
        let mut g = t.next_id.lock();
        let next = u128::from(doc_id) + 1;
        if next > *g {
            *g = next;
        }
        Ok(())
    }

    /// Append a column to the schema. No data migration is needed because
    /// the document store is sparse; rows missing the column read back as
    /// `Value::Null`.
    pub fn register_column(
        &self,
        table: &str,
        column: uqa_sql::ast::ColumnDef,
    ) -> StorageBackendResult<()> {
        self.try_register_column(table, column)
    }

    pub(crate) fn try_register_column(
        &self,
        table: &str,
        column: uqa_sql::ast::ColumnDef,
    ) -> StorageBackendResult<()> {
        self.with_implicit_storage_transaction(|engine| {
            engine.try_register_column_inner(table, column)
        })
    }

    fn try_register_column_inner(
        &self,
        table: &str,
        mut column: uqa_sql::ast::ColumnDef,
    ) -> StorageBackendResult<()> {
        let table_name = self
            .try_resolve_table_name(table)?
            .ok_or_else(|| table_not_found(table))?;
        let t = self
            .try_table(&table_name)?
            .ok_or_else(|| table_not_found(&table_name))?;
        if let Some(default) = &mut column.default {
            self.bind_sequence_references_in_expr(default)?;
        }
        if let Some(reference) = &mut column.references {
            reference.table = self.canonical_foreign_key_target(&reference.table)?;
        }
        let mut columns = t.columns.write();
        if columns.iter().any(|c| c.name == column.name) {
            return Err(StorageBackendError::Other(format!(
                "column `{}` already exists on table `{table_name}`",
                column.name
            )));
        }
        let mut next = columns.clone();
        next.push(column);
        self.mark_column_stats_dirty(&table_name, &t)?;
        if self.is_persistent() {
            self.try_save_table_schema_with_columns(&table_name, &t, &next)?;
        }
        *columns = next;
        drop(columns);
        self.refresh_value_indexes_for_table(&table_name)?;
        Ok(())
    }

    pub fn drop_column(&self, table: &str, column: &str) -> StorageBackendResult<bool> {
        self.try_drop_column(table, column)
    }

    pub(crate) fn try_drop_column(&self, table: &str, column: &str) -> StorageBackendResult<bool> {
        self.with_implicit_storage_transaction(|engine| engine.try_drop_column_inner(table, column))
    }

    fn try_drop_column_inner(&self, table: &str, column: &str) -> StorageBackendResult<bool> {
        let Some(table_name) = self.resolve_table_ddl_target(table, "ALTER TABLE DROP COLUMN")?
        else {
            return Ok(false);
        };
        let Some(t) = self.try_table(table)? else {
            return Ok(false);
        };
        if !t
            .columns
            .read()
            .iter()
            .any(|candidate| candidate.name == column)
        {
            return Ok(false);
        }
        self.preflight_drop_column_dependencies(&table_name, column)?;
        Self::value_indexes_clear(&t);
        {
            let mut cols = t.columns.write();
            cols.retain(|c| c.name != column);
        }
        t.key_constraints
            .write()
            .retain(|constraint| !constraint.columns.iter().any(|name| name == column));
        t.foreign_keys.write().retain(|foreign_key| {
            !foreign_key.local_columns.iter().any(|name| name == column)
                && !foreign_key
                    .on_delete_set_columns
                    .iter()
                    .any(|name| name == column)
        });
        // Remove from FTS field list if present.
        {
            let mut fts = t.fts_fields.write();
            fts.retain(|f| f != column);
        }
        // Drop the vector index for this field if it exists.
        {
            let mut vs = t.vector_indexes.write();
            if let Some(mut idx) = vs.remove(column) {
                idx.clear()?;
            }
        }
        self.remove_catalog_indexes_for_column(&table_name, column)?;
        self.table_field_analyzers
            .write()
            .retain(|(table, field), _| !(table == &table_name && field == column));
        let ids = t.document_store.read().doc_ids()?;
        for doc_id in ids {
            let Some(mut doc) = t.document_store.read().get(doc_id)? else {
                continue;
            };
            if doc.remove(column).is_some() {
                self.rewrite_document_for_schema_change(&table_name, doc_id, doc)
                    .map_err(|err| StorageBackendError::Other(err.to_string()))?;
            }
        }
        if self.is_persistent() {
            if let Some(catalog) = self.catalog.as_ref() {
                catalog.drop_column_data(&table_name, column)?;
            }
            self.try_save_table_schema(&table_name, &t)?;
        }
        self.mark_column_stats_dirty(&table_name, &t)?;
        self.refresh_value_indexes_for_table(&table_name)?;
        Ok(true)
    }

    pub(crate) fn try_drop_vector_indexes_for_column(
        &self,
        table: &str,
        column: &str,
    ) -> StorageBackendResult<bool> {
        self.with_implicit_storage_transaction(|engine| {
            engine.try_drop_vector_indexes_for_column_inner(table, column)
        })
    }

    fn try_drop_vector_indexes_for_column_inner(
        &self,
        table: &str,
        column: &str,
    ) -> StorageBackendResult<bool> {
        let Some(table_name) = self.resolve_table_ddl_target(table, "ALTER TABLE ALTER COLUMN")?
        else {
            return Ok(false);
        };
        let Some(t) = self.try_table(table)? else {
            return Ok(false);
        };
        if let Some(mut idx) = t.vector_indexes.write().remove(column) {
            idx.clear()?;
        }
        for index_name in self.vector_catalog_index_names_for_column(&table_name, column)? {
            self.try_drop_catalog_index(&index_name)?;
        }
        self.try_save_table_schema(&table_name, &t)?;
        Ok(true)
    }

    pub(crate) fn try_rebuild_vector_index_for_column(
        &self,
        table: &str,
        column: &str,
        dimensions: u32,
    ) -> StorageBackendResult<bool> {
        self.with_implicit_storage_transaction(|engine| {
            engine.try_rebuild_vector_index_for_column_inner(table, column, dimensions)
        })
    }

    fn try_rebuild_vector_index_for_column_inner(
        &self,
        table: &str,
        column: &str,
        dimensions: u32,
    ) -> StorageBackendResult<bool> {
        let table_name = self
            .try_resolve_table_name(table)?
            .ok_or_else(|| table_not_found(table))?;
        let params = self.ivf_catalog_params_for_column(&table_name, column)?;
        let rebuilt = if let Some(params) = params {
            self.rebuild_ivf_vector_field(&table_name, column, dimensions, params)
        } else {
            self.rebuild_vector_field(&table_name, column, dimensions)
        }?;
        if !rebuilt {
            return Err(StorageBackendError::Other(format!(
                "failed to rebuild vector index for `{table_name}`.`{column}`"
            )));
        }
        let t = self
            .try_table(&table_name)?
            .ok_or_else(|| table_not_found(&table_name))?;
        self.try_save_table_schema(&table_name, &t)?;
        Ok(true)
    }

    pub fn rename_column(&self, table: &str, from: &str, to: &str) -> StorageBackendResult<bool> {
        self.try_rename_column(table, from, to)
    }

    pub(crate) fn try_rename_column(
        &self,
        table: &str,
        from: &str,
        to: &str,
    ) -> StorageBackendResult<bool> {
        self.with_implicit_storage_transaction(|engine| {
            engine.try_rename_column_inner(table, from, to)
        })
    }

    fn try_rename_column_inner(
        &self,
        table: &str,
        from: &str,
        to: &str,
    ) -> StorageBackendResult<bool> {
        let Some(table_name) = self.resolve_table_ddl_target(table, "ALTER TABLE RENAME COLUMN")?
        else {
            return Ok(false);
        };
        let Some(t) = self.try_table(table)? else {
            return Ok(false);
        };
        {
            let columns = t.columns.read();
            if !columns.iter().any(|candidate| candidate.name == from) {
                return Ok(false);
            }
            if from != to && columns.iter().any(|candidate| candidate.name == to) {
                return Ok(false);
            }
        }
        self.rewrite_column_rename_dependencies(&table_name, from, to)?;
        Self::value_indexes_clear(&t);
        {
            let mut cols = t.columns.write();
            for c in cols.iter_mut() {
                if c.name == from {
                    c.name = to.to_string();
                }
            }
        }
        for constraint in t.key_constraints.write().iter_mut() {
            for column in &mut constraint.columns {
                if column == from {
                    *column = to.to_string();
                }
            }
        }
        {
            let mut fts = t.fts_fields.write();
            for f in fts.iter_mut() {
                if f == from {
                    *f = to.to_string();
                }
            }
        }
        let vector_dimensions = {
            let mut vs = t.vector_indexes.write();
            if let Some(mut idx) = vs.remove(from) {
                let dimensions = idx.dimensions();
                idx.clear()?;
                Some(dimensions)
            } else {
                None
            }
        };
        let ids = t.document_store.read().doc_ids()?;
        for doc_id in ids {
            let Some(mut doc) = t.document_store.read().get(doc_id)? else {
                continue;
            };
            if let Some(value) = doc.remove(from) {
                doc.insert(to.to_string(), value);
                self.rewrite_document_for_schema_change(&table_name, doc_id, doc)
                    .map_err(|err| StorageBackendError::Other(err.to_string()))?;
            }
        }
        if let Some(dimensions) = vector_dimensions {
            self.create_vector_field(&table_name, to, dimensions)?;
        }
        self.rename_catalog_index_column_refs(&table_name, from, to)?;
        {
            let mut analyzers = self.table_field_analyzers.write();
            let mut moved = Vec::new();
            analyzers.retain(|(table, field), value| {
                if table == &table_name && field == from {
                    moved.push(((table_name.clone(), to.to_string()), value.clone()));
                    false
                } else {
                    true
                }
            });
            analyzers.extend(moved);
        }
        if self.is_persistent() {
            if let Some(catalog) = self.catalog.as_ref() {
                catalog.rename_column_data(&table_name, from, to)?;
            }
            if let Some(dimensions) = vector_dimensions {
                if let Some(params) = self.ivf_catalog_params_for_column(&table_name, to)? {
                    if !self.rebuild_ivf_vector_field(&table_name, to, dimensions, params)? {
                        return Err(StorageBackendError::Other(format!(
                            "failed to rebuild IVF index for `{table_name}`.`{to}`"
                        )));
                    }
                }
            }
            self.try_save_table_schema(&table_name, &t)?;
        }
        self.mark_column_stats_dirty(&table_name, &t)?;
        self.refresh_value_indexes_for_table(&table_name)?;
        Ok(true)
    }

    pub fn rename_table(&self, from: &str, to: &str) -> StorageBackendResult<bool> {
        self.try_rename_table(from, to)
    }

    pub(crate) fn try_rename_table(&self, from: &str, to: &str) -> StorageBackendResult<bool> {
        self.with_implicit_storage_transaction(|engine| engine.try_rename_table_inner(from, to))
    }

    fn try_rename_table_inner(&self, from: &str, to: &str) -> StorageBackendResult<bool> {
        let Some(from) = self.resolve_table_ddl_target(from, "ALTER TABLE RENAME")? else {
            return Ok(false);
        };
        let from_relation = Self::resolved_relation_identity(&from)?;
        let (target_schema, target_name) =
            RelationIdentity::parse_reference(to).map_err(StorageBackendError::Other)?;
        let to_relation = RelationIdentity::new(
            target_schema.unwrap_or_else(|| from_relation.schema.clone()),
            target_name,
        );
        if !self.schemas.read().contains(&to_relation.schema) {
            return Err(StorageBackendError::Other(format!(
                "schema `{}` does not exist",
                to_relation.schema
            )));
        }
        let to = to_relation.qualified_name();
        if let Some(kind) = self.relation_kind_at(&to)? {
            return Err(StorageBackendError::Other(format!(
                "relation `{to}` already exists as {kind}"
            )));
        }
        {
            let tables = self.tables.read();
            if !tables.contains_key(&from_relation) || tables.contains_key(&to_relation) {
                return Ok(false);
            }
        }
        self.rewrite_table_rename_dependencies(&from, &to)?;
        if self.is_persistent() {
            if let Some(catalog) = self.catalog.as_ref() {
                catalog.rename_table_data(&from, &to)?;
            }
        }
        let mut tables = self.tables.write();
        if tables.contains_key(&to_relation) {
            return Ok(false);
        }
        let Some(state) = tables.remove(&from_relation) else {
            return Ok(false);
        };
        tables.insert(to_relation, state.clone());
        drop(tables);
        self.rename_catalog_index_table_refs(&from, &to);
        {
            let mut analyzers = self.table_field_analyzers.write();
            let mut moved = Vec::new();
            analyzers.retain(|(table, field), value| {
                if table == &from {
                    moved.push(((to.clone(), field.clone()), value.clone()));
                    false
                } else {
                    true
                }
            });
            analyzers.extend(moved);
        }
        if self.is_persistent() {
            self.rebind_persistent_table_stores(&to, &state)?;
            self.try_save_table_schema(&to, &state)?;
        }
        self.mark_column_stats_dirty(&to, &state)?;
        self.refresh_value_indexes_for_table(&to)?;
        Ok(true)
    }

    /// Append `field` to the table's FTS field list. Existing rows are
    /// indexed immediately so SQL `CREATE INDEX USING gin` behaves like a
    /// real secondary-index build rather than a metadata-only toggle.
    pub fn add_fts_field(&self, table: &str, field: FieldName) -> Result<(), String> {
        self.add_fts_field_with_analyzer(table, field, None)
    }

    /// Same as [`Engine::add_fts_field`], but allows registering a
    /// per-field analyzer name (e.g. `standard_cjk`). When `None`, the
    /// table-level analyzer continues to apply.
    pub fn add_fts_field_with_analyzer(
        &self,
        table: &str,
        field: FieldName,
        analyzer: Option<&str>,
    ) -> Result<(), String> {
        self.with_implicit_string_transaction(|engine| {
            engine.add_fts_field_with_analyzer_inner(table, field, analyzer)
        })
    }

    fn add_fts_field_with_analyzer_inner(
        &self,
        table: &str,
        field: FieldName,
        analyzer: Option<&str>,
    ) -> Result<(), String> {
        let table_name = self
            .try_resolve_table_name(table)
            .map_err(|err| format!("resolve table `{table}`: {err}"))?
            .ok_or_else(|| format!("unknown table `{table}`"))?;
        let t = self
            .try_table(table)
            .map_err(|err| format!("resolve table `{table}`: {err}"))?
            .ok_or_else(|| format!("unknown table `{table}`"))?;
        if let Some(analyzer_name) = analyzer {
            let analyzer = self.resolve_analyzer(analyzer_name)?;
            t.inverted_index
                .write()
                .set_field_analyzer(&field, analyzer, AnalyzerPhase::Both)
                .map_err(|e| format!("add_fts_field: {e}"))?;
            self.table_field_analyzers.write().insert(
                (table_name.clone(), field.clone()),
                (analyzer_name.to_string(), "both".to_string()),
            );
            if let Some(catalog) = self.catalog.as_ref() {
                catalog
                    .replace_table_field_analyzer(&table_name, &field, "both", analyzer_name)
                    .map_err(|err| format!("persist FTS analyzer: {err}"))?;
            }
        }
        {
            let mut fts = t.fts_fields.write();
            if !fts.contains(&field) {
                fts.push(field);
            }
        }
        Self::rebuild_fts_index(&t)?;
        if self.is_persistent() {
            self.try_save_table_schema(&table_name, &t)
                .map_err(|err| format!("persist FTS schema `{table_name}`: {err}"))?;
        }
        Ok(())
    }

    /// Remove a field from the physical FTS index and from every piece of
    /// analyzer/schema metadata that makes the field searchable.  Callers
    /// must first establish that no other logical GIN index still references
    /// the field.
    pub(crate) fn drop_fts_field(&self, table: &str, field: &str) -> Result<(), String> {
        self.with_implicit_string_transaction(|engine| engine.drop_fts_field_inner(table, field))
    }

    fn drop_fts_field_inner(&self, table: &str, field: &str) -> Result<(), String> {
        let table_name = self
            .try_resolve_table_name(table)
            .map_err(|err| format!("resolve table `{table}`: {err}"))?
            .ok_or_else(|| format!("unknown table `{table}`"))?;
        let t = self
            .try_table(&table_name)
            .map_err(|err| format!("resolve table `{table_name}`: {err}"))?
            .ok_or_else(|| format!("unknown table `{table_name}`"))?;

        if !t
            .fts_fields
            .read()
            .iter()
            .any(|candidate| candidate == field)
        {
            return Err(format!(
                "field `{table_name}`.`{field}` is not registered in the physical FTS index"
            ));
        }

        t.inverted_index
            .write()
            .remove_field_analyzers(field)
            .map_err(|err| format!("remove FTS analyzer `{table_name}`.`{field}`: {err}"))?;
        t.fts_fields.write().retain(|candidate| candidate != field);
        Self::rebuild_fts_index(&t)
            .map_err(|err| format!("rebuild FTS index for `{table_name}`: {err}"))?;

        if let Some(catalog) = self.catalog.as_ref() {
            catalog
                .drop_table_field_analyzer_field(&table_name, field)
                .map_err(|err| {
                    format!("drop persisted FTS analyzer `{table_name}`.`{field}`: {err}")
                })?;
        }
        self.table_field_analyzers
            .write()
            .remove(&(table_name.clone(), field.to_string()));
        if self.is_persistent() {
            self.try_save_table_schema(&table_name, &t)
                .map_err(|err| format!("persist FTS schema `{table_name}`: {err}"))?;
            self.note_catalog_registry_changed();
        }
        Ok(())
    }

    pub fn get_document(&self, table: &str, doc_id: DocId) -> Result<Option<Document>, SQLError> {
        let t = self.require_table(table)?;
        let result = t.document_store.read().get(doc_id);
        result.map_err(|error| document_store_read_error("read document", &error))
    }

    /// Fetch a column projection for many documents in one round trip.
    /// The value vector aligns with `fields`; missing fields are Null.
    /// Persistent backends extract the fields inside the storage scan
    /// so whole documents never materialise.
    pub(crate) fn get_document_fields_multi(
        &self,
        table: &str,
        doc_ids: &[DocId],
        fields: &[&str],
    ) -> Result<BTreeMap<DocId, Vec<Value>>, SQLError> {
        let t = self.require_table(table)?;
        let result = t.document_store.read().get_fields_multi(doc_ids, fields);
        result.map_err(|error| document_store_read_error("read document fields", &error))
    }

    pub(crate) fn get_document_fields(
        &self,
        table: &str,
        doc_ids: &[DocId],
        field: &str,
    ) -> Result<BTreeMap<DocId, Value>, SQLError> {
        let t = self.require_table(table)?;
        let rows = t
            .document_store
            .read()
            .get_fields_multi(doc_ids, &[field])
            .map_err(|error| document_store_read_error("read document field", &error))?;
        let mut out = BTreeMap::new();
        for (doc_id, mut values) in rows {
            if values.len() != 1 {
                return Err(SQLError::Internal(format!(
                    "read document field returned {} projected values for document {doc_id}; expected 1",
                    values.len()
                )));
            }
            out.insert(doc_id, values.remove(0));
        }
        Ok(out)
    }

    pub fn find_doc_id_by_field(
        &self,
        table: &str,
        field: &str,
        value: &Value,
    ) -> Result<Option<DocId>, SQLError> {
        let t = self.require_table(table)?;
        let result = t.document_store.read().find_doc_id_by_field(field, value);
        result.map_err(|error| document_store_read_error("find document by field", &error))
    }

    /// Find the first document whose conflict columns all match the
    /// given values. Returns the existing doc id when a conflict
    /// exists, `None` when the row would be a fresh insert. Mirrors
    /// `PostgreSQL`'s `ON CONFLICT (col, ...)` lookup; the conflict
    /// columns map to the unique-constraint target.
    ///
    /// Lookup order: the integer-primary-key slot mapping, then a
    /// value-index equality probe on the first index-answerable
    /// conflict column (conflict targets are PRIMARY KEY / UNIQUE
    /// columns admitted by `value_indexable_fields`), and only then the
    /// evaluated document scan. The index
    /// probe is what keeps per-row UNIQUE and FOREIGN KEY validation
    /// `O(log n)` during bulk inserts -- previously every insert into a
    /// table with a non-integer unique column re-scanned all documents,
    /// making an n-row load `O(n^2)`.
    pub fn find_conflict(
        &self,
        table: &str,
        conflict_columns: &[String],
        values: &[Value],
    ) -> Result<Option<DocId>, SQLError> {
        if conflict_columns.is_empty() || conflict_columns.len() != values.len() {
            return Ok(None);
        }
        let t = self.require_table(table)?;
        if conflict_columns.len() == 1 {
            if let Some(doc_id) =
                Self::doc_id_for_primary_key_conflict(&t, &conflict_columns[0], &values[0])
            {
                if u128::from(doc_id) >= *t.next_id.lock() {
                    return Ok(None);
                }
                let exists = t
                    .document_store
                    .read()
                    .contains_doc_id(doc_id)
                    .map_err(|error| {
                        document_store_read_error("check conflicting document", &error)
                    })?;
                return Ok(exists.then_some(doc_id));
            }
        }
        match self.find_conflict_via_value_index(&t, table, conflict_columns, values)? {
            IndexConflictProbe::Conflict(doc_id) => return Ok(Some(doc_id)),
            IndexConflictProbe::NoConflict => return Ok(None),
            IndexConflictProbe::Unanswerable => {}
        }
        let result = t
            .document_store
            .read()
            .find_doc_id_by_fields(conflict_columns, values);
        result.map_err(|error| document_store_read_error("find conflicting document", &error))
    }

    /// Index-backed conflict lookup. `Unanswerable` means no conflict
    /// column could be answered by a value index (unindexed columns, or
    /// the temporal/NaN semantics guard refused) and the caller must
    /// fall back to the evaluated scan. Otherwise the answer is
    /// authoritative: candidates narrow through the pivot column's
    /// posting list in `O(log n + k)` and the remaining columns verify
    /// against stored fields on those candidates only, with the same
    /// `Value` equality the evaluated scan uses. An empty posting list
    /// is an authoritative `NoConflict`, which is the common case on
    /// insert and must not degrade into a scan.
    fn find_conflict_via_value_index(
        &self,
        t: &TableState,
        table: &str,
        conflict_columns: &[String],
        values: &[Value],
    ) -> Result<IndexConflictProbe, SQLError> {
        for (pivot, (column, value)) in conflict_columns.iter().zip(values.iter()).enumerate() {
            let Some(candidates) =
                self.value_index_scan(table, column, &uqa_core::Predicate::Equals(value.clone()))?
            else {
                continue;
            };
            let store = t.document_store.read();
            for entry in candidates.entries() {
                let mut matches = true;
                for (index, (column, expected)) in
                    conflict_columns.iter().zip(values.iter()).enumerate()
                {
                    if index == pivot {
                        continue;
                    }
                    let actual = store.get_field(entry.doc_id, column).map_err(|error| {
                        document_store_read_error("verify conflicting document", &error)
                    })?;
                    if actual.unwrap_or(Value::Null) != *expected {
                        matches = false;
                        break;
                    }
                }
                if matches {
                    return Ok(IndexConflictProbe::Conflict(entry.doc_id));
                }
            }
            return Ok(IndexConflictProbe::NoConflict);
        }
        Ok(IndexConflictProbe::Unanswerable)
    }

    fn doc_id_for_primary_key_conflict(
        table: &TableState,
        column: &str,
        value: &Value,
    ) -> Option<DocId> {
        let Value::Int(id) = value else {
            return None;
        };
        if *id < 0 {
            return None;
        }
        let columns = table.columns.read();
        let maps_to_doc_id = columns.iter().any(|col| {
            col.name == column
                && col.primary_key
                && matches!(col.ty, uqa_sql::ast::ColumnType::Integer)
        });
        if !maps_to_doc_id {
            return None;
        }
        Some(*id as DocId)
    }

    /// Apply per-column updates to an existing document. Mirrors the
    /// `DO UPDATE SET col = expr` branch of an ON CONFLICT clause.
    /// Returns whether the row was updated; `Ok(false)` when the
    /// document no longer exists. Storage write failures surface as
    /// `Err` so the enclosing transaction rolls back instead of
    /// committing a delete whose re-insert never happened.
    pub fn update_document_fields(
        &self,
        table: &str,
        doc_id: DocId,
        updates: BTreeMap<String, Value>,
        vectors: BTreeMap<String, Vec<f32>>,
    ) -> Result<bool, SQLError> {
        let vector_values = vectors
            .into_iter()
            .map(|(field, vector)| (field, vec![vector]))
            .collect();
        self.update_document_fields_with_vector_values(table, doc_id, updates, vector_values)
    }

    pub fn update_document_fields_with_vector_values(
        &self,
        table: &str,
        doc_id: DocId,
        updates: BTreeMap<String, Value>,
        vectors: BTreeMap<String, Vec<Vec<f32>>>,
    ) -> Result<bool, SQLError> {
        self.with_implicit_transaction(|engine| {
            engine.update_document_fields_with_vector_values_inner(table, doc_id, updates, vectors)
        })
    }

    fn update_document_fields_with_vector_values_inner(
        &self,
        table: &str,
        doc_id: DocId,
        updates: BTreeMap<String, Value>,
        vectors: BTreeMap<String, Vec<Vec<f32>>>,
    ) -> Result<bool, SQLError> {
        let Some(t) = self
            .try_table(table)
            .map_err(|error| SQLError::Internal(format!("resolve table `{table}`: {error}")))?
        else {
            return Err(SQLError::UnknownTable(table.to_string()));
        };
        let Some(mut doc) = t
            .document_store
            .read()
            .get(doc_id)
            .map_err(|error| document_store_read_error("read document for update", &error))?
        else {
            return Ok(false);
        };
        self.validate_vector_values(table, &vectors)?;
        for (k, v) in updates {
            doc.insert(k, v);
        }
        let mut replacement_vectors = Self::document_vector_values(&t, &doc)?;
        for (field, values) in vectors {
            replacement_vectors.insert(field, values);
        }
        // Each index's replacement path validates/stages before publishing.
        // Never delete the old row/index state first: an analyzer or backend
        // failure must leave the prior version queryable.
        self.add_document_with_vector_values_inner(table, doc_id, doc, replacement_vectors, false)?;
        Ok(true)
    }

    /// Apply field-level updates without materialising the whole
    /// document. Callers must only use this path when constraints and
    /// referential actions do not need the old or complete new row.
    pub fn patch_document_fields(
        &self,
        table: &str,
        doc_id: DocId,
        updates: &BTreeMap<String, Value>,
        vectors: &BTreeMap<String, Vec<f32>>,
    ) -> Result<bool, SQLError> {
        let vector_values: BTreeMap<String, Vec<Vec<f32>>> = vectors
            .iter()
            .map(|(field, vector)| (field.clone(), vec![vector.clone()]))
            .collect();
        self.patch_document_fields_with_vector_values(table, doc_id, updates, &vector_values)
    }

    pub fn patch_document_fields_with_vector_values(
        &self,
        table: &str,
        doc_id: DocId,
        updates: &BTreeMap<String, Value>,
        vectors: &BTreeMap<String, Vec<Vec<f32>>>,
    ) -> Result<bool, SQLError> {
        self.with_implicit_transaction(|engine| {
            engine.patch_document_fields_with_vector_values_inner(table, doc_id, updates, vectors)
        })
    }

    fn patch_document_fields_with_vector_values_inner(
        &self,
        table: &str,
        doc_id: DocId,
        updates: &BTreeMap<String, Value>,
        vectors: &BTreeMap<String, Vec<Vec<f32>>>,
    ) -> Result<bool, SQLError> {
        let Some(t) = self
            .try_table(table)
            .map_err(|error| SQLError::Internal(format!("resolve table `{table}`: {error}")))?
        else {
            return Err(SQLError::UnknownTable(table.to_string()));
        };
        let Some(mut document) = t
            .document_store
            .read()
            .get(doc_id)
            .map_err(|error| document_store_read_error("read document for update", &error))?
        else {
            return Ok(false);
        };
        self.validate_vector_values(table, vectors)?;
        for (field, value) in updates {
            if matches!(value, Value::Null) {
                document.remove(field);
            } else {
                document.insert(field.clone(), value.clone());
            }
        }

        let vector_fields = t.vector_indexes.read().keys().cloned().collect::<Vec<_>>();
        let mut replacement_vectors = vectors.clone();
        for field in vector_fields {
            if !updates.contains_key(&field) || replacement_vectors.contains_key(&field) {
                continue;
            }
            let values = match document.get(&field) {
                Some(value) => Self::field_index_vectors(&t, &field, value)?.unwrap_or_default(),
                None => Vec::new(),
            };
            replacement_vectors.insert(field, values);
        }

        // The common replacement path stages text analysis and vector input
        // before publishing and updates the document/value indexes as one
        // logical row version. This avoids the old patch -> remove -> add
        // sequence where an analyzer failure left the stored row changed and
        // its postings deleted in a memory engine.
        self.add_document_with_vector_values_inner(
            table,
            doc_id,
            document,
            replacement_vectors,
            false,
        )?;
        Ok(true)
    }

    pub(crate) fn rewrite_document(
        &self,
        table: &str,
        doc_id: DocId,
        document: Document,
    ) -> Result<(), SQLError> {
        self.with_implicit_transaction(|engine| {
            engine.rewrite_document_inner(table, doc_id, document)
        })
    }

    fn rewrite_document_inner(
        &self,
        table: &str,
        doc_id: DocId,
        document: Document,
    ) -> Result<(), SQLError> {
        let table_name = self
            .try_resolve_table_name(table)
            .map_err(|error| SQLError::Internal(format!("resolve table `{table}`: {error}")))?
            .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
        let t = self
            .try_table(&table_name)
            .map_err(|error| SQLError::Internal(format!("resolve table `{table}`: {error}")))?
            .ok_or_else(|| SQLError::UnknownTable(table_name.clone()))?;
        let vectors = Self::document_vector_values(&t, &document)?;
        self.validate_vector_values(&table_name, &vectors)?;
        self.add_document_with_vector_values_inner(&table_name, doc_id, document, vectors, false)
    }

    /// Rewrite a row while a column is being dropped or renamed. The
    /// operation changes field names, not the indexed values: catalog
    /// lifecycle code drops or renames the durable postings afterward.
    /// Maintaining them against the half-updated schema here would replace a
    /// renamed field with NULL before its metadata has moved.
    fn rewrite_document_for_schema_change(
        &self,
        table: &str,
        doc_id: DocId,
        document: Document,
    ) -> Result<(), SQLError> {
        let table_name = self
            .try_resolve_table_name(table)
            .map_err(|error| SQLError::Internal(format!("resolve table `{table}`: {error}")))?
            .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
        let t = self
            .try_table(&table_name)
            .map_err(|error| SQLError::Internal(format!("resolve table `{table}`: {error}")))?
            .ok_or_else(|| SQLError::UnknownTable(table_name.clone()))?;
        let vector_fields: Vec<FieldName> = t.vector_indexes.read().keys().cloned().collect();
        let mut vectors: BTreeMap<FieldName, Vec<Vec<f32>>> = BTreeMap::new();
        for field in vector_fields {
            let Some(value) = document.get(&field) else {
                continue;
            };
            if let Some(values) = Self::field_index_vectors(&t, &field, value)? {
                vectors.insert(field, values);
            }
        }
        let mut text_fields: BTreeMap<FieldName, String> = BTreeMap::new();
        for field in t.fts_fields() {
            if let Some(Value::Str(value)) = document.get(&field) {
                text_fields.insert(field, value.clone());
            }
        }
        t.document_store
            .write()
            .put(doc_id, document)
            .map_err(|err| document_store_write_error(&err))?;
        {
            let mut index = t.inverted_index.write();
            index
                .add_document(doc_id, text_fields)
                .map_err(|error| SQLError::Internal(format!("index document: {error}")))?;
        }
        for (field, index) in t.vector_indexes.write().iter_mut() {
            index
                .add_many(doc_id, vectors.remove(field).unwrap_or_default())
                .map_err(|error| SQLError::Internal(format!("index document vector: {error}")))?;
        }
        self.mark_column_stats_dirty(&table_name, &t)
            .map_err(|err| SQLError::Internal(format!("invalidate column stats: {err}")))?;
        Ok(())
    }

    pub fn delete_document(&self, table: &str, doc_id: DocId) -> Result<(), SQLError> {
        self.with_implicit_transaction(|engine| engine.delete_document_inner(table, doc_id))
    }

    fn delete_document_inner(&self, table: &str, doc_id: DocId) -> Result<(), SQLError> {
        let table_name = self
            .try_resolve_table_name(table)
            .map_err(|error| SQLError::Internal(format!("resolve table `{table}`: {error}")))?
            .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
        let t = self
            .try_table(&table_name)
            .map_err(|error| SQLError::Internal(format!("resolve table `{table}`: {error}")))?
            .ok_or_else(|| SQLError::UnknownTable(table_name.clone()))?;
        let old_indexed = Self::value_indexes_old_values(&t, doc_id)?;
        let mut store = t.document_store.write();
        store
            .delete(doc_id)
            .map_err(|err| document_store_write_error(&err))?;
        self.persist_value_indexes_apply_write(&table_name, doc_id, None)?;
        if let Some(old) = old_indexed.as_ref() {
            Self::value_indexes_apply_write(&t, doc_id, Some(old), None);
        }
        drop(store);
        t.inverted_index
            .write()
            .remove_document(doc_id)
            .map_err(|error| SQLError::Internal(format!("remove indexed document: {error}")))?;
        for idx in t.vector_indexes.write().values_mut() {
            idx.as_mut()
                .delete(doc_id)
                .map_err(|error| SQLError::Internal(format!("delete indexed vector: {error}")))?;
        }
        self.mark_column_stats_dirty(&table_name, &t)
            .map_err(|err| SQLError::Internal(format!("invalidate column stats: {err}")))?;
        Ok(())
    }

    pub fn document_count(&self, table: &str) -> Result<u64, SQLError> {
        let t = self.require_table(table)?;
        let result = t.inverted_index.read().doc_count();
        result.map_err(|error| SQLError::Internal(format!("read indexed document count: {error}")))
    }
}

/// A persistent document-store write failed. Surfacing this as a
/// statement error makes the enclosing transaction roll back, so the
/// on-disk state never keeps a half-applied rewrite.
pub(crate) fn document_store_write_error(err: &StorageBackendError) -> SQLError {
    SQLError::Internal(format!("document store write failed: {err}"))
}

pub(crate) fn document_store_read_error(action: &str, err: &StorageBackendError) -> SQLError {
    SQLError::Internal(format!("{action} failed: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_relation_references_match_canonical_targets_and_corruption_fails_closed() {
        let public_parent = RelationIdentity::new("public", "parent");
        let app_parent = RelationIdentity::new("app", "parent");

        assert!(stored_relation_reference_matches("parent", &public_parent));
        assert!(stored_relation_reference_matches("parent", &app_parent));
        assert!(stored_relation_reference_matches(
            "public.parent",
            &public_parent
        ));
        assert!(!stored_relation_reference_matches(
            "public.parent",
            &app_parent
        ));
        assert!(stored_relation_reference_matches(
            "corrupt.reference.extra",
            &public_parent
        ));
    }

    #[test]
    fn schema_change_rewrite_rejects_an_unknown_table() {
        let error = Engine::new()
            .rewrite_document_for_schema_change("missing", 1, Document::new())
            .unwrap_err();
        assert!(matches!(error, SQLError::UnknownTable(_)), "{error}");
    }
}
