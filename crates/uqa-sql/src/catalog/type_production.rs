//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog type projections admit their destination names and nested payloads before construction.

use super::{
    composite_type::StoredComposite, domain::StoredDomain, enum_type::StoredEnum,
    range_type::StoredRange, relation_oids::RelationCatalogOids,
};
mod system;
use crate::ast::{
    ColumnType, CompositeTypeReference, EnumTypeReference, RangeAlignment, RangeTypeReference,
};
pub use system::system_catalog_domain_with_control;
use uqa_core::{
    memory::{Produced, ProductionControl},
    RelationIdentity, ValueRetentionError,
};

fn names(
    identity: &RelationIdentity,
    control: &ProductionControl<'_>,
) -> Result<Produced<(String, String)>, ValueRetentionError> {
    let schema = control.copy_text(&identity.schema)?;
    let name = control.copy_text(&identity.name)?;
    let (schema, schema_memory) = schema.into_parts();
    let (name, name_memory) = name.into_parts();
    control.finish((schema, name), control.combine(schema_memory, name_memory))
}

impl<Owner> StoredDomain<Owner> {
    pub fn column_type_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<ColumnType>, ValueRetentionError> {
        ColumnType::domain_with_control(
            &self.identity.schema,
            &self.identity.name,
            self.oid,
            self.array_oid,
            self.definition.collation_oid,
            &self.definition.base,
            control,
        )
    }
}

impl<Owner> StoredEnum<Owner> {
    pub fn column_type_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<ColumnType>, ValueRetentionError> {
        let ((schema, name), memory) = names(&self.identity, control)?.into_parts();
        control.finish(
            ColumnType::Enum(EnumTypeReference {
                schema,
                name,
                oid: self.oid,
                array_oid: self.array_oid,
            }),
            memory,
        )
    }
}

impl<Owner> StoredComposite<Owner> {
    pub fn column_type_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<ColumnType>, ValueRetentionError> {
        let ((schema, name), memory) = names(&self.identity, control)?.into_parts();
        control.finish(
            ColumnType::Composite(CompositeTypeReference {
                schema,
                name,
                oid: self.oid,
                array_oid: self.array_oid,
                relation_oid: self.relation_oid,
            }),
            memory,
        )
    }
}

impl StoredRange {
    pub fn column_type_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<ColumnType>, ValueRetentionError> {
        self.project_type(false, control)
    }

    pub fn multirange_type_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<ColumnType>, ValueRetentionError> {
        self.project_type(true, control)
    }

    fn project_type(
        &self,
        multirange: bool,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<ColumnType>, ValueRetentionError> {
        let (identity, oid, array_oid) = if multirange {
            (
                &self.multirange.identity,
                self.multirange.oid,
                self.multirange.array_oid,
            )
        } else {
            (&self.identity, self.oid, self.array_oid)
        };
        let ((schema, name), memory) = names(identity, control)?.into_parts();
        let reference = RangeTypeReference {
            schema,
            name,
            oid,
            array_oid,
            alignment: RangeAlignment::for_endpoint(&self.subtype),
        };
        control.finish(
            if multirange {
                ColumnType::DeclaredMultirange(reference)
            } else {
                ColumnType::DeclaredRange(reference)
            },
            memory,
        )
    }
}

impl RelationCatalogOids {
    pub fn row_type_reference_with_control(
        &self,
        identity: &RelationIdentity,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<CompositeTypeReference>>, ValueRetentionError> {
        control.check()?;
        let Some(oid) = self.row_type else {
            return Ok(None);
        };
        let ((schema, name), memory) = names(identity, control)?.into_parts();
        control
            .finish(
                CompositeTypeReference {
                    schema,
                    name,
                    oid,
                    array_oid: self.array_type.unwrap_or(0),
                    relation_oid: self.relation,
                },
                memory,
            )
            .map(Some)
    }
}

#[cfg(test)]
mod tests;
