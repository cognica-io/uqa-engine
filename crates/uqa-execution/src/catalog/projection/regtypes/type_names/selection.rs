//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! One borrowed type selection feeds complete descriptors and stable routine identities.

use crate::catalog::{
    composite_type::relations::{self, NamedRelationType},
    selection_work::{checkpoint, equal, Work},
    CatalogReadView,
};
use uqa_core::{
    image_io::AsyncImageInput,
    memory::{Produced, ProductionControl},
};
use uqa_sql::{
    ast::{UserTypeIdentity, UserTypeKind},
    catalog::{
        composite_type::StoredComposite, domain::StoredDomain, enum_type::StoredEnum,
        range_type::StoredRange,
    },
    ColumnType, SQLError,
};

pub(super) enum Definition<'a> {
    Range(&'a StoredRange, bool),
    Domain(&'a StoredDomain),
    Enum(&'a StoredEnum),
    Composite(&'a StoredComposite),
    Relation(NamedRelationType<'a>),
}
pub(super) struct Selected<'a> {
    definition: Definition<'a>,
    array: bool,
}
impl Selected<'_> {
    pub(super) fn project(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<ColumnType>>, SQLError> {
        let ty = match &self.definition {
            Definition::Range(value, true) => value.multirange_type_with_control(control)?,
            Definition::Range(value, false) => value.column_type_with_control(control)?,
            Definition::Domain(value) => value.column_type_with_control(control)?,
            Definition::Enum(value) => value.column_type_with_control(control)?,
            Definition::Composite(value) => value.column_type_with_control(control)?,
            Definition::Relation(value) => return value.project(control),
        };
        super::production::array_type(ty, self.array, control).map(Some)
    }
    fn identity(&self) -> Option<UserTypeIdentity> {
        let (kind, oid, array) = match &self.definition {
            Definition::Range(value, true) => {
                (UserTypeKind::Multirange, value.multirange.oid, self.array)
            }
            Definition::Range(value, false) => (UserTypeKind::Range, value.oid, self.array),
            Definition::Domain(value) => (UserTypeKind::Domain, value.oid, self.array),
            Definition::Enum(value) => (UserTypeKind::Enum, value.oid, self.array),
            Definition::Composite(value) => (UserTypeKind::Composite, value.oid, self.array),
            Definition::Relation(value) => {
                (UserTypeKind::Composite, value.oids.row_type?, value.array)
            }
        };
        Some(UserTypeIdentity {
            kind,
            oid,
            dimensions: usize::from(array),
        })
    }
}

/// Select only the identity of a qualified native user type. No domain base type, descriptor or relation columns are copied.
pub async fn catalog_user_type_name_identity_from_input(
    catalog: &CatalogReadView,
    schema: &str,
    name: &str,
    control: &ProductionControl<'_>,
    input: &mut AsyncImageInput<'_>,
) -> Result<Option<UserTypeIdentity>, SQLError> {
    control.check()?;
    let selected = select(catalog, schema, name, control, input)
        .await?
        .and_then(|value| value.identity());
    control.check()?;
    Ok(selected)
}

pub(super) async fn select<'a>(
    catalog: &'a CatalogReadView,
    schema: &str,
    name: &str,
    control: &ProductionControl<'_>,
    work: &mut impl Work,
) -> Result<Option<Selected<'a>>, SQLError> {
    for definition in catalog.ranges() {
        checkpoint(control, work).await?;
        for (identity, array, multi) in [
            (&definition.identity, &definition.array_name, false),
            (
                &definition.multirange.identity,
                &definition.multirange.array_name,
                true,
            ),
        ] {
            if !equal(&identity.schema, schema, control, work).await? {
                continue;
            }
            let base = equal(&identity.name, name, control, work).await?;
            let array = equal(array, name, control, work).await?;
            if base || array {
                return Ok(Some(Selected {
                    definition: Definition::Range(definition, multi),
                    array,
                }));
            }
        }
    }
    for definition in catalog.domains() {
        checkpoint(control, work).await?;
        if !equal(&definition.identity.schema, schema, control, work).await? {
            continue;
        }
        let array = match &definition.array_name {
            Some(array) => equal(array, name, control, work).await?,
            None => uqa_sql::catalog::array_type_names::default_array_type_name_matches(
                &definition.identity.name,
                name,
            ),
        };
        let base = equal(&definition.identity.name, name, control, work).await?;
        if base || array {
            return Ok(Some(Selected {
                definition: Definition::Domain(definition),
                array: !base,
            }));
        }
    }
    for definition in catalog.enums() {
        checkpoint(control, work).await?;
        if !equal(&definition.identity.schema, schema, control, work).await? {
            continue;
        }
        let base = equal(&definition.identity.name, name, control, work).await?;
        if base || equal(&definition.array_name, name, control, work).await? {
            return Ok(Some(Selected {
                definition: Definition::Enum(definition),
                array: !base,
            }));
        }
    }
    for definition in catalog.composites() {
        checkpoint(control, work).await?;
        if !equal(&definition.identity.schema, schema, control, work).await? {
            continue;
        }
        let base = equal(&definition.identity.name, name, control, work).await?;
        if base || equal(&definition.array_name, name, control, work).await? {
            return Ok(Some(Selected {
                definition: Definition::Composite(definition),
                array: !base,
            }));
        }
    }
    Ok(
        relations::select_name_from(catalog, schema, name, control, work)
            .await?
            .map(|value| Selected {
                definition: Definition::Relation(value),
                array: false,
            }),
    )
}
