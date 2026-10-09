//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Static `pg_type` and `pg_range` projection.

mod builtins;
mod polymorphic;
mod special;
pub(in crate::catalog::projection) use builtins::builtin_type_oid_in_use;
mod routine_internal;

#[cfg(test)]
thread_local! {
    pub(crate) static TYPE_PROJECTION_BUILDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

use uqa_core::Value;
use uqa_sql::ast::{ColumnType, RangeSubtype};
use uqa_sql::ResultRow;

use crate::catalog::CatalogReadView;

use super::super::helpers::oids::{current_user_oid, namespace_oid, schema_oid};
use super::super::helpers::rows::{bool_value, int_value, row, str_value};
use super::super::helpers::type_metadata::{
    pg_type_align, pg_type_array_oid, pg_type_by_value, pg_type_collation_oid, pg_type_element_oid,
    pg_type_len, pg_type_oid, pg_type_routine_oids, pg_type_storage, pg_type_subscript_handler,
    PgTypeRoutineOids,
};

pub fn build_pg_type(
    output: Option<&dyn uqa_sql::expr::EngineHook>,
    catalog: &CatalogReadView,
    resolution: &crate::catalog::RelationNameResolution,
) -> Result<Vec<ResultRow>, uqa_sql::SQLError> {
    build_pg_type_rows(output, catalog, resolution, true)
}

/// The `pg_type` rows with `typdefault` left NULL, for the `reg*` output catalog: printing a domain default may print a `reg*` constant, whose output function reads that catalog.
pub fn build_pg_type_without_defaults(
    catalog: &CatalogReadView,
    resolution: &crate::catalog::RelationNameResolution,
) -> Result<Vec<ResultRow>, uqa_sql::SQLError> {
    build_pg_type_rows(None, catalog, resolution, false)
}

#[expect(
    clippy::too_many_lines,
    reason = "preserves catalog column and OID order"
)]
fn build_pg_type_rows(
    output: Option<&dyn uqa_sql::expr::EngineHook>,
    catalog: &CatalogReadView,
    resolution: &crate::catalog::RelationNameResolution,
    with_defaults: bool,
) -> Result<Vec<ResultRow>, uqa_sql::SQLError> {
    #[cfg(test)]
    TYPE_PROJECTION_BUILDS.set(TYPE_PROJECTION_BUILDS.get() + 1);
    let catalog_types = builtins::CATALOG_TYPES;
    let mut types = catalog_types
        .iter()
        .cloned()
        .chain(
            catalog_types
                .iter()
                .filter(|&(ty, _, _, kind)| {
                    matches!(*kind, "b" | "r" | "m") && pg_type_array_oid(ty) != 0
                })
                .cloned()
                .map(|(ty, _, _, _)| (ColumnType::Array(Box::new(ty)), "A", false, "b")),
        )
        .map(|(ty, category, preferred, kind)| {
            pg_type_catalog_row(
                &ty,
                schema_oid("pg_catalog"),
                kind,
                category,
                preferred,
                0,
                -1,
            )
        })
        .collect::<Vec<_>>();
    types.extend(polymorphic::metadata().map(special_pg_type_catalog_row));
    types.extend(routine_internal::metadata().map(special_pg_type_catalog_row));
    for domain in super::super::schema::information_schema_domains() {
        let ColumnType::Domain { oid, base, .. } = &domain else {
            unreachable!("information schema type constructor returned a non-domain")
        };
        let (category, type_modifier) = match *oid {
            13_307 => ("N", -1),
            13_310 | 13_312 => ("S", -1),
            13_318 => ("D", 2),
            13_320 => ("S", 7),
            _ => unreachable!("unknown PostgreSQL 18 information schema domain {oid}"),
        };
        types.push(pg_type_catalog_row(
            &domain,
            schema_oid("information_schema"),
            "d",
            category,
            false,
            pg_type_oid(base),
            type_modifier,
        ));
        types.push(pg_type_catalog_row(
            &ColumnType::Array(Box::new(domain)),
            schema_oid("information_schema"),
            "b",
            "A",
            false,
            0,
            -1,
        ));
    }
    for domain in super::super::schema::ag_catalog_domains() {
        let ColumnType::Domain { name, base, .. } = &domain else {
            unreachable!("ag_catalog type constructor returned a non-domain")
        };
        let category = match name.as_str() {
            "label_id" => "N",
            "label_kind" => "Z",
            other => unreachable!("unknown ag_catalog domain {other}"),
        };
        types.push(pg_type_catalog_row(
            &domain,
            schema_oid(super::super::schema::AG_CATALOG_SCHEMA),
            "d",
            category,
            false,
            pg_type_oid(base),
            -1,
        ));
        types.push(pg_type_catalog_row(
            &ColumnType::Array(Box::new(domain)),
            schema_oid(super::super::schema::AG_CATALOG_SCHEMA),
            "b",
            "A",
            false,
            0,
            -1,
        ));
    }
    types.extend(super::super::ag_catalog::age_pg_type_rows());
    types.extend(special::metadata().map(special_pg_type_catalog_row));
    for domain in catalog.domains() {
        let ty = domain.column_type();
        let owner = domain.owner.oid;
        let base = &domain.definition.base;
        let mut scalar = base;
        while let ColumnType::Domain { base, .. } = scalar {
            scalar = base;
        }
        let category = types
            .iter()
            .find(|entry| entry.get("oid") == Some(&int_value(pg_type_oid(scalar))))
            .and_then(|entry| entry.get("typcategory"))
            .cloned()
            .unwrap_or_else(|| str_value("U"));
        let mut entry = pg_type_catalog_row(
            &ty,
            namespace_oid(catalog, &domain.identity.schema),
            "d",
            "U",
            false,
            pg_type_oid(base),
            super::super::helpers::type_metadata::pg_type_modifier(base),
        );
        entry.insert("typcategory".into(), category);
        entry.insert("typowner".into(), int_value(owner));
        entry.insert(
            "typnotnull".into(),
            bool_value(domain.definition.not_null.is_some()),
        );
        // `typdefaultbin` is the stored default, which `pg_get_expr` prints; `typdefault` its text.
        let default = match domain.definition.default.as_ref().filter(|_| with_defaults) {
            Some(default) => str_value(super::super::view_definition::stored_expression_text(
                output, catalog, resolution, default,
            )?),
            None => Value::Null,
        };
        entry.insert("typdefaultbin".into(), default.clone());
        entry.insert("typdefault".into(), default);
        entry.insert(
            "typacl".into(),
            type_acl_value(catalog, domain.usage_acl.as_deref())?,
        );
        types.push(entry);
        let mut array = pg_type_catalog_row(
            &ColumnType::Array(Box::new(ty)),
            namespace_oid(catalog, &domain.identity.schema),
            "b",
            "A",
            false,
            0,
            -1,
        );
        array.insert("typname".into(), str_value(domain.array_type_name()));
        array.insert("typowner".into(), int_value(owner));
        types.push(array);
    }
    types.extend(super::composites::composite_type_rows(catalog)?);
    types.extend(super::row_types::relation_type_rows(catalog));
    types.extend(super::languages::language_type_rows(catalog));
    types.extend(super::foreign::type_rows(catalog));
    for definition in catalog.enums() {
        let ty = definition.column_type();
        let owner = int_value(definition.owner.oid);
        let namespace = namespace_oid(catalog, &definition.identity.schema);
        let mut entry = pg_type_catalog_row(&ty, namespace, "e", "E", false, 0, -1);
        entry.insert("typowner".into(), owner.clone());
        entry.insert(
            "typacl".into(),
            type_acl_value(catalog, definition.usage_acl.as_deref())?,
        );
        types.push(entry);
        let mut array = pg_type_catalog_row(
            &ColumnType::Array(Box::new(ty)),
            namespace,
            "b",
            "A",
            false,
            0,
            -1,
        );
        array.insert("typname".into(), str_value(definition.array_name.clone()));
        array.insert("typowner".into(), owner);
        types.push(array);
    }
    types.sort_by_key(|entry| match entry.get("oid") {
        Some(Value::Int(oid)) => *oid,
        _ => i64::MAX,
    });
    Ok(types)
}

