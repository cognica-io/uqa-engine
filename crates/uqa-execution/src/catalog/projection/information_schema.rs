//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Virtual `information_schema` relation builders.

mod foreign_tables;
mod identity;
mod routines;

use std::collections::BTreeSet;

use foreign_tables::insert_foreign_table_column_privileges;
use identity::{owned_identity_sequence, IdentityAttributes};
pub use routines::build_info_routines;

use super::helpers::constraints::{constraint_catalog_rows, ConstraintCatalogKind};
use super::helpers::information_schema_types::{
    info_character_maximum_length, info_character_octet_length, info_data_type,
    info_datetime_precision, info_interval_type, info_numeric_precision, info_numeric_scale,
    info_udt_name,
};
use super::helpers::oids::{current_user_name, split_schema_name};
use super::helpers::rows::{catalog_name, catalog_ordinal, int_value, row, str_value};
use super::helpers::views::view_columns_for;
use crate::catalog::context::CatalogContext;
use crate::catalog::{services::CatalogSession, CatalogReadView, RelationNameResolution};
use uqa_core::Value;
use uqa_sql::ast::ColumnDef as SQLColumnDef;
use uqa_sql::ast::ColumnType;
use uqa_sql::expr::value_to_text;
use uqa_sql::{ResultRow, SQLError};
use uqa_storage::SequenceOwnerDependency;
use uqa_storage::{TableAclEntry, TablePrivileges};

pub fn build_info_catalog_name() -> Vec<ResultRow> {
    vec![row([("catalog_name", catalog_name())])]
}

pub fn build_info_schemata(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
) -> Result<Vec<ResultRow>, SQLError> {
    let current_user = resolution.current_user();
    catalog
        .all_schema_names()
        .into_iter()
        .filter(|schema| {
            catalog.schema_has_privilege_to(
                schema,
                current_user,
                crate::catalog::security::schema::SchemaAclPrivilege::Usage,
            ) || catalog.schema_has_privilege_to(
                schema,
                current_user,
                crate::catalog::security::schema::SchemaAclPrivilege::Create,
            )
        })
        .map(|schema| {
            let owner = catalog.schema_security_names(&schema)?.map_or_else(
                || current_user_name().to_string(),
                |security| security.role_owner,
            );
            Ok(row([
                ("catalog_name", catalog_name()),
                ("schema_name", str_value(schema)),
                ("schema_owner", str_value(owner)),
                ("default_character_set_catalog", catalog_name()),
                ("default_character_set_schema", str_value("pg_catalog")),
                ("default_character_set_name", str_value("UTF8")),
                ("sql_path", Value::Null),
            ]))
        })
        .collect()
}

