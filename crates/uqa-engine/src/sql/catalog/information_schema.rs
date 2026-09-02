//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Virtual `information_schema` relation builders.

use super::builtin_routines::PG18_BUILTIN_ROUTINE_GROUPS;
use super::expression_text::{default_expr_text, schema_expr_text};
use super::helpers::constraints::{constraint_catalog_rows, ConstraintCatalogKind};
use super::helpers::information_schema_types::{
    info_character_maximum_length, info_character_octet_length, info_data_type,
    info_datetime_precision, info_numeric_precision, info_numeric_scale, info_udt_name,
};
use super::helpers::oids::{current_user_name, split_schema_name, stable_oid};
use super::helpers::rows::{catalog_name, catalog_ordinal, int_value, row, str_value};
use super::helpers::type_metadata::{catalog_regtype_name, catalog_type_name};
use super::helpers::views::{all_schema_names, view_columns_for};
use crate::engine_capabilities::{CatalogReadView, RelationNameResolution, SessionExecutionView};
use crate::engine_user_functions::routine_signature_types;
use crate::sql::value_to_text;
use crate::{Engine, SequenceOwnerDependency};
use uqa_core::Value;
use uqa_sql::ast::ColumnDef as SQLColumnDef;
use uqa_sql::registry::registered_names;
use uqa_sql::{ResultRow, SQLError};

pub(super) fn build_info_catalog_name() -> Vec<ResultRow> {
    vec![row([("catalog_name", catalog_name())])]
}

pub(super) fn build_info_schemata(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
) -> Result<Vec<ResultRow>, SQLError> {
    let current_user = resolution.current_user();
    Ok(all_schema_names(catalog, resolution)?
        .into_iter()
        .filter(|schema| {
            catalog.schema_security(schema).is_none()
                || catalog.schema_has_privilege_to(
                    schema,
                    current_user,
                    crate::engine_schema_security::SchemaAclPrivilege::Usage,
                )
                || catalog.schema_has_privilege_to(
                    schema,
                    current_user,
                    crate::engine_schema_security::SchemaAclPrivilege::Create,
                )
        })
        .map(|schema| {
            let owner = catalog.schema_security(&schema).map_or_else(
                || current_user_name().to_string(),
                |security| security.role_owner.clone(),
            );
            row([
                ("catalog_name", catalog_name()),
                ("schema_name", str_value(schema)),
                ("schema_owner", str_value(owner)),
                ("default_character_set_catalog", catalog_name()),
                ("default_character_set_schema", str_value("pg_catalog")),
                ("default_character_set_name", str_value("UTF8")),
                ("sql_path", Value::Null),
            ])
        })
        .collect())
}

pub(super) fn build_info_tables(
    engine: &Engine,
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
    for (name, _) in catalog.views_of_kind(crate::StoredViewKind::View) {
        let (schema, view) = split_schema_name(&name)?;
        let updatability = crate::sql::dml::view_automatic::view_updatability(engine, &name)?;
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
            ("is_insertable_into", str_value("YES")),
            ("is_typed", str_value("NO")),
            ("commit_action", Value::Null),
        ]));
    }
    out.extend(super::ag_catalog::age_info_table_rows(catalog)?);
    out.sort_by(|a, b| {
        value_to_text(a.get("table_schema").unwrap_or(&Value::Null))
            .cmp(&value_to_text(
                b.get("table_schema").unwrap_or(&Value::Null),
            ))
            .then_with(|| {
                value_to_text(a.get("table_name").unwrap_or(&Value::Null))
                    .cmp(&value_to_text(b.get("table_name").unwrap_or(&Value::Null)))
            })
    });
    Ok(out)
}

#[expect(
    clippy::too_many_lines,
    reason = "preserves catalog column and OID order"
)]
fn information_schema_column_row(
    schema: String,
    table: String,
    index: usize,
    column: &SQLColumnDef,
    updatable: bool,
) -> Result<ResultRow, SQLError> {
    Ok(row([
        ("table_catalog", catalog_name()),
        ("table_schema", str_value(schema)),
        ("table_name", str_value(table)),
        ("column_name", str_value(column.name.clone())),
        (
            "ordinal_position",
            int_value(catalog_ordinal(index, "information_schema column")?),
        ),
        ("column_default", default_expr_text(column.default.as_ref())),
        (
            "is_nullable",
            str_value(if column.not_null || column.primary_key {
                "NO"
            } else {
                "YES"
            }),
        ),
        ("data_type", str_value(info_data_type(&column.ty))),
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
        ("interval_type", Value::Null),
        ("interval_precision", Value::Null),
        ("character_set_catalog", Value::Null),
        ("character_set_schema", Value::Null),
        ("character_set_name", Value::Null),
        ("collation_catalog", Value::Null),
        ("collation_schema", Value::Null),
        ("collation_name", Value::Null),
        ("domain_catalog", Value::Null),
        ("domain_schema", Value::Null),
        ("domain_name", Value::Null),
        ("udt_catalog", catalog_name()),
        ("udt_schema", str_value("pg_catalog")),
        ("udt_name", str_value(info_udt_name(&column.ty))),
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
        (
            "identity_start",
            if column
                .auto_increment
                .as_ref()
                .is_some_and(|provenance| provenance.is_identity() || provenance.is_legacy())
            {
                str_value("1")
            } else {
                Value::Null
            },
        ),
        (
            "identity_increment",
            if column
                .auto_increment
                .as_ref()
                .is_some_and(|provenance| provenance.is_identity() || provenance.is_legacy())
            {
                str_value("1")
            } else {
                Value::Null
            },
        ),
        ("identity_maximum", Value::Null),
        ("identity_minimum", Value::Null),
        ("identity_cycle", str_value("NO")),
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
            column.generated.as_ref().map_or(Value::Null, |generated| {
                str_value(schema_expr_text(&generated.expression))
            }),
        ),
        (
            "is_updatable",
            str_value(if updatable { "YES" } else { "NO" }),
        ),
    ]))
}

