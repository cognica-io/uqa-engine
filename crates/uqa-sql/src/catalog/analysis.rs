//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog descriptions required by static SQL binding.

use super::resolution::RelationNameResolution;
use crate::ast::ColumnDef;
use crate::plan::QueryPlan;
use crate::routines::SQLUserFunction;
use crate::{ColumnType, SQLError};
use std::sync::Arc;
mod owned;

pub type VirtualRelationSchema = Vec<(String, ColumnType)>;

#[derive(Clone)]
pub struct TableDefinition {
    pub columns: Arc<Vec<ColumnDef>>,
    pub columns_declared: bool,
}

#[derive(Clone)]
pub struct ViewDefinition {
    pub query: QueryPlan,
    pub output_columns: Option<Vec<String>>,
    pub materialized: bool,
    pub materialized_column_types: Vec<Option<ColumnType>>,
    pub materialized_column_collations: Option<Vec<u32>>,
}

#[derive(Clone, Copy)]
pub struct ViewDefinitionRef<'a> {
    pub query: &'a QueryPlan,
    pub output_columns: Option<&'a [String]>,
    pub materialized: bool,
    pub materialized_column_types: &'a [Option<ColumnType>],
    pub materialized_column_collations: Option<&'a [u32]>,
}

impl<'a> From<&'a ViewDefinition> for ViewDefinitionRef<'a> {
    fn from(view: &'a ViewDefinition) -> Self {
        Self {
            query: &view.query,
            output_columns: view.output_columns.as_deref(),
            materialized: view.materialized,
            materialized_column_types: &view.materialized_column_types,
            materialized_column_collations: view.materialized_column_collations.as_deref(),
        }
    }
}

/// Immutable namespace-aware lookup for SQL analysis. This contract exposes definitions only; it grants no row access, mutation, locking, or transaction services.
pub trait AnalysisCatalog: Send + Sync {
    /// The actual row type of a selected catalog relation. Anonymous query sources and catalog fixtures without identities leave this unavailable.
    fn relation_row_type(
        &self,
        _resolution: &RelationNameResolution,
        _name: &str,
    ) -> Result<Option<crate::ast::CompositeTypeReference>, SQLError> {
        Ok(None)
    }