pub fn build_info_tables(
    context: &CatalogContext<'_>,
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
) -> Result<Vec<ResultRow>, SQLError> {
    let mut out = Vec::new();
    for name in catalog.table_names() {
        let table_snapshot = catalog
            .table(resolution, &name)?
            .ok_or_else(|| SQLError::UnknownTable(name.clone()))?;
        if !catalog.table_is_visible_to(table_snapshot, resolution.current_user()) {
            continue;
        }
        let (schema, table) = split_schema_name(&name)?;
        out.push(row([
            ("table_catalog", catalog_name()),
            ("table_schema", str_value(schema)),
            ("table_name", str_value(table)),
            ("table_type", str_value("BASE TABLE")),
            ("self_referencing_column_name", Value::Null),
            ("reference_generation", Value::Null),
            ("user_defined_type_catalog", Value::Null),
            ("user_defined_type_schema", Value::Null),
            ("user_defined_type_name", Value::Null),
            ("is_insertable_into", str_value("YES")),
            ("is_typed", str_value("NO")),
            ("commit_action", Value::Null),
        ]));
    }
    for (name, stored) in catalog.views_of_kind(crate::catalog::view::StoredViewKind::View) {
        if !catalog.view_is_visible_to(&stored, resolution.current_user()) {
            continue;
        }
        let (schema, view) = split_schema_name(&name)?;
        let updatability = context.views.view_updatability(&name)?;
        out.push(row([
            ("table_catalog", catalog_name()),
            ("table_schema", str_value(schema)),
            ("table_name", str_value(view)),
            ("table_type", str_value("VIEW")),
            ("self_referencing_column_name", Value::Null),
            ("reference_generation", Value::Null),
            ("user_defined_type_catalog", Value::Null),
            ("user_defined_type_schema", Value::Null),
            ("user_defined_type_name", Value::Null),
            (
                "is_insertable_into",
                str_value(if updatability.catalog.insertable {
                    "YES"
                } else {
                    "NO"
                }),
            ),
            ("is_typed", str_value("NO")),
            ("commit_action", Value::Null),
        ]));
    }
    for name in catalog.foreign_table_names() {
        if !catalog.foreign_table_is_visible_to(&name, resolution.current_user())? {
            continue;
        }
        let (schema, table) = split_schema_name(&name)?;
        out.push(row([
            ("table_catalog", catalog_name()),
            ("table_schema", str_value(schema)),
            ("table_name", str_value(table)),
            ("table_type", str_value("FOREIGN")),
            ("self_referencing_column_name", Value::Null),
            ("reference_generation", Value::Null),
            ("user_defined_type_catalog", Value::Null),
            ("user_defined_type_schema", Value::Null),
            ("user_defined_type_name", Value::Null),
            ("is_insertable_into", str_value("NO")),
            ("is_typed", str_value("NO")),
            ("commit_action", Value::Null),
        ]));
    }
    out.extend(super::ag_catalog::age_info_table_rows(catalog)?);
    let mut keyed = out
        .into_iter()
        .map(|row| {
            let schema = value_to_text(row.get("table_schema").unwrap_or(&Value::Null))?;
            let table = value_to_text(row.get("table_name").unwrap_or(&Value::Null))?;
            Ok(((schema, table), row))
        })
        .collect::<Result<Vec<_>, SQLError>>()?;
    keyed.sort_by(|(left, _), (right, _)| left.cmp(right));
    Ok(keyed.into_iter().map(|(_, row)| row).collect())
}

/// The schema and type name recorded for a column's declared type. User-defined types and their arrays name the catalog type, including a displaced array type name.
fn column_udt(catalog: &CatalogReadView, ty: &ColumnType) -> (String, String) {
    match ty {
        ColumnType::Enum(reference) => (reference.schema.clone(), reference.name.clone()),
        ColumnType::Domain { schema, name, .. } => (schema.clone(), name.clone()),
        ColumnType::Array(element) => match element.as_ref() {
            ColumnType::Enum(reference) => (
                reference.schema.clone(),
                catalog.enum_by_type_oid(reference.oid).map_or_else(
                    || info_udt_name(ty),
                    |definition| definition.array_name.clone(),
                ),
            ),
            ColumnType::Domain { oid, schema, .. } => (
                schema.clone(),
                catalog
                    .domains()
                    .find(|domain| domain.oid == *oid)
                    .map_or_else(
                        || info_udt_name(ty),
                        uqa_sql::catalog::domain::StoredDomain::array_type_name,
                    ),
            ),
            _ => ("pg_catalog".into(), info_udt_name(ty)),
        },
        _ => ("pg_catalog".into(), info_udt_name(ty)),
    }
}

/// How `information_schema.columns` describes a declared type: a domain column reports its base type as the data type and UDT and names the domain separately.
struct ColumnTypeDescription {
    data_type: String,
    udt: (String, String),
    domain: Option<(String, String)>,
}

