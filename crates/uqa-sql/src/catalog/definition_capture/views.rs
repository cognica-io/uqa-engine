//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! View images retain bound syntax and evaluated materialized rows without query execution.

use crate::{
    catalog::{
        security::table_binding::CapturedTableSecurity,
        stored_view::{StoredView, StoredViewDefinition},
        view::StoredViewKind,
    },
    plan::{copy::SyntaxProduction, QueryPlan},
    SQLError,
};
use std::collections::BTreeMap;
use uqa_core::{
    memory::{Produced, ProductionControl},
    RelationIdentity, Value,
};

#[derive(Debug)]
pub struct CapturedView {
    pub security: CapturedTableSecurity,
    pub object_id: [u8; 16],
    pub query: QueryPlan,
    pub output_columns: Option<Vec<String>>,
    pub persistence: crate::ast::RelationPersistence,
    pub options: Vec<(String, String)>,
    pub kind: StoredViewKind,
    pub materialized_rows: Vec<Vec<(String, Value)>>,
    pub materialized_column_types: Vec<Option<crate::ast::ColumnType>>,
    pub materialized_column_collations: Option<Vec<u32>>,
    pub populated: bool,
    pub catalog_oids: Option<crate::catalog::relation_oids::RelationCatalogOids>,
    pub row_type_array_name: Option<String>,
}

pub type CapturedViews = Vec<(RelationIdentity, CapturedView)>;

impl StoredView {
    pub fn capture_definition(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<CapturedView>, SQLError> {
        let output = SyntaxProduction::new(*control);
        let StoredViewDefinition {
            object_id,
            query,
            output_columns,
            persistence,
            options,
            kind,
            materialized_rows,
            materialized_column_types,
            materialized_column_collations,
            populated,
            catalog_oids,
            row_type_array_name,
        } = &self.definition;
        let security = output.take(self.security.capture_security(control)?);
        let query = output.capture_stored(query)?;
        let mut rows = output.vector(materialized_rows.len())?;
        for row in materialized_rows {
            let mut entries = output.vector(row.len())?;
            for (name, value) in row {
                entries.push((output.text(name)?, output.take(control.copy_value(value)?)))?;
            }
            rows.push(entries.finish()?)?;
        }
        let captured = CapturedView {
            security,
            object_id: *object_id,
            query,
            output_columns: output.copy(output_columns)?,
            persistence: *persistence,
            options: output.copy(options)?,
            kind: *kind,
            materialized_rows: rows.finish()?,
            materialized_column_types: output.copy(materialized_column_types)?,
            materialized_column_collations: output.copy(materialized_column_collations)?,
            populated: *populated,
            catalog_oids: *catalog_oids,
            row_type_array_name: output.copy(row_type_array_name)?,
        };
        output.finish(captured)
    }
}

/// Select durable definitions before admitting names, syntax, authority or evaluated rows. Individual definition capture remains independent of persistence; this catalog boundary excludes session-owned views.
pub fn durable_views(
    values: &BTreeMap<RelationIdentity, StoredView>,
    control: &ProductionControl<'_>,
) -> Result<Produced<CapturedViews>, SQLError> {
    let output = SyntaxProduction::new(*control);
    let count = values.values().try_fold(0, |count, view| {
        output.check()?;
        Ok::<_, SQLError>(
            count + usize::from(view.persistence != crate::ast::RelationPersistence::Temporary),
        )
    })?;
    let mut entries = output.vector(count)?;
    for (relation, view) in values {
        output.check()?;
        if view.persistence == crate::ast::RelationPersistence::Temporary {
            continue;
        }
        entries.push((
            output.copy(relation)?,
            output.take(view.capture_definition(control)?),
        ))?;
    }
    let entries = entries.finish()?;
    output.finish(entries)
}

#[cfg(test)]
mod tests;
