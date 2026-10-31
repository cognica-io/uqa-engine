//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{CatalogReadView, RelationNameResolution};
use std::sync::Arc;
use uqa_sql::catalog::analysis::{AnalysisCatalog, TableDefinition, ViewDefinition};
use uqa_sql::{ColumnType, SQLError};

impl AnalysisCatalog for CatalogReadView {
    fn relation_row_type_with_control(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<uqa_core::memory::Produced<uqa_sql::ast::CompositeTypeReference>>, SQLError>
    {
        let selected = self.relation_kind_resolution_with_control(resolution, name, control)?;
        let super::RelationResolution::Found(canonical, _) = &*selected else {
            return Ok(None);
        };
        let reference =
            uqa_core::RelationIdentity::try_borrowed_reference(canonical).map_err(|error| {
                control
                    .format(format_args!("{error}"))
                    .map(|text| SQLError::Internal(text.into_parts().0))
                    .unwrap_or_else(Into::into)
            })?;
        let relation =
            reference.identity_with_control(uqa_core::RelationName::decoded("public"), control)?;
        let snapshot = self.snapshot();
        let oids = snapshot
            .tables
            .get(&*relation)
            .map(|table| table.catalog_oids)
            .or_else(|| {
                snapshot
                    .definitions
                    .views
                    .get(&*relation)
                    .map(|view| view.relation_oids())
            })
            .or_else(|| {
                snapshot
                    .definitions
                    .foreign_tables
                    .get(&*relation)
                    .map(super::foreign::StoredForeignTable::relation_oids)
            });
        oids.map(|oids| {
            oids.row_type_reference_with_control(&relation, control)
                .map_err(Into::into)
        })
        .transpose()
        .map(Option::flatten)
    }

    fn sequence_exists_with_control(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        CatalogReadView::sequence_exists_with_control(self, resolution, name, control)
    }

    fn virtual_relation_schema_with_control(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<
        Option<uqa_core::memory::Produced<uqa_sql::catalog::analysis::VirtualRelationSchema>>,
        SQLError,
    > {
        if let Some(relation) =
            self.virtual_relation_resolved_with_control(resolution, name, control)?
        {
            return relation.schema_with_control(control).map(Some);
        }
        super::graph::age_label_relation_schema_with_control(self, resolution, name, control)
    }

    fn with_view_definition_with_control(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
        read: &mut dyn FnMut(
            uqa_sql::catalog::analysis::ViewDefinitionRef<'_>,
        ) -> Result<(), SQLError>,
    ) -> Result<(), SQLError> {
        if let Some(view) =
            CatalogReadView::view_resolved_with_control(self, resolution, name, control)?
        {
            read(uqa_sql::catalog::analysis::ViewDefinitionRef {
                query: &view.query,
                output_columns: view.output_columns.as_deref(),
                materialized: view.kind == crate::catalog::view::StoredViewKind::Materialized,
                materialized_column_types: &view.materialized_column_types,
                materialized_column_collations: view.materialized_column_collations.as_deref(),
            })?;
        }
        control.check()?;
        Ok(())
    }

    fn with_foreign_table_columns_with_control(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
        read: &mut dyn FnMut(&[uqa_sql::ast::ColumnDef]) -> Result<(), SQLError>,
    ) -> Result<(), SQLError> {
        if let Some(table) =
            CatalogReadView::foreign_table_resolved_with_control(self, resolution, name, control)?
        {
            read(&table.columns)?;
        }
        control.check()?;
        Ok(())
    }

    fn virtual_relation_columns_with_control(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<uqa_core::memory::Produced<Vec<String>>>, SQLError> {
        if let Some(relation) =
            self.virtual_relation_resolved_with_control(resolution, name, control)?
        {
            return relation.column_names_with_control(control).map(Some);
        }
        super::graph::age_label_relation_columns_with_control(self, resolution, name, control)
    }

    fn relation_row_type(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<uqa_sql::ast::CompositeTypeReference>, SQLError> {
        let Some((canonical, _)) = self
            .relation_kind_resolution(resolution, name)?
            .into_found()
        else {
            return Ok(None);
        };
        let relation =
            uqa_core::RelationIdentity::from_legacy_name(&canonical).map_err(SQLError::Internal)?;
        let snapshot = self.snapshot();
        let oids = snapshot
            .tables
            .get(&relation)
            .map(|table| table.catalog_oids)
            .or_else(|| {
                snapshot
                    .definitions
                    .views
                    .get(&relation)
                    .map(|view| view.relation_oids())
            })
            .or_else(|| {
                snapshot
                    .definitions
                    .foreign_tables
                    .get(&relation)
                    .map(super::foreign::StoredForeignTable::relation_oids)
            });
        Ok(oids.and_then(|oids| oids.row_type_reference(&relation)))
    }

    fn relation_dependency(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<u32>, SQLError> {
        let Some((canonical, kind)) = self
            .relation_kind_resolution(resolution, name)?
            .into_found()
        else {
            return Ok(None);
        };
        super::projection::resolved_relation_oid(self, resolution, &canonical, kind)
            .and_then(|oid| {
                u32::try_from(oid)
                    .map_err(|_| SQLError::Internal(format!("invalid relation OID {oid}")))
            })
            .map(Some)
    }

    fn relation_dependency_with_control(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<u32>, SQLError> {
        super::projection::resolved_relation_dependency_with_control(
            self, resolution, name, control,
        )
    }

    fn prepared_dependency_snapshot(
        &self,
        dependencies: &uqa_sql::prepared::dependencies::PreparedAnalysisDependencies,
    ) -> Result<Option<uqa_sql::prepared::dependencies::PreparedDependencySnapshot>, SQLError> {
        super::prepared_dependencies::capture(self, dependencies).map(Some)
    }

    fn prepared_dependency_snapshot_with_control(
        &self,
        dependencies: &uqa_sql::prepared::dependencies::PreparedAnalysisDependencies,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<uqa_sql::prepared::dependencies::PreparedDependencySnapshot>, SQLError> {
        super::prepared_dependencies::capture_with_control(self, dependencies, control).map(Some)
    }

    fn effective_search_path(
        &self,
        resolution: &RelationNameResolution,
    ) -> Result<Option<uqa_sql::catalog::resolution::EffectiveSearchPath>, SQLError> {
        let role = &resolution.current_user;
        Ok(Some(uqa_sql::catalog::resolution::EffectiveSearchPath {
            schemas: super::namespaces::current_schema_names(self, resolution, role, true),
            creation_namespace: super::namespaces::current_schema_name(self, resolution, role),
        }))
    }

    fn effective_search_path_with_control(
        &self,
        resolution: &RelationNameResolution,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<
        uqa_core::memory::Produced<Option<uqa_sql::catalog::resolution::EffectiveSearchPath>>,
        SQLError,
    > {
        let role = &resolution.current_user;
        let schemas = super::namespaces::current_schema_names_with_control(
            self, resolution, role, true, control,
        )?;
        let explicit = super::namespaces::current_schema_names_with_control(
            self, resolution, role, false, control,
        )?;
        let creation = explicit
            .first()
            .map(|name| control.copy_text(name))
            .transpose()?;
        let (schemas, schema_memory) = schemas.into_parts();
        let (creation_namespace, creation_memory) = creation.map_or_else(
            || (None, control.empty_reservation()),
            |name| {
                let (name, memory) = name.into_parts();
                (Some(name), memory)
            },
        );
        Ok(control.finish(
            Some(uqa_sql::catalog::resolution::EffectiveSearchPath {
                schemas,
                creation_namespace,
            }),
            control.combine(schema_memory, creation_memory),
        )?)
    }

    fn table_resolved(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<TableDefinition>, SQLError> {
        CatalogReadView::table_resolved(self, resolution, name).map(|table| {
            table.map(|table| TableDefinition {
                columns: table.columns.clone(),
                columns_declared: table.columns_declared,
            })
        })
    }

    fn table_resolved_with_control(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<TableDefinition>, SQLError> {
        CatalogReadView::table_resolved_with_control(self, resolution, name, control).map(|table| {
            table.map(|table| TableDefinition {
                columns: table.columns.clone(),
                columns_declared: table.columns_declared,
            })
        })
    }

    fn table_name_resolved_with_control(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<uqa_core::memory::Produced<String>>, SQLError> {
        CatalogReadView::table_name_resolved_with_control(self, resolution, name, control)
    }

    fn table_name_resolved(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<String>, SQLError> {
        CatalogReadView::table_name_resolved(self, resolution, name)
    }

    fn view_resolved(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<ViewDefinition>, SQLError> {
        CatalogReadView::view_resolved(self, resolution, name).map(|view| {
            view.map(|view| ViewDefinition {
                query: view.query.clone(),
                output_columns: view.output_columns.clone(),
                materialized: view.kind == crate::catalog::view::StoredViewKind::Materialized,
                materialized_column_types: view.materialized_column_types.clone(),
                materialized_column_collations: view.materialized_column_collations.clone(),
            })
        })
    }

    fn foreign_table_resolved(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<TableDefinition>, SQLError> {
        CatalogReadView::foreign_table_resolved(self, resolution, name).map(|table| {
            table.map(|table| TableDefinition {
                columns: Arc::new(table.columns.clone()),
                columns_declared: true,
            })
        })
    }

    fn sequence_exists(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<bool, SQLError> {
        CatalogReadView::sequence_exists_with_control(
            self,
            resolution,
            name,
            &uqa_core::memory::ProductionControl::uncontrolled(),
        )
    }

    fn virtual_relation_schema(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<Vec<(String, ColumnType)>>, SQLError> {
        super::schema::virtual_relation_schema(self, resolution, name)
    }

    fn sql_functions(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<Vec<Arc<uqa_sql::routines::SQLUserFunction>>>, SQLError> {
        CatalogReadView::sql_functions(self, resolution, name)
    }

    fn unopenable_relation_with_control(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<
        Option<uqa_core::memory::Produced<uqa_sql::catalog::analysis::UnopenableRelation>>,
        SQLError,
    > {
        let selected = self.relation_kind_resolution_with_control(resolution, name, control)?;
        let super::RelationResolution::Found(canonical, "composite type") = &*selected else {
            return Ok(None);
        };
        let reference =
            uqa_core::RelationIdentity::try_borrowed_reference(canonical).map_err(|error| {
                control
                    .format(format_args!("{error}"))
                    .map(|text| SQLError::Internal(text.into_parts().0))
                    .unwrap_or_else(Into::into)
            })?;
        let (name, memory) = control
            .format(format_args!("{}", reference.name))?
            .into_parts();
        Ok(Some(control.finish(
            uqa_sql::catalog::analysis::UnopenableRelation {
                name,
                kinds: "composite types",
            },
            memory,
        )?))
    }

    fn unopenable_relation(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<uqa_sql::catalog::analysis::UnopenableRelation>, SQLError> {
        Ok(
            match self
                .relation_kind_resolution(resolution, name)?
                .into_found()
            {
                Some((canonical, "composite type")) => {
                    let relation = uqa_core::RelationIdentity::from_legacy_name(&canonical)
                        .map_err(SQLError::Internal)?;
                    Some(uqa_sql::catalog::analysis::UnopenableRelation {
                        name: relation.name,
                        kinds: "composite types",
                    })
                }
                _ => None,
            },
        )
    }
}

#[cfg(test)]
mod tests;