fn describe_column_type(catalog: &CatalogReadView, ty: &ColumnType) -> ColumnTypeDescription {
    let ColumnType::Domain {
        schema, name, base, ..
    } = ty
    else {
        let data_type = if matches!(ty, ColumnType::Array(_)) {
            "ARRAY".to_string()
        } else if column_udt(catalog, ty).0 == "pg_catalog" {
            info_data_type(ty).to_string()
        } else {
            "USER-DEFINED".to_string()
        };
        return ColumnTypeDescription {
            data_type,
            udt: column_udt(catalog, ty),
            domain: None,
        };
    };
    let base = describe_column_type(catalog, base);
    ColumnTypeDescription {
        data_type: base.data_type,
        udt: base.udt,
        domain: Some((schema.clone(), name.clone())),
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "preserves catalog column and OID order"
)]
fn information_schema_column_row(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    (schema, table): (String, String),
    index: usize,
    column: &SQLColumnDef,
    updatable: bool,
    sequence: Option<&crate::catalog::sequence::SequenceState>,
) -> Result<ResultRow, SQLError> {
    let description = describe_column_type(catalog, &column.ty);
    let identity = IdentityAttributes::of(column, sequence);
    Ok(row([
        ("table_catalog", catalog_name()),
        ("table_schema", str_value(schema)),
        ("table_name", str_value(table)),
        ("column_name", str_value(column.name.clone())),
        (
            "ordinal_position",
            int_value(catalog_ordinal(index, "information_schema column")?),
        ),
        (
            "column_default",
            match column.default.as_ref() {
                Some(default) => str_value(super::view_definition::stored_expression_text(
                    catalog, resolution, default,
                )?),
                None => Value::Null,
            },
        ),
        (
            "is_nullable",
            str_value(if column.not_null || column.primary_key {
                "NO"
            } else {
                "YES"
            }),
        ),
        ("data_type", str_value(description.data_type.clone())),
        (
            "character_maximum_length",
            info_character_maximum_length(&column.ty),
        ),
        (
            "character_octet_length",
            info_character_octet_length(&column.ty),
        ),
        ("numeric_precision", info_numeric_precision(&column.ty)),
        ("numeric_precision_radix", Value::Int(10)),
        ("numeric_scale", info_numeric_scale(&column.ty)),
        ("datetime_precision", info_datetime_precision(&column.ty)),
        ("interval_type", info_interval_type(&column.ty)),
        ("interval_precision", Value::Null),
        ("character_set_catalog", Value::Null),
        ("character_set_schema", Value::Null),
        ("character_set_name", Value::Null),
        ("collation_catalog", Value::Null),
        ("collation_schema", Value::Null),
        ("collation_name", Value::Null),
        (
            "domain_catalog",
            description
                .domain
                .as_ref()
                .map_or(Value::Null, |_| catalog_name()),
        ),
        (
            "domain_schema",
            description
                .domain
                .as_ref()
                .map_or(Value::Null, |(schema, _)| str_value(schema.clone())),
        ),
        (
            "domain_name",
            description
                .domain
                .as_ref()
                .map_or(Value::Null, |(_, name)| str_value(name.clone())),
        ),
        ("udt_catalog", catalog_name()),
        ("udt_schema", str_value(description.udt.0.clone())),
        ("udt_name", str_value(description.udt.1.clone())),
        ("scope_catalog", Value::Null),
        ("scope_schema", Value::Null),
        ("scope_name", Value::Null),
        ("maximum_cardinality", Value::Null),
        ("dtd_identifier", str_value((index + 1).to_string())),
        (
            "is_self_referencing",
            str_value(if column.references.is_some() {
                "YES"
            } else {
                "NO"
            }),
        ),
        (
            "is_identity",
            str_value(
                if column
                    .auto_increment
                    .as_ref()
                    .is_some_and(|provenance| provenance.is_identity() || provenance.is_legacy())
                {
                    "YES"
                } else {
                    "NO"
                },
            ),
        ),
        (
            "identity_generation",
            match column.auto_increment.as_ref().map(|value| value.kind) {
                Some(uqa_sql::ast::AutoIncrementKind::IdentityAlways) => str_value("ALWAYS"),
                Some(
                    uqa_sql::ast::AutoIncrementKind::IdentityByDefault
                    | uqa_sql::ast::AutoIncrementKind::Legacy,
                ) => str_value("BY DEFAULT"),
                _ => Value::Null,
            },
        ),
        ("identity_start", identity.start),
        ("identity_increment", identity.increment),
        ("identity_maximum", identity.maximum),
        ("identity_minimum", identity.minimum),
        ("identity_cycle", identity.cycle),
        (
            "is_generated",
            str_value(if column.generated.is_some() {
                "ALWAYS"
            } else {
                "NEVER"
            }),
        ),
        (
            "generation_expression",
            match column.generated.as_ref() {
                Some(generated) => str_value(super::view_definition::stored_expression_text(
                    catalog,
                    resolution,
                    &generated.expression,
                )?),
                None => Value::Null,
            },
        ),
        (
            "is_updatable",
            str_value(if updatable { "YES" } else { "NO" }),
        ),
    ]))
}