/// `typacl`: NULL for the default ACL; array types have none of their own.
pub(super) fn type_acl_value(
    catalog: &CatalogReadView,
    acl: Option<&[uqa_sql::ast::ObjectAclEntry]>,
) -> Result<Value, uqa_sql::SQLError> {
    let Some(acl) = acl else {
        return Ok(Value::Null);
    };
    super::super::helpers::rows::catalog_array(
        super::super::helpers::acl::object_acl_items(
            &catalog.snapshot().definitions.roles,
            acl,
            'U',
            "type",
        )?,
        "pg_type.typacl",
    )
}

/// One row per label of every enum, with `PostgreSQL`'s float4 sort position.
pub fn build_pg_enum(catalog: &CatalogReadView) -> Vec<ResultRow> {
    let mut rows = Vec::new();
    for definition in catalog.enums() {
        for label in &definition.labels {
            rows.push(row([
                ("oid", int_value(i64::from(label.oid))),
                ("enumtypid", int_value(i64::from(definition.oid))),
                ("enumsortorder", Value::Float(f64::from(label.sort_order))),
                ("enumlabel", str_value(label.label.clone())),
            ]));
        }
    }
    rows.sort_by_key(|entry| match entry.get("oid") {
        Some(Value::Int(oid)) => *oid,
        _ => i64::MAX,
    });
    rows
}

