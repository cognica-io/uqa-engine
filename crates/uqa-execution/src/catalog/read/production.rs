//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Selected immutable table metadata shares its catalog owner while candidate and canonical names are admitted.

use super::{CatalogReadView, CatalogTableSnapshot, RelationNameResolution, SQLError};
use uqa_core::{
    memory::{Produced, ProductionControl},
    RelationName, RelationReference,
};

mod hierarchy;

impl CatalogReadView {
    /// Analysis selects a declaration without reading runtime sequence positions or security payloads.
    pub fn sequence_exists_with_control(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
        control: &ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        let candidates = self
            .relation_names()
            .relation_lookup_candidates_with_control(resolution, name, control)?;
        for relation in candidates.iter() {
            control.check()?;
            if self.snapshot.definitions.sequences.contains_key(relation) {
                return Ok(true);
            }
            if self.relation_exists(relation) {
                return Ok(false);
            }
        }
        Ok(false)
    }

    pub fn virtual_relation_resolved_with_control(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Option<uqa_sql::catalog::VirtualRelation>, SQLError> {
        let candidates = self
            .relation_names()
            .relation_lookup_candidates_with_control(resolution, name, control)?;
        for relation in candidates.iter() {
            control.check()?;
            if let Some(relation) =
                uqa_sql::catalog::VirtualRelation::at(&relation.schema, &relation.name)
            {
                return Ok(Some(relation));
            }
            if self.relation_exists(relation) {
                return Ok(None);
            }
        }
        Ok(None)
    }

    pub fn view_resolved_with_control(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Option<&crate::catalog::view::StoredView>, SQLError> {
        let candidates = self
            .relation_names()
            .relation_lookup_candidates_with_control(resolution, name, control)?;
        for relation in candidates.iter() {
            control.check()?;
            if let Some(view) = self.snapshot.definitions.views.get(relation) {
                return Ok(Some(view));
            }
            if self.relation_exists(relation) {
                return Ok(None);
            }
        }
        Ok(None)
    }

    pub fn view_name_resolved_with_control(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<String>>, SQLError> {
        let candidates = self
            .relation_names()
            .relation_lookup_candidates_with_control(resolution, name, control)?;
        for relation in candidates.iter() {
            control.check()?;
            if self.snapshot.definitions.views.contains_key(relation) {
                return Ok(Some(relation.qualified_name_with_control(control)?));
            }
            if self.relation_exists(relation) {
                return Ok(None);
            }
        }
        Ok(None)
    }

    pub fn foreign_table_resolved_with_control(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Option<&crate::catalog::foreign::StoredForeignTable>, SQLError> {
        let candidates = self
            .relation_names()
            .relation_lookup_candidates_with_control(resolution, name, control)?;
        for relation in candidates.iter() {
            control.check()?;
            if let Some(table) = self.snapshot.definitions.foreign_tables.get(relation) {
                return Ok(Some(table));
            }
            if self.relation_exists(relation) {
                return Ok(None);
            }
        }
        Ok(None)
    }
    pub fn table_resolved_with_control(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Option<&CatalogTableSnapshot>, SQLError> {
        let candidates = self
            .relation_names()
            .relation_lookup_candidates_with_control(resolution, name, control)?;
        for relation in candidates.iter() {
            control.check()?;
            if let Some(table) = self.snapshot.tables.get(relation) {
                return Ok(Some(table));
            }
            if self.relation_exists(relation) {
                return Ok(None);
            }
        }
        Ok(None)
    }

    pub fn table_name_resolved_with_control(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<String>>, SQLError> {
        let candidates = self
            .relation_names()
            .relation_lookup_candidates_with_control(resolution, name, control)?;
        for relation in candidates.iter() {
            control.check()?;
            if self.snapshot.tables.contains_key(relation) {
                return Ok(Some(
                    RelationReference {
                        schema: Some(RelationName::decoded(&relation.schema)),
                        name: RelationName::decoded(&relation.name),
                    }
                    .render_with_control(control)?,
                ));
            }
            if self.relation_exists(relation) {
                return Ok(None);
            }
        }
        Ok(None)
    }
}