    fn relation_row_type_with_control(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<uqa_core::memory::Produced<crate::ast::CompositeTypeReference>>, SQLError>
    {
        control.check()?;
        self.relation_row_type(resolution, name)?
            .map(|ty| ty.clone_with_control(control).map_err(Into::into))
            .transpose()
    }

    fn sequence_exists_with_control(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        control.check()?;
        let exists = self.sequence_exists(resolution, name)?;
        control.check()?;
        Ok(exists)
    }

    fn virtual_relation_schema_with_control(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<uqa_core::memory::Produced<VirtualRelationSchema>>, SQLError> {
        control.check()?;
        let Some(schema) = self.virtual_relation_schema(resolution, name)? else {
            return Ok(None);
        };
        let mut output = uqa_core::memory::ProductionVec::new(*control);
        for (name, ty) in &schema {
            let (name, name_memory) = control.copy_text(name)?.into_parts();
            let (ty, type_memory) = ty.clone_with_control(control)?.into_parts();
            output.push_produced(
                control.finish((name, ty), control.combine(name_memory, type_memory))?,
            )?;
        }
        Ok(Some(output.finish()?))
    }

    /// Exact revisions of the requested identities in this catalog view. Return `None` only when the catalog has no revision capability; within an available snapshot, include every requested identity and use a missing value for an absent object.
    fn prepared_dependency_snapshot(
        &self,
        _dependencies: &crate::prepared::dependencies::PreparedAnalysisDependencies,
    ) -> Result<Option<crate::prepared::dependencies::PreparedDependencySnapshot>, SQLError> {
        Ok(None)
    }

    fn prepared_dependency_snapshot_with_control(
        &self,
        dependencies: &crate::prepared::dependencies::PreparedAnalysisDependencies,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<crate::prepared::dependencies::PreparedDependencySnapshot>, SQLError> {
        control.check()?;
        self.prepared_dependency_snapshot(dependencies)?
            .map(|snapshot| snapshot.copy_with_control(control))
            .transpose()
    }

    /// The OID of a relation successfully selected in this same immutable namespace. The binder calls this after selecting a catalog relation, never for a CTE or an unsuccessful lookup candidate. Catalog fixtures without identities may leave this unavailable.
    fn relation_dependency(
        &self,
        _resolution: &RelationNameResolution,
        _name: &str,
    ) -> Result<Option<u32>, SQLError> {
        Ok(None)
    }

    /// Observe a selected relation identity within the caller's planning control.
    fn relation_dependency_with_control(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<u32>, SQLError> {
        control.check()?;
        let dependency = self.relation_dependency(resolution, name)?;
        control.check()?;
        Ok(dependency)
    }

    /// The schemas that exist and that the analyzing role may use, including implicit namespaces, and its creation namespace. Catalogs without namespace state leave this unavailable.
    fn effective_search_path(
        &self,
        _resolution: &RelationNameResolution,
    ) -> Result<Option<super::resolution::EffectiveSearchPath>, SQLError> {
        Ok(None)
    }

    fn effective_search_path_with_control(
        &self,
        resolution: &RelationNameResolution,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<uqa_core::memory::Produced<Option<super::resolution::EffectiveSearchPath>>, SQLError>
    {
        use uqa_core::memory::ProductionVec;
        control.check()?;
        let Some(path) = self.effective_search_path(resolution)? else {
            return Ok(control.finish(None, control.empty_reservation())?);
        };
        let mut schemas = ProductionVec::new(*control);
        for schema in &path.schemas {
            schemas.push_produced(control.copy_text(schema)?)?;
        }
        let creation = path
            .creation_namespace
            .as_ref()
            .map(|name| control.copy_text(name))
            .transpose()?;
        let (schemas, schema_memory) = schemas.finish()?.into_parts();
        let (creation_namespace, creation_memory) = creation.map_or_else(
            || (None, control.empty_reservation()),
            |name| {
                let (name, memory) = name.into_parts();
                (Some(name), memory)
            },
        );
        Ok(control.finish(
            Some(super::resolution::EffectiveSearchPath {
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
    ) -> Result<Option<TableDefinition>, SQLError>;
    /// Native implementations share already retained declarations and admit namespace lookup scratch before producing it.
    fn table_resolved_with_control(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<TableDefinition>, SQLError> {
        control.check()?;
        let table = self.table_resolved(resolution, name)?;
        control.check()?;
        Ok(table)
    }

    fn table_name_resolved_with_control(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<uqa_core::memory::Produced<String>>, SQLError> {
        control.check()?;
        self.table_name_resolved(resolution, name)?
            .map(|name| control.copy_text(&name).map_err(Into::into))
            .transpose()
    }

    fn table_name_resolved(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<String>, SQLError>;
    fn view_resolved(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<ViewDefinition>, SQLError>;

    /// Native catalogs lend one selected retained declaration; the default preserves ordinary catalog adapters.
    fn with_view_definition_with_control(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
        read: &mut dyn FnMut(ViewDefinitionRef<'_>) -> Result<(), SQLError>,
    ) -> Result<(), SQLError> {
        control.check()?;
        if let Some(view) = self.view_resolved(resolution, name)? {
            read((&view).into())?;
        }
        control.check()?;
        Ok(())
    }

    /// Native catalogs lend column declarations without copying foreign options or unrelated metadata.
    fn with_foreign_table_columns_with_control(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
        read: &mut dyn FnMut(&[ColumnDef]) -> Result<(), SQLError>,
    ) -> Result<(), SQLError> {
        control.check()?;
        if let Some(table) = self.foreign_table_resolved(resolution, name)? {
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
        control.check()?;
        let Some(schema) = self.virtual_relation_schema(resolution, name)? else {
            return Ok(None);
        };
        let mut columns = uqa_core::memory::ProductionVec::new(*control);
        for (name, _) in &schema {
            columns.push_produced(control.copy_text(name)?)?;
        }
        Ok(Some(columns.finish()?))
    }
    fn foreign_table_resolved(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<TableDefinition>, SQLError>;
    fn sequence_exists(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<bool, SQLError>;
    fn virtual_relation_schema(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<Vec<(String, ColumnType)>>, SQLError>;
    fn sql_functions(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<Vec<Arc<SQLUserFunction>>>, SQLError>;

    fn unopenable_relation_with_control(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<uqa_core::memory::Produced<UnopenableRelation>>, SQLError> {
        control.check()?;
        self.unopenable_relation(resolution, name)?
            .map(|relation| {
                let (name, memory) = control.copy_text(&relation.name)?.into_parts();
                Ok(control.finish(
                    UnopenableRelation {
                        name,
                        kinds: relation.kinds,
                    },
                    memory,
                )?)
            })
            .transpose()
    }

    /// A relation that a query cannot open, such as a composite type's relation: its unqualified name and the kind `PostgreSQL`'s detail names.
    fn unopenable_relation(
        &self,
        _resolution: &RelationNameResolution,
        _name: &str,
    ) -> Result<Option<UnopenableRelation>, SQLError> {
        Ok(None)
    }
}

/// `errdetail_relkind_not_supported`: the detail naming a relation kind that an operation refuses.
#[must_use]
pub fn relkind_not_supported_detail(kind: &str) -> Option<String> {
    let plural = match kind {
        "table" => "tables",
        "index" => "indexes",
        "sequence" => "sequences",
        "view" => "views",
        "materialized view" => "materialized views",
        "composite type" => "composite types",
        "foreign table" => "foreign tables",
        "partitioned table" => "partitioned tables",
        _ => return None,
    };
    Some(format!("This operation is not supported for {plural}."))
}

/// A relation that holds no rows a query or command can read or change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnopenableRelation {
    pub name: String,
    /// The relation kind in the plural, as `errdetail_relkind_not_supported` names it.
    pub kinds: &'static str,
}

impl UnopenableRelation {
    pub fn error_with_control(
        &self,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> SQLError {
        let output = (|| {
            let message = control.format(format_args!("cannot open relation \"{}\"", self.name))?;
            let detail = control.format(format_args!(
                "This operation is not supported for {}.",
                self.kinds
            ))?;
            Ok::<_, uqa_core::ValueRetentionError>(SQLError::Diagnostic {
                sqlstate: "42809".into(),
                message: message.into_parts().0,
                detail: Some(detail.into_parts().0),
                hint: None,
            })
        })();
        output.unwrap_or_else(Into::into)
    }

    /// `table_open`'s refusal of a relation that is not a table.
    #[must_use]
    pub fn error(&self) -> SQLError {
        SQLError::Diagnostic {
            sqlstate: "42809".into(),
            message: format!("cannot open relation \"{}\"", self.name),
            detail: Some(format!(
                "This operation is not supported for {}.",
                self.kinds
            )),
            hint: None,
        }
    }
}

pub type CatalogReadView = Arc<dyn AnalysisCatalog>;
