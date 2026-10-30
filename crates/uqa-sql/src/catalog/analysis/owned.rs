//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A shared admitted catalog adapter preserves its immutable owner through every trait-object clone.

use super::{AnalysisCatalog, TableDefinition, ViewDefinition};
use crate::{catalog::resolution::RelationNameResolution, ColumnType, SQLError};
use std::sync::Arc;
use uqa_core::memory::Produced;

impl<T: AnalysisCatalog> AnalysisCatalog for Produced<T> {
    fn with_view_definition_with_control(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
        read: &mut dyn FnMut(super::ViewDefinitionRef<'_>) -> Result<(), SQLError>,
    ) -> Result<(), SQLError> {
        (**self).with_view_definition_with_control(resolution, name, control, read)
    }

    fn with_foreign_table_columns_with_control(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
        read: &mut dyn FnMut(&[crate::ast::ColumnDef]) -> Result<(), SQLError>,
    ) -> Result<(), SQLError> {
        (**self).with_foreign_table_columns_with_control(resolution, name, control, read)
    }

    fn virtual_relation_columns_with_control(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<Produced<Vec<String>>>, SQLError> {
        (**self).virtual_relation_columns_with_control(resolution, name, control)
    }

    fn relation_row_type(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<crate::ast::CompositeTypeReference>, SQLError> {
        (**self).relation_row_type(resolution, name)
    }

    fn relation_dependency(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<u32>, SQLError> {
        (**self).relation_dependency(resolution, name)
    }

    fn relation_dependency_with_control(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<u32>, SQLError> {
        (**self).relation_dependency_with_control(resolution, name, control)
    }

    fn prepared_dependency_snapshot(
        &self,
        dependencies: &crate::prepared::dependencies::PreparedAnalysisDependencies,
    ) -> Result<Option<crate::prepared::dependencies::PreparedDependencySnapshot>, SQLError> {
        (**self).prepared_dependency_snapshot(dependencies)
    }

    fn prepared_dependency_snapshot_with_control(
        &self,
        dependencies: &crate::prepared::dependencies::PreparedAnalysisDependencies,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<crate::prepared::dependencies::PreparedDependencySnapshot>, SQLError> {
        (**self).prepared_dependency_snapshot_with_control(dependencies, control)
    }

    fn effective_search_path(
        &self,
        resolution: &RelationNameResolution,
    ) -> Result<Option<crate::catalog::resolution::EffectiveSearchPath>, SQLError> {
        (**self).effective_search_path(resolution)
    }

    fn effective_search_path_with_control(
        &self,
        resolution: &RelationNameResolution,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Produced<Option<crate::catalog::resolution::EffectiveSearchPath>>, SQLError> {
        (**self).effective_search_path_with_control(resolution, control)
    }

    fn table_resolved(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<TableDefinition>, SQLError> {
        (**self).table_resolved(resolution, name)
    }

    fn table_resolved_with_control(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<TableDefinition>, SQLError> {
        (**self).table_resolved_with_control(resolution, name, control)
    }

    fn table_name_resolved_with_control(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<Produced<String>>, SQLError> {
        (**self).table_name_resolved_with_control(resolution, name, control)
    }

    fn table_name_resolved(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<String>, SQLError> {
        (**self).table_name_resolved(resolution, name)
    }

    fn view_resolved(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<ViewDefinition>, SQLError> {
        (**self).view_resolved(resolution, name)
    }

    fn foreign_table_resolved(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<TableDefinition>, SQLError> {
        (**self).foreign_table_resolved(resolution, name)
    }

    fn sequence_exists(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<bool, SQLError> {
        (**self).sequence_exists(resolution, name)
    }

    fn virtual_relation_schema(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<Vec<(String, ColumnType)>>, SQLError> {
        (**self).virtual_relation_schema(resolution, name)
    }

    fn sql_functions(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<Vec<Arc<crate::routines::SQLUserFunction>>>, SQLError> {
        (**self).sql_functions(resolution, name)
    }

    fn unopenable_relation(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<crate::catalog::analysis::UnopenableRelation>, SQLError> {
        (**self).unopenable_relation(resolution, name)
    }
}