pub fn build_info_columns(
    context: &CatalogContext<'_>,
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
) -> Result<Vec<ResultRow>, SQLError> {
    let mut out: Vec<ResultRow> = Vec::new();
    let sequences = catalog
        .sequence_definitions()?
        .into_iter()
        .map(|(relation, state, _, _)| (relation.qualified_name(), state))
        .collect::<std::collections::BTreeMap<_, _>>();
    for tname in catalog.table_names() {
        let table_snapshot = catalog
            .table(resolution, &tname)?
            .ok_or_else(|| SQLError::UnknownTable(tname.clone()))?;
        if !catalog.table_is_visible_to(table_snapshot, resolution.current_user()) {
            continue;
        }
        let cols = &table_snapshot.columns;
        let (schema, table) = split_schema_name(&tname)?;
        for (idx, col) in cols.iter().enumerate() {
            if !catalog.table_column_is_visible_to(
                table_snapshot,
                &col.name,
                resolution.current_user(),
            ) {
                continue;
            }
            out.push(information_schema_column_row(
                catalog,
                resolution,
                (schema.clone(), table.clone()),
                idx,
                col,
                true,
                owned_identity_sequence(&sequences, &tname, col)?,
            )?);
        }
    }
    for (view_name, stored) in catalog.views_of_kind(crate::catalog::view::StoredViewKind::View) {
        if !catalog.view_is_visible_to(&stored, resolution.current_user()) {
            continue;
        }
        let (schema, view) = split_schema_name(&view_name)?;
        let updatability = context.views.view_updatability(&view_name)?;
        let columns = view_columns_for(context, catalog, resolution, &stored)?;
        for (idx, column) in columns.iter().enumerate() {
            if !catalog.view_column_is_visible_to(&stored, &column.name, resolution.current_user())
            {
                continue;
            }
            out.push(information_schema_column_row(
                catalog,
                resolution,
                (schema.clone(), view.clone()),
                idx,
                column,
                updatability
                    .catalog_columns
                    .get(idx)
                    .copied()
                    .unwrap_or(false),
                None,
            )?);
        }
    }
    for (foreign_name, foreign_table) in catalog.foreign_tables() {
        if !catalog.foreign_table_is_visible_to(&foreign_name, resolution.current_user())? {
            continue;
        }
        let (schema, table) = split_schema_name(&foreign_name)?;
        for (idx, column) in foreign_table.columns.iter().enumerate() {
            if !catalog.foreign_table_column_is_visible_to(
                &foreign_name,
                &column.name,
                resolution.current_user(),
            )? {
                continue;
            }
            out.push(information_schema_column_row(
                catalog,
                resolution,
                (schema.clone(), table.clone()),
                idx,
                column,
                false,
                owned_identity_sequence(&sequences, &foreign_name, column)?,
            )?);
        }
    }
    out.extend(super::ag_catalog::age_info_column_rows(catalog)?);
    Ok(out)
}

type ColumnPrivilegeCatalogRow = (
    String,
    String,
    String,
    String,
    String,
    uqa_core::catalog_acl::AclGrantee,
    String,
    bool,
);