pub fn build_pg_range() -> Vec<ResultRow> {
    [
        (RangeSubtype::Integer, 1_978, 3_914, 3_922),
        (RangeSubtype::Numeric, 3_125, 0, 3_924),
        (RangeSubtype::Timestamp, 3_128, 0, 3_929),
        (RangeSubtype::TimestampTz, 3_127, 0, 3_930),
        (RangeSubtype::Date, 3_122, 3_915, 3_925),
        (RangeSubtype::BigInteger, 3_124, 3_928, 3_923),
    ]
    .into_iter()
    .map(|(subtype, subtype_opclass, canonical, subtype_diff)| {
        row([
            (
                "rngtypid",
                int_value(pg_type_oid(&ColumnType::Range(subtype))),
            ),
            ("rngsubtype", int_value(pg_type_oid(&subtype.scalar_type()))),
            (
                "rngmultitypid",
                int_value(pg_type_oid(&ColumnType::Multirange(subtype))),
            ),
            ("rngcollation", int_value(0)),
            ("rngsubopc", int_value(subtype_opclass)),
            ("rngcanonical", int_value(canonical)),
            ("rngsubdiff", int_value(subtype_diff)),
        ])
    })
    .collect()
}

struct PgTypeCatalogMetadata<'a> {
    oid: i64,
    name: String,
    namespace_oid: i64,
    len: i64,
    by_value: bool,
    kind: &'a str,
    category: &'a str,
    preferred: bool,
    relation_oid: i64,
    subscript: i64,
    element_oid: i64,
    array_oid: i64,
    routines: PgTypeRoutineOids,
    align: &'a str,
    storage: &'a str,
    base_oid: i64,
    type_modifier: i64,
    collation_oid: i64,
}

pub(super) fn pg_type_catalog_row(
    ty: &ColumnType,
    namespace_oid: i64,
    kind: &str,
    category: &str,
    preferred: bool,
    base_oid: i64,
    type_modifier: i64,
) -> ResultRow {
    special_pg_type_catalog_row(PgTypeCatalogMetadata {
        oid: pg_type_oid(ty),
        name: super::super::helpers::information_schema_types::info_udt_name(ty),
        namespace_oid,
        len: pg_type_len(ty),
        by_value: pg_type_by_value(ty),
        kind,
        category,
        preferred,
        relation_oid: 0,
        subscript: pg_type_subscript_handler(ty),
        element_oid: pg_type_element_oid(ty),
        array_oid: pg_type_array_oid(ty),
        routines: pg_type_routine_oids(ty),
        align: pg_type_align(ty),
        storage: pg_type_storage(ty),
        base_oid,
        type_modifier,
        collation_oid: pg_type_collation_oid(ty),
    })
}

fn special_pg_type_catalog_row(metadata: PgTypeCatalogMetadata<'_>) -> ResultRow {
    row([
        ("oid", int_value(metadata.oid)),
        ("typname", str_value(metadata.name)),
        ("typnamespace", int_value(metadata.namespace_oid)),
        ("typowner", int_value(current_user_oid())),
        ("typlen", int_value(metadata.len)),
        ("typbyval", bool_value(metadata.by_value)),
        ("typtype", str_value(metadata.kind)),
        ("typcategory", str_value(metadata.category)),
        ("typispreferred", bool_value(metadata.preferred)),
        ("typisdefined", bool_value(true)),
        ("typdelim", str_value(",")),
        ("typrelid", int_value(metadata.relation_oid)),
        ("typsubscript", int_value(metadata.subscript)),
        ("typelem", int_value(metadata.element_oid)),
        ("typarray", int_value(metadata.array_oid)),
        ("typinput", int_value(metadata.routines.input)),
        ("typoutput", int_value(metadata.routines.output)),
        ("typreceive", int_value(metadata.routines.receive)),
        ("typsend", int_value(metadata.routines.send)),
        ("typmodin", int_value(metadata.routines.modifier_input)),
        ("typmodout", int_value(metadata.routines.modifier_output)),
        ("typanalyze", int_value(metadata.routines.analyze)),
        ("typalign", str_value(metadata.align)),
        ("typstorage", str_value(metadata.storage)),
        ("typnotnull", bool_value(false)),
        ("typbasetype", int_value(metadata.base_oid)),
        ("typtypmod", int_value(metadata.type_modifier)),
        ("typndims", int_value(0)),
        ("typcollation", int_value(metadata.collation_oid)),
        ("typdefaultbin", Value::Null),
        ("typdefault", Value::Null),
        ("typacl", Value::Null),
    ])
}
