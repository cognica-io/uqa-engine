//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Statement-snapshot column ownership for filter pushdown.

use std::collections::{BTreeMap, BTreeSet};

use uqa_sql::semantics::{DOC_ID_COLUMN, SCORE_COLUMN};
use uqa_sql::SQLError;

use super::FilterPushdownScope;
use uqa_sql::catalog::{analysis::AnalysisCatalog, resolution::RelationNameResolution};

use super::{query_plan_output_columns, SourcePlan, TABLE_OID_COLUMN, XMIN_COLUMN};

pub(super) type ColumnOwners = BTreeMap<String, Option<String>>;

/// The columns that the relations of one FROM clause expose to unqualified references.
pub(super) struct SourceColumns {
    /// Each listed column name with its owning qualifier, or `None` when several relations, or a relation without a qualifier, expose it.
    owners: ColumnOwners,
    /// Qualifiers whose complete column list the statement snapshot does not determine: CTE references, table functions, aliased joins, schemaless tables and derived tables with a wildcard target.
    opaque: BTreeSet<String>,
    /// The qualifiers that the FROM clause exposes.
    qualifiers: BTreeSet<String>,
    /// Whether an unaliased VALUES list or subquery exposes columns without any qualifier.
    unqualified_relation: bool,
}

impl SourceColumns {
    pub(super) fn new(
        scope: FilterPushdownScope<'_>,
        source: &SourcePlan,
        qualifiers: &BTreeSet<String>,
    ) -> Result<Self, SQLError> {
        let mut columns = Self {
            owners: ColumnOwners::new(),
            opaque: BTreeSet::new(),
            qualifiers: qualifiers.clone(),
            unqualified_relation: false,
        };
        columns.collect(scope, source)?;
        Ok(columns)
    }

    /// Ownership of explicitly listed columns, for tests that exercise attribution without a FROM clause.
    #[cfg(test)]
    pub(super) fn listed(owners: ColumnOwners, qualifiers: BTreeSet<String>) -> Self {
        Self {
            owners,
            opaque: BTreeSet::new(),
            qualifiers,
            unqualified_relation: false,
        }
    }

    pub(super) fn owners(&self) -> &ColumnOwners {
        &self.owners
    }

    /// The only relation of the FROM clause. A predicate that reads no column, such as a retrieval predicate over the relation's documents, belongs to it.
    pub(super) fn only_relation(&self) -> Option<&str> {
        let mut qualifiers = self.qualifiers.iter();
        match (qualifiers.next(), qualifiers.next()) {
            (Some(qualifier), None) => Some(qualifier),
            _ => None,
        }
    }

    /// Whether an unqualified reference to `column` reads a column of `qualifier`. A listed column belongs only to its owner. A name that no relation lists belongs to the qualifier only when it is the only relation, no unaliased relation exposes columns beside it, its column list is not determined, and the name is not a FROM relation's name, which would make the reference a whole-row value.
    pub(super) fn reads_column_of(&self, column: &str, qualifier: &str) -> bool {
        match self.owners.get(column) {
            Some(owner) => owner.as_deref() == Some(qualifier),
            None => {
                self.only_relation() == Some(qualifier)
                    && !self.unqualified_relation
                    && self.opaque.contains(qualifier)
                    && !self.qualifiers.contains(column)
            }
        }
    }