fn insert_column_privilege_rows(
    rows: &mut BTreeSet<ColumnPrivilegeCatalogRow>,
    schema: &str,
    table: &str,
    column: &str,
    owner: &str,
    entry: &TableAclEntry,
) {
    let grantor = entry.grantor.as_deref().unwrap_or(owner);
    for (privilege_type, granted, grantable) in [
        (
            "INSERT",
            entry.privileges.insert,
            entry.grant_options.insert,
        ),
        (
            "SELECT",
            entry.privileges.select,
            entry.grant_options.select,
        ),
        (
            "UPDATE",
            entry.privileges.update,
            entry.grant_options.update,
        ),
        (
            "REFERENCES",
            entry.privileges.references,
            entry.grant_options.references,
        ),
    ] {
        if granted {
            rows.insert((
                schema.to_string(),
                table.to_string(),
                column.to_string(),
                owner.to_string(),
                grantor.to_string(),
                entry.role.clone(),
                privilege_type.to_string(),
                grantable,
            ));
        }
    }
}

fn default_table_acl_entry(owner: &str) -> TableAclEntry {
    TableAclEntry {
        role: owner.into(),
        grantor: Some(owner.to_string()),
        privileges: TablePrivileges {
            select: true,
            insert: true,
            update: true,
            references: true,
            ..TablePrivileges::default()
        },
        grant_options: TablePrivileges::default(),
    }
}

fn insert_view_column_privileges(
    context: &CatalogContext<'_>,
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    privileges: &mut BTreeSet<ColumnPrivilegeCatalogRow>,
) -> Result<(), SQLError> {
    for (view_name, view) in catalog.views_of_kind(crate::catalog::view::StoredViewKind::View) {
        let (schema, table) = split_schema_name(&view_name)?;
        let columns = view_columns_for(context, catalog, resolution, &view)?;
        let security = catalog.relation_security_names(&view.security)?;
        let default_view_acl;
        let view_acl = if let Some(acl) = security.acl.as_deref() {
            acl
        } else {
            default_view_acl = [default_table_acl_entry(&security.role_owner)];
            &default_view_acl
        };
        for column in &columns {
            for entry in view_acl {
                insert_column_privilege_rows(
                    privileges,
                    &schema,
                    &table,
                    &column.name,
                    &security.role_owner,
                    entry,
                );
            }
            if let Some(column_acl) = security.column_acls.get(&column.name) {
                for entry in column_acl {
                    insert_column_privilege_rows(
                        privileges,
                        &schema,
                        &table,
                        &column.name,
                        &security.role_owner,
                        entry,
                    );
                }
            }
        }
    }
    Ok(())
}

pub fn build_info_column_privileges(
    context: &CatalogContext<'_>,
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    role_grants_only: bool,
) -> Result<Vec<ResultRow>, SQLError> {
    let current_user = resolution.current_user();
    let mut privileges = BTreeSet::new();
    for table_name in catalog.table_names() {
        let table_snapshot = catalog
            .table(resolution, &table_name)?
            .ok_or_else(|| SQLError::UnknownTable(table_name.clone()))?;
        let (schema, table) = split_schema_name(&table_name)?;
        let security = catalog.relation_security_names(&table_snapshot.security)?;
        let default_table_acl;
        let table_acl = if let Some(acl) = security.acl.as_deref() {
            acl
        } else {
            default_table_acl = [default_table_acl_entry(&security.role_owner)];
            &default_table_acl
        };
        for column in table_snapshot.columns.iter() {
            for entry in table_acl {
                insert_column_privilege_rows(
                    &mut privileges,
                    &schema,
                    &table,
                    &column.name,
                    &security.role_owner,
                    entry,
                );
            }
            if let Some(column_acl) = security.column_acls.get(&column.name) {
                for entry in column_acl {
                    insert_column_privilege_rows(
                        &mut privileges,
                        &schema,
                        &table,
                        &column.name,
                        &security.role_owner,
                        entry,
                    );
                }
            }
        }
    }
    insert_view_column_privileges(context, catalog, resolution, &mut privileges)?;
    insert_foreign_table_column_privileges(catalog, &mut privileges)?;

    Ok(privileges
        .into_iter()
        .filter_map(
            |(schema, table, column, owner, grantor, grantee, privilege_type, grantable)| {
                let grantor_enabled = catalog.role_is_enabled_for(current_user, &grantor);
                let grantee_name = grantee.to_string();
                // PostgreSQL's information-schema views filter the displayed recipient name. Grantability below still distinguishes the role from PUBLIC.
                let grantee_enabled = catalog.role_is_enabled_for(current_user, &grantee_name);
                if (role_grants_only || grantee_name != "PUBLIC")
                    && !grantor_enabled
                    && !grantee_enabled
                {
                    return None;
                }
                let is_grantable = grantable
                    || grantee
                        .role_name()
                        .is_some_and(|name| catalog.role_is_enabled_for(name, &owner));
                Some(row([
                    ("grantor", str_value(grantor)),
                    ("grantee", str_value(grantee_name)),
                    ("table_catalog", catalog_name()),
                    ("table_schema", str_value(schema)),
                    ("table_name", str_value(table)),
                    ("column_name", str_value(column)),
                    ("privilege_type", str_value(privilege_type)),
                    (
                        "is_grantable",
                        str_value(if is_grantable { "YES" } else { "NO" }),
                    ),
                ]))
            },
        )
        .collect())
}