pub(super) fn build_info_columns(
    engine: &Engine,
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
) -> Result<Vec<ResultRow>, SQLError> {
    let mut out: Vec<ResultRow> = Vec::new();
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
            out.push(information_schema_column_row(
                schema.clone(),
                table.clone(),
                idx,
                col,
                true,
            )?);
        }
    }
    for (view_name, stored) in catalog.views_of_kind(crate::StoredViewKind::View) {
        let (schema, view) = split_schema_name(&view_name)?;
        let updatability = crate::sql::dml::view_automatic::view_updatability(engine, &view_name)?;
        let columns = view_columns_for(engine, catalog, resolution, &stored)?;
        for (idx, column) in columns.iter().enumerate() {
            out.push(information_schema_column_row(
                schema.clone(),
                view.clone(),
                idx,
                column,
                updatability
                    .catalog_columns
                    .get(idx)
                    .copied()
                    .unwrap_or(false),
            )?);
        }
    }
    out.extend(super::ag_catalog::age_info_column_rows(catalog)?);
    Ok(out)
}

pub(super) fn build_info_views(
    engine: &Engine,
    catalog: &CatalogReadView,
) -> Result<Vec<ResultRow>, SQLError> {
    let mut rows = Vec::new();
    for (name, stored) in catalog.views_of_kind(crate::StoredViewKind::View) {
        let (schema, view) = split_schema_name(&name)?;
        let updatability = crate::sql::dml::view_automatic::view_updatability(engine, &name)?;
        let trigger_insertable = crate::sql::dml::view_automatic::has_instead_of_trigger(
            engine,
            &name,
            uqa_sql::ast::TriggerEvent::Insert,
        )?;
        let trigger_updatable = crate::sql::dml::view_automatic::has_instead_of_trigger(
            engine,
            &name,
            uqa_sql::ast::TriggerEvent::Update,
        )?;
        let trigger_deletable = crate::sql::dml::view_automatic::has_instead_of_trigger(
            engine,
            &name,
            uqa_sql::ast::TriggerEvent::Delete,
        )?;
        let definition = format!("{:?}", stored.query);
        rows.push(row([
            ("table_catalog", catalog_name()),
            ("table_schema", str_value(schema)),
            ("table_name", str_value(view)),
            ("view_definition", str_value(definition)),
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

#[expect(
    clippy::too_many_lines,
    reason = "preserves catalog column and OID order"
)]
pub(super) fn build_info_routines(catalog: &CatalogReadView) -> Result<Vec<ResultRow>, SQLError> {
    let mut rows: Vec<ResultRow> = PG18_BUILTIN_ROUTINE_GROUPS
        .iter()
        .flat_map(|group| group.iter())
        .map(|routine| {
            let regtype_name = catalog_regtype_name(routine.return_type);
            row([
                ("specific_catalog", catalog_name()),
                ("specific_schema", str_value("pg_catalog")),
                (
                    "specific_name",
                    str_value(format!("{}_{}", routine.name, routine.oid)),
                ),
                ("routine_catalog", catalog_name()),
                ("routine_schema", str_value("pg_catalog")),
                ("routine_name", str_value(routine.name)),
                (
                    "routine_type",
                    if routine.kind == "f" {
                        str_value("FUNCTION")
                    } else {
                        Value::Null
                    },
                ),
                ("module_catalog", Value::Null),
                ("module_schema", Value::Null),
                ("module_name", Value::Null),
                ("udt_catalog", Value::Null),
                ("udt_schema", Value::Null),
                ("udt_name", Value::Null),
                (
                    "data_type",
                    str_value(catalog_type_name(routine.return_type)),
                ),
                (
                    "type_udt_catalog",
                    regtype_name.map_or(Value::Null, |_| catalog_name()),
                ),
                (
                    "type_udt_schema",
                    regtype_name.map_or(Value::Null, |_| str_value("pg_catalog")),
                ),
                ("type_udt_name", regtype_name.map_or(Value::Null, str_value)),
                ("routine_body", str_value("EXTERNAL")),
                ("routine_definition", Value::Null),
                ("external_name", Value::Null),
                ("external_language", str_value("INTERNAL")),
                (
                    "is_deterministic",
                    str_value(if routine.volatility == "i" {
                        "YES"
                    } else {
                        "NO"
                    }),
                ),
                ("sql_data_access", str_value("MODIFIES")),
                (
                    "is_null_call",
                    str_value(if routine.strict { "YES" } else { "NO" }),
                ),
                ("schema_level_routine", str_value("YES")),
                ("max_dynamic_result_sets", Value::Int(0)),
                ("is_udt_dependent", str_value("NO")),
            ])
        })
        .collect();
    rows.extend(registered_names().into_iter().map(|name| {
        row([
            ("specific_catalog", catalog_name()),
            ("specific_schema", str_value("pg_catalog")),
            ("specific_name", str_value(format!("{name}_0"))),
            ("routine_catalog", catalog_name()),
            ("routine_schema", str_value("pg_catalog")),
            ("routine_name", str_value(name)),
            ("routine_type", str_value("FUNCTION")),
            ("module_catalog", Value::Null),
            ("module_schema", Value::Null),
            ("module_name", Value::Null),
            ("udt_catalog", catalog_name()),
            ("udt_schema", str_value("pg_catalog")),
            ("udt_name", str_value("text")),
            ("data_type", str_value("text")),
            ("routine_body", str_value("EXTERNAL")),
            ("routine_definition", Value::Null),
            ("external_name", Value::Null),
            ("external_language", str_value("rust")),
            ("is_deterministic", str_value("NO")),
            ("sql_data_access", str_value("READS SQL DATA")),
            ("is_null_call", str_value("YES")),
            ("schema_level_routine", str_value("YES")),
            ("max_dynamic_result_sets", Value::Int(0)),
            ("is_udt_dependent", str_value("NO")),
        ])
    }));
    for function in catalog.all_sql_functions() {
        let def = &function.def;
        let (routine_schema, routine_name) = split_schema_name(&def.name)?;
        let signature = routine_signature_types(def);
        let identity = format!(
            "{}:{}:{}",
            def.name,
            if def.is_procedure {
                "procedure"
            } else {
                "function"
            },
            signature.join(",")
        );
        let routine_type = if def.is_procedure {
            "PROCEDURE"
        } else {
            "FUNCTION"
        };
        let (routine_body, external_language) = if def.language == "sql" {
            ("SQL", "SQL".to_string())
        } else {
            ("EXTERNAL", def.language.to_ascii_uppercase())
        };
        let definition = match &def.body {
            uqa_sql::ast::FunctionBody::Source(source) => str_value(source.clone()),
            uqa_sql::ast::FunctionBody::Statements(_) => Value::Null,
        };
        let data_type = match &def.returns {
            uqa_sql::ast::FunctionReturns::Scalar { type_name }
            | uqa_sql::ast::FunctionReturns::SetOf { type_name } => str_value(type_name.clone()),
            uqa_sql::ast::FunctionReturns::Table => str_value("record"),
            uqa_sql::ast::FunctionReturns::None => Value::Null,
        };
        rows.push(row([
            ("specific_catalog", catalog_name()),
            ("specific_schema", str_value(routine_schema.clone())),
            (
                "specific_name",
                str_value(format!(
                    "{}_{}",
                    routine_name,
                    stable_oid("routine", &identity)
                )),
            ),
            ("routine_catalog", catalog_name()),
            ("routine_schema", str_value(routine_schema)),
            ("routine_name", str_value(routine_name)),
            ("routine_type", str_value(routine_type)),
            ("module_catalog", Value::Null),
            ("module_schema", Value::Null),
            ("module_name", Value::Null),
            ("udt_catalog", catalog_name()),
            ("udt_schema", str_value("pg_catalog")),
            ("udt_name", data_type.clone()),
            ("data_type", data_type),
            ("routine_body", str_value(routine_body)),
            ("routine_definition", definition),
            ("external_name", Value::Null),
            ("external_language", str_value(external_language)),
            (
                "is_deterministic",
                str_value(
                    if matches!(def.volatility, uqa_sql::ast::FunctionVolatility::Immutable) {
                        "YES"
                    } else {
                        "NO"
                    },
                ),
            ),
            ("sql_data_access", str_value("MODIFIES SQL DATA")),
            (
                "is_null_call",
                str_value(if def.strict { "YES" } else { "NO" }),
            ),
            ("schema_level_routine", str_value("YES")),
            ("max_dynamic_result_sets", Value::Int(0)),
            ("is_udt_dependent", str_value("NO")),
        ]));
    }
    Ok(rows)
}

pub(super) fn build_info_sequences(
    catalog: &CatalogReadView,
    session: SessionExecutionView<'_>,
) -> Vec<ResultRow> {
    let current_user = session.current_user();
    let temporary_schema = session.temporary_schema_name();
    catalog
        .sequence_states()
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
                    crate::engine_schema_security::SchemaAclPrivilege::Usage,
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
        .collect()
}

pub(super) fn build_info_table_constraints(
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

pub(super) fn build_info_key_column_usage(
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