    fn collect(
        &mut self,
        scope: FilterPushdownScope<'_>,
        source: &SourcePlan,
    ) -> Result<(), SQLError> {
        match source {
            SourcePlan::RetainedRows(_) => {}
            SourcePlan::Table {
                name,
                qualifier,
                alias,
                column_aliases,
                bound_columns,
                ..
            } => {
                let qualifier = alias.as_deref().unwrap_or(qualifier);
                let columns = if (scope.is_visible_cte)(name) {
                    None
                } else if let Some(columns) = bound_columns {
                    Some(with_table_pseudo_columns(columns.clone()))
                } else {
                    relation_source_columns(scope.catalog, scope.resolution, name)?
                };
                match columns {
                    Some(mut columns) => {
                        for (column, alias) in columns.iter_mut().zip(column_aliases) {
                            column.clone_from(alias);
                        }
                        self.register(qualifier, columns);
                    }
                    None => self.register_opaque(qualifier, column_aliases.clone()),
                }
            }
            SourcePlan::Join {
                left, right, alias, ..
            } => match alias {
                Some(alias) => self.register_opaque(alias, Vec::new()),
                None => {
                    self.collect(scope, left)?;
                    self.collect(scope, right)?;
                }
            },
            SourcePlan::Values {
                rows,
                alias,
                column_aliases,
                internal_relation,
                ..
            } => {
                let width = rows.first().map_or(column_aliases.len(), Vec::len);
                let columns = (0..width).map(|index| {
                    column_aliases
                        .get(index)
                        .cloned()
                        .unwrap_or_else(|| format!("column{}", index + 1))
                });
                match (alias, internal_relation) {
                    (Some(alias), _) => self.register(alias, columns),
                    (None, None) => self.register_unqualified(columns),
                    // An internal carrier exposes its attributes only as internal columns.
                    (None, Some(_)) => {}
                }
            }
            SourcePlan::Subquery {
                body,
                alias,
                column_aliases,
                ..
            } => {
                let output = query_plan_output_columns(body)
                    .filter(|columns| !columns.iter().any(|column| column == "*"));
                match (alias, output) {
                    (Some(alias), Some(mut columns)) => {
                        for (column, alias) in columns.iter_mut().zip(column_aliases) {
                            column.clone_from(alias);
                        }
                        self.register(alias, columns);
                    }
                    (Some(alias), None) => self.register_opaque(alias, column_aliases.clone()),
                    (None, output) => self.register_unqualified(output.unwrap_or_default()),
                }
            }
            SourcePlan::Function { column_aliases, .. }
            | SourcePlan::FunctionGroup { column_aliases, .. } => {
                if let Some(qualifier) = source.visible_qualifier() {
                    self.register_opaque(qualifier, column_aliases.clone());
                }
            }
        }
        Ok(())
    }

    fn register(&mut self, qualifier: &str, columns: impl IntoIterator<Item = String>) {
        for column in columns {
            self.owners
                .entry(column)
                .and_modify(|owner| *owner = None)
                .or_insert_with(|| Some(qualifier.to_string()));
        }
    }

    /// Register the columns known by alias for a relation whose complete column list is not determined.
    fn register_opaque(&mut self, qualifier: &str, columns: Vec<String>) {
        self.opaque.insert(qualifier.to_string());
        self.register(qualifier, columns);
    }

    /// A column of a relation without a qualifier belongs to no qualifier, whichever other relation also defines its name.
    fn register_unqualified(&mut self, columns: impl IntoIterator<Item = String>) {
        self.unqualified_relation = true;
        for column in columns {
            self.owners.insert(column, None);
        }
    }
}

/// Complete a table's declared columns with the system and retrieval metadata columns that every table scan resolves.
fn with_table_pseudo_columns(mut columns: Vec<String>) -> Vec<String> {
    for pseudo in [TABLE_OID_COLUMN, XMIN_COLUMN, DOC_ID_COLUMN, SCORE_COLUMN] {
        if !columns.iter().any(|column| column == pseudo) {
            columns.push(pseudo.to_string());
        }
    }
    columns
}

/// The complete column list of a catalog relation, or `None` when the snapshot does not determine it.
fn relation_source_columns(
    catalog: &dyn AnalysisCatalog,
    resolution: &RelationNameResolution,
    name: &str,
) -> Result<Option<Vec<String>>, SQLError> {
    if catalog.sequence_exists(resolution, name)? {
        return Ok(Some(vec![
            "last_value".into(),
            "log_cnt".into(),
            "is_called".into(),
        ]));
    }
    if let Some(table) = catalog.table_resolved(resolution, name)? {
        // A schemaless table resolves any field name its documents carry.
        if table.columns.is_empty() {
            return Ok(None);
        }
        return Ok(Some(with_table_pseudo_columns(
            table
                .columns
                .iter()
                .map(|column| column.name.clone())
                .collect(),
        )));
    }
    if let Some(view) = catalog.view_resolved(resolution, name)? {
        return Ok(view
            .output_columns
            .clone()
            .or_else(|| query_plan_output_columns(&view.query))
            .filter(|columns| !columns.iter().any(|column| column == "*")));
    }
    Ok(catalog
        .foreign_table_resolved(resolution, name)?
        .map(|table| {
            table
                .columns
                .iter()
                .map(|column| column.name.clone())
                .collect()
        }))
}