pub fn build_info_views(
    context: &CatalogContext<'_>,
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
) -> Result<Vec<ResultRow>, SQLError> {
    let mut rows = Vec::new();
    for (name, stored) in catalog.views_of_kind(crate::catalog::view::StoredViewKind::View) {
        if !catalog.view_is_visible_to(&stored, resolution.current_user()) {
            continue;
        }
        let (schema, view) = split_schema_name(&name)?;
        let updatability = context.views.view_updatability(&name)?;
        let trigger_insertable = context
            .views
            .has_instead_of_trigger(&name, uqa_sql::ast::TriggerEvent::Insert)?;
        let trigger_updatable = context
            .views
            .has_instead_of_trigger(&name, uqa_sql::ast::TriggerEvent::Update)?;
        let trigger_deletable = context
            .views
            .has_instead_of_trigger(&name, uqa_sql::ast::TriggerEvent::Delete)?;
        let definition = if catalog
            .role_is_enabled_for(resolution.current_user(), &stored.security.role_owner)
        {
            str_value(super::view_definition::view_definition(
                catalog, resolution, &stored, false, 0,
            )?)
        } else {
            Value::Null
        };
        rows.push(row([
            ("table_catalog", catalog_name()),
            ("table_schema", str_value(schema)),
            ("table_name", str_value(view)),
            ("view_definition", definition),
            ("check_option", str_value(updatability.check_option)),
            (
                "is_updatable",
                str_value(if updatability.catalog.fully_updatable() {
                    "YES"
                } else {
                    "NO"
                }),
            ),
            (
                "is_insertable_into",
                str_value(if updatability.catalog.insertable {
                    "YES"
                } else {
                    "NO"
                }),
            ),
            (
                "is_trigger_updatable",
                str_value(if trigger_updatable { "YES" } else { "NO" }),
            ),
            (
                "is_trigger_deletable",
                str_value(if trigger_deletable { "YES" } else { "NO" }),
            ),
            (
                "is_trigger_insertable_into",
                str_value(if trigger_insertable { "YES" } else { "NO" }),
            ),
        ]));
    }
    Ok(rows)
}

