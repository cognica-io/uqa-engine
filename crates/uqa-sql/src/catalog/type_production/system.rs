//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Fixed catalog domains borrow declaration literals and project only the requested identity.

use crate::ColumnType;
use uqa_core::{
    memory::{Produced, ProductionControl},
    ValueRetentionError,
};

struct Domain {
    schema: &'static str,
    name: &'static str,
    oid: u32,
    array: Option<u32>,
    base: ColumnType,
}

fn domains() -> impl Iterator<Item = Domain> {
    [
        (13_307, "cardinal_number", ColumnType::Integer),
        (13_310, "character_data", ColumnType::Varchar(None)),
        (13_312, "sql_identifier", ColumnType::Name),
        (13_318, "time_stamp", ColumnType::TimestampTz),
        (13_320, "yes_or_no", ColumnType::Varchar(Some(3))),
    ]
    .into_iter()
    .map(|(oid, name, base)| Domain {
        schema: "information_schema",
        name,
        oid,
        array: Some(oid - 1),
        base,
    })
    .chain(
        [
            ("label_id", "ag_catalog.label_id", ColumnType::Integer),
            (
                "label_kind",
                "ag_catalog.label_kind",
                ColumnType::InternalChar,
            ),
            ("graphid", "ag_catalog.graphid", ColumnType::BigInteger),
            ("agtype", "ag_catalog.agtype", ColumnType::JsonB),
        ]
        .into_iter()
        .map(|(name, qualified, base)| Domain {
            schema: super::super::AG_CATALOG_SCHEMA,
            name,
            oid: u32::try_from(crate::catalog::oids::stable_oid("type", qualified))
                .expect("catalog OID"),
            array: None,
            base,
        }),
    )
}

pub(crate) fn is_system_catalog_type_oid(oid: u32) -> bool {
    domains().any(|domain| {
        domain.oid == oid
            || super::super::type_metadata::pg_domain_array_oid(domain.oid, domain.array)
                == i64::from(oid)
    })
}

pub fn system_catalog_domain_with_control(
    oid: u32,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, ValueRetentionError> {
    control.check()?;
    for domain in domains() {
        control.check()?;
        if domain.oid == oid {
            return ColumnType::domain_with_control(
                domain.schema,
                domain.name,
                oid,
                domain.array,
                None,
                &domain.base,
                control,
            )
            .map(Some);
        }
    }
    Ok(None)
}
