//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Relation binding borrows retained declarations and publishes the selected namespace under the invoking control.

use super::{alias_table_schema, SchemaScope};
use crate::binding::{non_returning_cte_error, projection::rename_schema_with_control};
use crate::semantics::sets::placement_scope::SetPlacementScope;
use crate::{
    catalog::analysis::ViewDefinitionRef, plan::SourcePlan, routines::RoutineResolution,
    ColumnType, RowSchema, SQLError, SQLParam,
};

impl SchemaScope {
    #[expect(
        clippy::too_many_lines,
        reason = "preserves ordered CTE, sequence, view and relation selection in one scope"
    )]
    pub(in crate::binding) fn bind_relation_source(
        &mut self,
        routines: &dyn RoutineResolution,
        source: &SourcePlan,
        params: &[SQLParam],
        outer: Option<&RowSchema>,
    ) -> Result<RowSchema, SQLError> {
        let SourcePlan::Table {
            name,
            qualifier,
            alias,
            column_aliases,
            bound_columns,
            ..
        } = source
        else {
            unreachable!("relation source")
        };
        let qualifier = alias.as_deref().unwrap_or(qualifier);
        let resources = self.production.clone();
        let control = resources.control();
        let cte_name = crate::RelationIdentity::borrowed_reference(name)
            .filter(|reference| reference.schema.is_none())
            .map(|reference| control.format(format_args!("{}", reference.name)))
            .transpose()?;
        if let Some(schema) = cte_name.as_ref().and_then(|name| self.ctes.get(name)) {
            if let Some(name) = cte_name
                .as_ref()
                .filter(|name| self.non_returning_ctes.contains(name))
            {
                return Err(non_returning_cte_error(name));
            }
            return alias_table_schema(schema, qualifier, column_aliases, &control);
        }
        if let Some(plan) = cte_name
            .as_ref()
            .and_then(|name| self.deferred_ctes.get(name))
        {
            if !plan.body.returns_rows() {
                return Err(non_returning_cte_error(&plan.name));
            }
        }
        if let Some(name) = cte_name
            .as_ref()
            .filter(|name| self.deferred_ctes.contains_key(name))
        {
            let retained = self.deferred_ctes.extract(name);
            let plan = retained.get(name).expect("selected deferred CTE");
            let result = self
                .bind_cte_schemas(routines, std::slice::from_ref(plan.as_ref()), params, outer)
                .and_then(|previous| {
                    let schema = self.ctes.get(&plan.name).expect("CTE schema was bound");
                    let result = alias_table_schema(schema, qualifier, column_aliases, &control);
                    self.restore_cte_schemas(previous);
                    result
                });
            self.deferred_ctes.append(retained);
            return result;
        }
        if self
            .catalog
            .sequence_exists_with_control(&self.resolution, name, &control)?
        {
            self.record_relation_dependency(name)?;
            let schema = RowSchema::with_column_metadata_with_control(
                Some(qualifier),
                [
                    ("last_value", Some(&ColumnType::BigInteger), None),
                    ("log_cnt", Some(&ColumnType::BigInteger), None),
                    ("is_called", Some(&ColumnType::Boolean), None),
                ],
                &control,
            )?;
            return self.alias_relation_schema(&schema, name, qualifier, column_aliases);
        }
        let catalog = self.catalog.clone();
        let resolution = self.resolution.copy_with_control(&control)?;
        let mut view_schema = None;
        catalog.with_view_definition_with_control(&resolution, name, &control, &mut |view| {
            view_schema = Some(self.bind_view_source(
                routines,
                view,
                name,
                qualifier,
                column_aliases,
                params,
                outer,
            )?);
            Ok(())
        })?;
        if let Some(schema) = view_schema {
            return Ok(schema);
        }
        if let Some(table) = catalog.table_resolved_with_control(&resolution, name, &control)? {
            self.record_relation_dependency(name)?;
            let schema = RowSchema::with_column_metadata_with_control(
                Some(qualifier),
                table
                    .columns
                    .iter()
                    .map(|column| (column.name.as_str(), Some(&column.ty), column.collation_oid)),
                &control,
            )?;
            let schema = crate::semantics::bound_source_schema_with_control(
                &schema,
                bound_columns.as_deref(),
                &control,
            )?;
            let schema = self.alias_relation_schema(&schema, name, qualifier, column_aliases)?;
            let schema = if table.columns.is_empty() && !table.columns_declared {
                RowSchema::with_open_columns_with_control(&schema, Some(qualifier), &control)?
            } else {
                schema
            };
            return crate::binding::analysis::with_table_pseudo_columns_with_control(
                &schema, qualifier, &control,
            );
        }
        let mut foreign_schema = None;
        catalog.with_foreign_table_columns_with_control(
            &resolution,
            name,
            &control,
            &mut |columns| {
                self.record_relation_dependency(name)?;
                let schema = RowSchema::with_column_metadata_with_control(
                    Some(qualifier),
                    columns.iter().map(|column| {
                        (column.name.as_str(), Some(&column.ty), column.collation_oid)
                    }),
                    &control,
                )?;
                let schema = crate::semantics::bound_source_schema_with_control(
                    &schema,
                    bound_columns.as_deref(),
                    &control,
                )?;
                foreign_schema =
                    Some(self.alias_relation_schema(&schema, name, qualifier, column_aliases)?);
                Ok(())
            },
        )?;
        if let Some(schema) = foreign_schema {
            return Ok(schema);
        }
        if let Some(columns) =
            catalog.virtual_relation_schema_with_control(&resolution, name, &control)?
        {
            self.record_relation_dependency(name)?;
            let schema = RowSchema::with_column_metadata_with_control(
                Some(qualifier),
                columns
                    .iter()
                    .map(|(name, ty)| (name.as_str(), Some(ty), None)),
                &control,
            )?;
            return self.alias_relation_schema(&schema, name, qualifier, column_aliases);
        }
        if let Some(relation) =
            catalog.unopenable_relation_with_control(&resolution, name, &control)?
        {
            return Err(relation.error_with_control(&control));
        }
        Err(SQLError::UnknownTable(
            control.copy_text(name)?.into_parts().0,
        ))
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "retains the original relation binding scope and declaration"
    )]
    fn bind_view_source(
        &mut self,
        routines: &dyn RoutineResolution,
        view: ViewDefinitionRef<'_>,
        name: &str,
        qualifier: &str,
        aliases: &[String],
        params: &[SQLParam],
        outer: Option<&RowSchema>,
    ) -> Result<RowSchema, SQLError> {
        self.record_relation_dependency(name)?;
        let resources = self.production.clone();
        let control = resources.control();
        if view.materialized {
            let columns = view.output_columns.unwrap_or_default();
            if columns.len() != view.materialized_column_types.len() {
                return Err(SQLError::Internal(
                    control
                        .format(format_args!(
                            "materialized view `{name}` has {} columns but {} stored column types",
                            columns.len(),
                            view.materialized_column_types.len()
                        ))?
                        .into_parts()
                        .0,
                ));
            }
            let schema = crate::catalog::view::materialized_view_schema_with_control(
                columns,
                view.materialized_column_types,
                view.materialized_column_collations,
                &control,
            )?;
            let schema =
                RowSchema::with_relation_qualifier_with_control(&schema, qualifier, &control)?;
            return self.alias_relation_schema(&schema, name, qualifier, aliases);
        }
        let (mut key, memory) = control.copy_text(name)?.into_parts();
        key.make_ascii_lowercase();
        let key = control.finish(key, memory)?;
        if !self.visiting_views.insert_with_control(&key, &control)? {
            return Err(SQLError::Internal(
                control
                    .format(format_args!(
                        "view `{name}` has a recursive schema dependency"
                    ))?
                    .into_parts()
                    .0,
            ));
        }
        let result = (|| {
            self.record_stored_query_dependencies(view.query)?;
            // Stored definitions retain their original set-returning expression placement.
            let schema = {
                let _stored = SetPlacementScope::stored_definition();
                self.bind_query(routines, view.query, params, outer)?
            };
            let schema = rename_schema_with_control(
                &schema,
                view.output_columns.unwrap_or_default(),
                Some(qualifier),
                &control,
            )?;
            self.alias_relation_schema(&schema, name, qualifier, aliases)
        })();
        self.visiting_views.remove(&key);
        result
    }
}

#[cfg(test)]
mod tests;