pub fn build_info_sequences(
    catalog: &CatalogReadView,
    session: &dyn CatalogSession,
) -> Result<Vec<ResultRow>, SQLError> {
    let current_user = session.current_role();
    let temporary_schema = session.temporary_schema_name();
    Ok(catalog
        .sequence_definitions()?
        .into_iter()
        .filter(|(relation, state, persistence, security)| {
            (*persistence != uqa_sql::ast::RelationPersistence::Temporary
                || relation.schema == temporary_schema)
                && !state
                    .owner
                    .is_some_and(|owner| owner.dependency == SequenceOwnerDependency::Internal)
                && catalog.schema_has_privilege_to(
                    &relation.schema,
                    &current_user,
                    crate::catalog::security::schema::SchemaAclPrivilege::Usage,
                )
                && catalog.sequence_is_visible_to(security, &current_user)
        })
        .map(|(relation, state, _, _)| {
            let numeric_precision = match state.data_type {
                uqa_sql::ast::SequenceDataType::SmallInt => 16,
                uqa_sql::ast::SequenceDataType::Integer => 32,
                uqa_sql::ast::SequenceDataType::BigInt => 64,
            };
            row([
                ("sequence_catalog", catalog_name()),
                ("sequence_schema", str_value(relation.schema)),
                ("sequence_name", str_value(relation.name)),
                ("data_type", str_value(state.data_type.sql_name())),
                ("numeric_precision", Value::Int(numeric_precision)),
                ("numeric_precision_radix", Value::Int(2)),
                ("numeric_scale", Value::Int(0)),
                ("start_value", str_value(state.start.to_string())),
                ("minimum_value", str_value(state.min_value.to_string())),
                ("maximum_value", str_value(state.max_value.to_string())),
                ("increment", str_value(state.increment.to_string())),
                (
                    "cycle_option",
                    str_value(if state.cycle { "YES" } else { "NO" }),
                ),
            ])
        })
        .collect())
}

pub fn build_info_table_constraints(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
) -> Result<Vec<ResultRow>, SQLError> {
    Ok(constraint_catalog_rows(catalog, resolution)?
        .into_iter()
        .map(|constraint| {
            let constraint_type = if constraint.kind == ConstraintCatalogKind::NotNull {
                "CHECK"
            } else {
                constraint.kind.label()
            };
            let nulls_distinct = constraint
                .kind
                .nulls_distinct()
                .map_or(Value::Null, |value| {
                    str_value(if value { "YES" } else { "NO" })
                });
            row([
                ("constraint_catalog", catalog_name()),
                ("constraint_schema", str_value(constraint.schema.clone())),
                ("constraint_name", str_value(constraint.name)),
                ("table_schema", str_value(constraint.schema)),
                ("table_name", str_value(constraint.table)),
                ("constraint_type", str_value(constraint_type)),
                (
                    "is_deferrable",
                    str_value(if constraint.state.deferrable() {
                        "YES"
                    } else {
                        "NO"
                    }),
                ),
                (
                    "initially_deferred",
                    str_value(if constraint.state.initially_deferred() {
                        "YES"
                    } else {
                        "NO"
                    }),
                ),
                (
                    "enforced",
                    str_value(if constraint.state.enforced() {
                        "YES"
                    } else {
                        "NO"
                    }),
                ),
                ("nulls_distinct", nulls_distinct),
            ])
        })
        .collect())
}

pub fn build_info_key_column_usage(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
) -> Result<Vec<ResultRow>, SQLError> {
    let mut rows = Vec::new();
    for constraint in constraint_catalog_rows(catalog, resolution)? {
        if !matches!(
            constraint.kind,
            ConstraintCatalogKind::PrimaryKey
                | ConstraintCatalogKind::Unique { .. }
                | ConstraintCatalogKind::ForeignKey
        ) {
            continue;
        }
        for (index, column) in constraint.columns.iter().enumerate() {
            let position_in_unique_constraint = constraint
                .foreign_key
                .as_ref()
                .and_then(|foreign_key| {
                    foreign_key
                        .positions_in_unique_constraint
                        .get(index)
                        .copied()
                        .flatten()
                })
                .map_or(Value::Null, int_value);
            rows.push(row([
                ("constraint_catalog", catalog_name()),
                ("constraint_schema", str_value(constraint.schema.clone())),
                ("constraint_name", str_value(constraint.name.clone())),
                ("table_catalog", catalog_name()),
                ("table_schema", str_value(constraint.schema.clone())),
                ("table_name", str_value(constraint.table.clone())),
                ("column_name", str_value(column.name.clone())),
                (
                    "ordinal_position",
                    int_value(catalog_ordinal(index, "key constraint column")?),
                ),
                (
                    "position_in_unique_constraint",
                    position_in_unique_constraint,
                ),
            ]));
        }
    }
    Ok(rows)
}
