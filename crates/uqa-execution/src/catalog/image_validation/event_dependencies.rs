//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Lend original event and relation catalogs to SQL's native reference predicate.

use crate::catalog::{relation_names::RelationNameCatalog, CatalogReadView};
use uqa_core::{memory::ProductionControl, RelationIdentity};
use uqa_sql::{catalog::events::native::NativeEventCatalog, SQLError};

pub(super) fn validate(
    catalog: &CatalogReadView,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    let definitions = &catalog.snapshot().definitions;
    uqa_sql::catalog::events::native::validate(
        &definitions.triggers,
        &definitions.rules,
        &Events(catalog),
        control,
    )
}

struct Events<'a>(&'a CatalogReadView);

impl NativeEventCatalog for Events<'_> {
    fn kind(&self, relation: &RelationIdentity) -> Result<Option<&'static str>, SQLError> {
        let snapshot = self.0.snapshot();
        if let Some(kind) =
            RelationNameCatalog::new(&snapshot.tables, &snapshot.definitions).kind_at(relation)
        {
            return Ok(Some(kind));
        }
        Ok(self
            .0
            .graph_label_kind(&relation.schema, &relation.name)?
            .map(|_| "table"))
    }

    fn contains_column(
        &self,
        relation: &RelationIdentity,
        column: &str,
        control: &ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        control.check()?;
        let snapshot = self.0.snapshot();
        let definitions = &snapshot.definitions;
        if let Some(table) = snapshot.tables.get(relation) {
            // Document fields have no closed declaration to validate against.
            if !table.columns_declared {
                return Ok(true);
            }
            return contains(
                table.columns.iter().map(|column| column.name.as_str()),
                column,
                control,
            );
        }
        if let Some(table) = definitions.foreign_tables.get(relation) {
            return contains(
                table.columns.iter().map(|column| column.name.as_str()),
                column,
                control,
            );
        }
        if let Some(view) = definitions.views.get(relation) {
            let columns = view.output_columns.as_ref().ok_or_else(|| {
                SQLError::Internal("native event view has no captured column names".into())
            })?;
            return contains(columns.iter().map(String::as_str), column, control);
        }
        if let Some(system) = uqa_sql::catalog::SystemRelation::at(&relation.schema, &relation.name)
        {
            let columns = system.column_names_with_control(control)?;
            return contains(columns.iter().map(String::as_str), column, control);
        }
        if definitions.sequences.contains_key(relation) {
            return contains(
                uqa_sql::schema::sequences::ROW_COLUMN_NAMES.into_iter(),
                column,
                control,
            );
        }
        match self.0.graph_label_kind(&relation.schema, &relation.name)? {
            Some(kind) => contains(
                crate::catalog::graph::age_columns(kind).iter().copied(),
                column,
                control,
            ),
            None => Ok(false),
        }
    }
}

fn contains<'a>(
    names: impl Iterator<Item = &'a str>,
    column: &str,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    for name in names {
        control.check()?;
        if name == column {
            return Ok(true);
        }
    }
    Ok(false)
}
