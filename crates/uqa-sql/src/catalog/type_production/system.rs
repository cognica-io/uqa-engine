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

pub fn system_catalog_domain_with_control(
    oid: u32,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, ValueRetentionError> {
    control.check()?;
    let information = match oid {
        13_307 => Some(("cardinal_number", ColumnType::Integer)),
        13_310 => Some(("character_data", ColumnType::Varchar(None))),
        13_312 => Some(("sql_identifier", ColumnType::Name)),
        13_318 => Some(("time_stamp", ColumnType::TimestampTz)),
        13_320 => Some(("yes_or_no", ColumnType::Varchar(Some(3)))),
        _ => None,
    };
    if let Some((name, base)) = information {
        return ColumnType::domain_with_control(
            "information_schema",
            name,
            oid,
            Some(oid - 1),
            None,
            &base,
            control,
        )
        .map(Some);
    }
    for (name, qualified, base) in [
        ("label_id", "ag_catalog.label_id", ColumnType::Integer),
        (
            "label_kind",
            "ag_catalog.label_kind",
            ColumnType::InternalChar,
        ),
        ("graphid", "ag_catalog.graphid", ColumnType::BigInteger),
        ("agtype", "ag_catalog.agtype", ColumnType::JsonB),
    ] {
        control.check()?;
        if i64::from(oid) == crate::catalog::oids::stable_oid("type", qualified) {
            return ColumnType::domain_with_control(
                super::super::AG_CATALOG_SCHEMA,
                name,
                oid,
                None,
                None,
                &base,
                control,
            )
            .map(Some);
        }
    }
    Ok(None)
}
