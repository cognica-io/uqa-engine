//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! CREATE TABLE, column constraint, and CREATE INDEX lowering.

use super::{
    compile_expr, compile_foreign_key_action, compile_foreign_key_match, compile_type_name,
    extract_strings, range_var_name, raw_type_name, validate_foreign_key_set_columns, ColumnDef,
    CreateIndex, CreateTable, Expr, NodeEnum, Result, SQLError, TableKeyConstraint,
    TableKeyConstraintKind,
};
use crate::ast::{AutoIncrement, ColumnType, GeneratedColumn, GeneratedColumnKind};

struct TableNotNullConstraint {
    name: Option<String>,
    column: String,
    validated: bool,
    no_inherit: bool,
}

#[expect(
    clippy::too_many_lines,
    reason = "ordered PostgreSQL lowering preserves syntax and error precedence"
)]
pub(in crate::compiler) fn compile_create_table(
    stmt: &pg_query::protobuf::CreateStmt,
) -> Result<CreateTable> {
    use crate::ast::{ForeignKey, TableCheck};
    use std::collections::BTreeSet;
    crate::compiler::validate_create_table_envelope(stmt, "CREATE TABLE")?;
    let relation = stmt
        .relation
        .as_ref()
        .ok_or_else(|| SQLError::Internal("CREATE TABLE without relation".into()))?;
    let persistence = crate::compiler::relation_persistence(relation, "CREATE TABLE")?;
    let on_commit =
        crate::compiler::compile_on_commit(stmt.oncommit(), persistence, "CREATE TABLE")?;
    let hierarchy = crate::compiler::compile_table_hierarchy(stmt)?;
    let name = range_var_name(relation);
    if name.is_empty() {
        return Err(SQLError::Internal("CREATE TABLE without name".into()));
    }
    let mut columns = Vec::new();
    let mut checks: Vec<TableCheck> = Vec::new();
    let mut foreign_keys: Vec<ForeignKey> = Vec::new();
    let mut key_constraints: Vec<TableKeyConstraint> = Vec::new();
    let mut table_not_nulls = Vec::new();
    let mut named_constraints = BTreeSet::new();
    let mut primary_key_seen = false;
    for elt in &stmt.table_elts {
        let inner = elt
            .node
            .as_ref()
            .ok_or_else(|| SQLError::Internal("CREATE TABLE contains an empty element".into()))?;
        match inner {
            NodeEnum::ColumnDef(col) => {
                for constraint in &col.constraints {
                    let inner = constraint.node.as_ref().ok_or_else(|| {
                        SQLError::Internal("column contains an empty constraint".into())
                    })?;
                    let NodeEnum::Constraint(cstr) = inner else {
                        return Err(SQLError::Internal(format!(
                            "unexpected column constraint node {inner:?}"
                        )));
                    };
                    register_constraint_name(&mut named_constraints, &cstr.conname)?;
                    let kind = match cstr.contype() {
                        pg_query::protobuf::ConstrType::ConstrPrimary => {
                            if primary_key_seen {
                                return Err(SQLError::TypeMismatch(
                                    "multiple PRIMARY KEY constraints are not allowed".into(),
                                ));
                            }
                            primary_key_seen = true;
                            Some(TableKeyConstraintKind::PrimaryKey)
                        }
                        pg_query::protobuf::ConstrType::ConstrUnique => {
                            Some(TableKeyConstraintKind::Unique)
                        }
                        _ => None,
                    };
                    if let Some(kind) = kind {
                        key_constraints.push(TableKeyConstraint {
                            name: constraint_name(&cstr.conname),
                            kind,
                            columns: vec![col.colname.clone()],
                            nulls_not_distinct: cstr.nulls_not_distinct,
                            without_overlaps: cstr.without_overlaps,
                        });
                    }
                }
                columns.push(compile_column_def(col)?);
            }
            NodeEnum::Constraint(cstr) => {
                register_constraint_name(&mut named_constraints, &cstr.conname)?;
                match cstr.contype() {
                    pg_query::protobuf::ConstrType::ConstrCheck => {
                        let raw = cstr
                            .raw_expr
                            .as_deref()
                            .ok_or_else(|| SQLError::Internal("CHECK without expression".into()))?;
                        let expr = compile_expr(raw)?;
                        let cname = if cstr.conname.is_empty() {
                            None
                        } else {
                            Some(cstr.conname.clone())
                        };
                        checks.push(TableCheck {
                            name: cname,
                            expr,
                            enforced: cstr.is_enforced,
                            validated: cstr.initially_valid && cstr.is_enforced,
                            no_inherit: cstr.is_no_inherit,
                            partition_constraint: None,
                        });
                    }
                    pg_query::protobuf::ConstrType::ConstrForeign => {
                        if cstr.fk_with_period != cstr.pk_with_period {
                            return Err(SQLError::TypeMismatch(
                                "FOREIGN KEY must use PERIOD on both the referencing and referenced key"
                                    .into(),
                            ));
                        }
                        let local_columns = extract_strings(&cstr.fk_attrs)?;
                        let ref_table =
                            cstr.pktable.as_ref().map(range_var_name).ok_or_else(|| {
                                SQLError::Internal("FOREIGN KEY without referenced table".into())
                            })?;
                        let ref_columns = extract_strings(&cstr.pk_attrs)?;
                        if local_columns.is_empty() {
                            return Err(SQLError::Internal(
                                "FOREIGN KEY without local columns".into(),
                            ));
                        }
                        if !ref_columns.is_empty() && local_columns.len() != ref_columns.len() {
                            return Err(SQLError::TypeMismatch(format!(
                                "FOREIGN KEY has {} local columns but {} referenced columns",
                                local_columns.len(),
                                ref_columns.len()
                            )));
                        }
                        let cname = if cstr.conname.is_empty() {
                            None
                        } else {
                            Some(cstr.conname.clone())
                        };
                        let on_delete_set_columns = extract_strings(&cstr.fk_del_set_cols)?;
                        validate_foreign_key_set_columns(
                            &local_columns,
                            &on_delete_set_columns,
                            &cstr.fk_del_action,
                        )?;
                        foreign_keys.push(ForeignKey {
                            name: cname,
                            object_id: None,
                            local_columns,
                            ref_table,
                            ref_columns,
                            on_update: compile_foreign_key_action(&cstr.fk_upd_action)?,
                            on_delete: compile_foreign_key_action(&cstr.fk_del_action)?,
                            on_delete_set_columns,
                            match_type: compile_foreign_key_match(&cstr.fk_matchtype)?,
                            enforced: cstr.is_enforced,
                            validated: cstr.initially_valid && cstr.is_enforced,
                            deferrable: cstr.deferrable,
                            initially_deferred: cstr.initdeferred,
                            period: cstr.fk_with_period,
                        });
                    }
                    pg_query::protobuf::ConstrType::ConstrPrimary
                    | pg_query::protobuf::ConstrType::ConstrUnique => {
                        let kind =
                            if cstr.contype() == pg_query::protobuf::ConstrType::ConstrPrimary {
                                if primary_key_seen {
                                    return Err(SQLError::TypeMismatch(
                                        "multiple PRIMARY KEY constraints are not allowed".into(),
                                    ));
                                }
                                primary_key_seen = true;
                                TableKeyConstraintKind::PrimaryKey
                            } else {
                                TableKeyConstraintKind::Unique
                            };
                        let key_columns = extract_strings(&cstr.keys)?;
                        key_constraints.push(TableKeyConstraint {
                            name: constraint_name(&cstr.conname),
                            kind,
                            columns: key_columns,
                            nulls_not_distinct: cstr.nulls_not_distinct,
                            without_overlaps: cstr.without_overlaps,
                        });
                    }
                    pg_query::protobuf::ConstrType::ConstrNotnull => {
                        let key_columns = extract_strings(&cstr.keys)?;
                        let [column] = key_columns.as_slice() else {
                            return Err(SQLError::TypeMismatch(
                                "NOT NULL constraint must name exactly one column".into(),
                            ));
                        };
                        table_not_nulls.push(TableNotNullConstraint {
                            name: constraint_name(&cstr.conname),
                            column: column.clone(),
                            validated: cstr.initially_valid,
                            no_inherit: cstr.is_no_inherit,
                        });
                    }
                    other => {
                        return Err(SQLError::Unsupported(format!(
                            "table constraint {other:?} is not supported"
                        )));
                    }
                }
            }
            other => {
                return Err(SQLError::Unsupported(format!(
                    "CREATE TABLE element {other:?} is not supported"
                )));
            }
        }
    }
    for constraint in table_not_nulls {
        let column = columns
            .iter_mut()
            .find(|column| column.name == constraint.column)
            .ok_or_else(|| {
                SQLError::TypeMismatch(format!(
                    "NOT NULL constraint references unknown column `{}`",
                    constraint.column
                ))
            })?;
        if column.not_null_explicit {
            return Err(SQLError::Routine {
                sqlstate: "55000".into(),
                message: format!(
                    "cannot create not-null constraint on column \"{}\": a not-null constraint already exists",
                    constraint.column
                ),
            });
        }
        column.not_null = true;
        column.not_null_explicit = true;
        column.not_null_name = constraint.name;
        column.not_null_validated = constraint.validated;
        column.not_null_no_inherit = constraint.no_inherit;
    }
    let column_names: BTreeSet<&str> = columns.iter().map(|column| column.name.as_str()).collect();
    for constraint in &key_constraints {
        if constraint.columns.is_empty() {
            return Err(SQLError::TypeMismatch(format!(
                "{} constraint must name at least one column",
                key_constraint_label(constraint.kind)
            )));
        }
        let mut seen = BTreeSet::new();
        for column in &constraint.columns {
            if !column_names.contains(column.as_str()) {
                return Err(SQLError::TypeMismatch(format!(
                    "{} constraint references unknown column `{column}`",
                    key_constraint_label(constraint.kind)
                )));
            }
            if !seen.insert(column.as_str()) {
                return Err(SQLError::TypeMismatch(format!(
                    "{} constraint names column `{column}` more than once",
                    key_constraint_label(constraint.kind)
                )));
            }
        }
        if constraint.without_overlaps {
            let period_column = constraint
                .columns
                .last()
                .expect("validated non-empty key constraint");
            let period_type = columns
                .iter()
                .find(|column| column.name == *period_column)
                .map(|column| &column.ty)
                .ok_or_else(|| SQLError::Internal("WITHOUT OVERLAPS column disappeared".into()))?;
            if !matches!(
                period_type,
                ColumnType::Range(_) | ColumnType::Multirange(_)
            ) {
                return Err(SQLError::TypeMismatch(format!(
                    "column \"{period_column}\" in WITHOUT OVERLAPS is not a range or multirange type"
                )));
            }
            if constraint.columns.len() < 2 {
                return Err(SQLError::TypeMismatch(
                    "constraint using WITHOUT OVERLAPS needs at least two columns".into(),
                ));
            }
        }
    }
    for foreign_key in &foreign_keys {
        if !foreign_key.period {
            continue;
        }
        if foreign_key.local_columns.len() < 2 {
            return Err(SQLError::TypeMismatch(
                "FOREIGN KEY using PERIOD needs at least two columns".into(),
            ));
        }
        if !matches!(
            (foreign_key.on_update, foreign_key.on_delete),
            (
                crate::ast::ForeignKeyAction::NoAction,
                crate::ast::ForeignKeyAction::NoAction
            )
        ) {
            return Err(SQLError::Unsupported(
                "unsupported referential action for foreign key constraint using PERIOD".into(),
            ));
        }
        let period_column = foreign_key
            .local_columns
            .last()
            .and_then(|name| columns.iter().find(|column| column.name == *name))
            .ok_or_else(|| SQLError::Internal("PERIOD column disappeared".into()))?;
        if !matches!(
            period_column.ty,
            ColumnType::Range(_) | ColumnType::Multirange(_)
        ) {
            return Err(SQLError::TypeMismatch(format!(
                "column \"{}\" in PERIOD is not a range or multirange type",
                period_column.name
            )));
        }
    }
    // Keep legacy scalar-key consumers correct while retaining the full typed
    // tuple above. A composite primary key makes every member NOT NULL, but no
    // individual member is itself a primary/unique key.
    for constraint in &key_constraints {
        for column_name in &constraint.columns {
            let column = columns
                .iter_mut()
                .find(|column| column.name == *column_name)
                .ok_or_else(|| {
                    SQLError::Internal(format!(
                        "validated key column `{column_name}` disappeared during lowering"
                    ))
                })?;
            if constraint.kind == TableKeyConstraintKind::PrimaryKey {
                column.not_null = true;
                if constraint.columns.len() == 1 {
                    column.primary_key = true;
                }
            } else if constraint.columns.len() == 1 {
                column.unique = true;
            }
        }
    }
    Ok(CreateTable {
        name,
        qualifier: relation.relname.clone(),
        columns,
        if_not_exists: stmt.if_not_exists,
        checks,
        foreign_keys,
        key_constraints,
        persistence,
        on_commit,
        hierarchy,
    })
}

pub(in crate::compiler) fn constraint_name(name: &str) -> Option<String> {
    (!name.is_empty()).then(|| name.to_string())
}

pub(in crate::compiler) fn register_constraint_name(
    names: &mut std::collections::BTreeSet<String>,
    name: &str,
) -> Result<()> {
    if !name.is_empty() && !names.insert(name.to_string()) {
        return Err(SQLError::TypeMismatch(format!(
            "constraint `{name}` is declared more than once"
        )));
    }
    Ok(())
}

pub(in crate::compiler) fn key_constraint_label(kind: TableKeyConstraintKind) -> &'static str {
    match kind {
        TableKeyConstraintKind::PrimaryKey => "PRIMARY KEY",
        TableKeyConstraintKind::Unique => "UNIQUE",
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "ordered PostgreSQL lowering preserves syntax and error precedence"
)]
pub(in crate::compiler) fn compile_column_def(
    col: &pg_query::protobuf::ColumnDef,
) -> Result<ColumnDef> {
    let name = col.colname.clone();
    let raw_type = raw_type_name(col)?;
    let ty = compile_type_name(col)?;
    let mut auto_increment = matches!(
        raw_type.as_deref(),
        Some("smallserial" | "serial2" | "serial" | "serial4" | "bigserial" | "serial8")
    )
    .then_some(AutoIncrement::serial());
    let mut primary_key = false;
    let mut not_null = false;
    let mut not_null_explicit = false;
    let mut not_null_name = None;
    let mut not_null_validated = true;
    let mut not_null_no_inherit = false;
    let mut unique = false;
    let mut default: Option<Expr> = None;
    let mut generated: Option<GeneratedColumn> = None;
    let mut check: Option<Expr> = None;
    let mut check_name = None;
    let mut check_enforced = true;
    let mut check_validated = true;
    let mut check_no_inherit = false;
    let mut references: Option<crate::ast::ForeignKeyRef> = None;
    #[derive(Clone, Copy)]
    enum EnforceableConstraint {
        Check,
        ForeignKey,
    }
    let mut last_enforceable = None;
    for c in &col.constraints {
        let inner = c
            .node
            .as_ref()
            .ok_or_else(|| SQLError::Internal("column contains an empty constraint".into()))?;
        match inner {
            NodeEnum::Constraint(cstr) => match cstr.contype() {
                pg_query::protobuf::ConstrType::ConstrPrimary => {
                    primary_key = true;
                    not_null = true;
                    last_enforceable = None;
                }
                pg_query::protobuf::ConstrType::ConstrNotnull => {
                    not_null = true;
                    not_null_explicit = true;
                    not_null_name = constraint_name(&cstr.conname);
                    not_null_validated = cstr.initially_valid;
                    not_null_no_inherit = cstr.is_no_inherit;
                    last_enforceable = None;
                }
                pg_query::protobuf::ConstrType::ConstrUnique => {
                    unique = true;
                    last_enforceable = None;
                }
                pg_query::protobuf::ConstrType::ConstrIdentity => {
                    auto_increment = Some(match cstr.generated_when.as_str() {
                        "a" => AutoIncrement::identity_always(),
                        "d" => AutoIncrement::identity_by_default(),
                        other => {
                            return Err(SQLError::Internal(format!(
                                "identity constraint has unknown generation {other:?}"
                            )));
                        }
                    });
                    last_enforceable = None;
                }
                pg_query::protobuf::ConstrType::ConstrDefault => {
                    let raw = cstr.raw_expr.as_deref().ok_or_else(|| {
                        SQLError::Internal("DEFAULT constraint without expression".into())
                    })?;
                    default = Some(compile_expr(raw)?);
                    last_enforceable = None;
                }
                pg_query::protobuf::ConstrType::ConstrGenerated => {
                    let raw = cstr.raw_expr.as_deref().ok_or_else(|| {
                        SQLError::Internal("generated column without expression".into())
                    })?;
                    let kind = match cstr.generated_kind.as_str() {
                        "v" => GeneratedColumnKind::Virtual,
                        "s" => GeneratedColumnKind::Stored,
                        other => {
                            return Err(SQLError::Internal(format!(
                                "generated column has unknown kind {other:?}"
                            )));
                        }
                    };
                    generated = Some(GeneratedColumn {
                        kind,
                        expression: Box::new(compile_expr(raw)?),
                        function_dependencies: Vec::new(),
                    });
                    last_enforceable = None;
                }
                pg_query::protobuf::ConstrType::ConstrCheck => {
                    let raw = cstr
                        .raw_expr
                        .as_deref()
                        .ok_or_else(|| SQLError::Internal("CHECK without expression".into()))?;
                    check = Some(compile_expr(raw)?);
                    check_name = constraint_name(&cstr.conname);
                    check_enforced = cstr.is_enforced;
                    check_validated = cstr.initially_valid && cstr.is_enforced;
                    check_no_inherit = cstr.is_no_inherit;
                    last_enforceable = Some(EnforceableConstraint::Check);
                }
                pg_query::protobuf::ConstrType::ConstrForeign => {
                    if cstr.fk_with_period || cstr.pk_with_period {
                        return Err(SQLError::TypeMismatch(
                            "column REFERENCES cannot declare PERIOD; use a table FOREIGN KEY"
                                .into(),
                        ));
                    }
                    let table =
                        cstr.pktable.as_ref().map(range_var_name).ok_or_else(|| {
                            SQLError::Internal("REFERENCES without a table".into())
                        })?;
                    let columns = extract_strings(&cstr.pk_attrs)?;
                    if columns.len() > 1 {
                        return Err(SQLError::TypeMismatch(
                            "column REFERENCES must name at most one referenced column".into(),
                        ));
                    }
                    references = Some(crate::ast::ForeignKeyRef {
                        name: constraint_name(&cstr.conname),
                        object_id: None,
                        table,
                        column: columns.into_iter().next(),
                        on_update: compile_foreign_key_action(&cstr.fk_upd_action)?,
                        on_delete: compile_foreign_key_action(&cstr.fk_del_action)?,
                        match_type: compile_foreign_key_match(&cstr.fk_matchtype)?,
                        enforced: cstr.is_enforced,
                        validated: cstr.initially_valid && cstr.is_enforced,
                        deferrable: cstr.deferrable,
                        initially_deferred: cstr.initdeferred,
                        period: false,
                    });
                    last_enforceable = Some(EnforceableConstraint::ForeignKey);
                }
                pg_query::protobuf::ConstrType::ConstrAttrEnforced
                | pg_query::protobuf::ConstrType::ConstrAttrNotEnforced => {
                    let enforced =
                        cstr.contype() == pg_query::protobuf::ConstrType::ConstrAttrEnforced;
                    match last_enforceable {
                        Some(EnforceableConstraint::Check) => {
                            check_enforced = enforced;
                            if !enforced {
                                check_validated = false;
                            }
                        }
                        Some(EnforceableConstraint::ForeignKey) => {
                            let reference = references.as_mut().ok_or_else(|| {
                                SQLError::Internal(
                                    "REFERENCES enforcement attribute lost its constraint".into(),
                                )
                            })?;
                            reference.enforced = enforced;
                            if !enforced {
                                reference.validated = false;
                            }
                        }
                        None => {
                            return Err(SQLError::Unsupported(
                                "constraint enforcement attribute without CHECK or FOREIGN KEY"
                                    .into(),
                            ));
                        }
                    }
                }
                pg_query::protobuf::ConstrType::ConstrNull => last_enforceable = None,
                other => {
                    return Err(SQLError::Unsupported(format!(
                        "column constraint {other:?} is not supported"
                    )));
                }
            },
            other => {
                return Err(SQLError::Internal(format!(
                    "unexpected column constraint node {other:?}"
                )));
            }
        }
    }
    // Postgres treats `SERIAL` / `BIGSERIAL` as `NOT NULL` by definition.
    if auto_increment.is_some() {
        not_null = true;
    }
    Ok(ColumnDef {
        name,
        ty,
        object_id: None,
        missing_value: None,
        primary_key,
        not_null,
        not_null_explicit,
        not_null_name,
        not_null_validated,
        not_null_no_inherit,
        auto_increment,
        unique,
        default,
        generated,
        check,
        check_name,
        check_enforced,
        check_validated,
        check_no_inherit,
        references,
    })
}

// -------------------------------------------------------------------------
// CREATE INDEX
// -------------------------------------------------------------------------

pub(in crate::compiler) fn compile_create_index(
    stmt: &pg_query::protobuf::IndexStmt,
) -> Result<CreateIndex> {
    let table = stmt
        .relation
        .as_ref()
        .map(range_var_name)
        .ok_or_else(|| SQLError::Internal("CREATE INDEX without table".into()))?;
    let access_method = stmt.access_method.clone();
    let mut columns = Vec::new();
    for elt in &stmt.index_params {
        let inner = elt
            .node
            .as_ref()
            .ok_or_else(|| SQLError::Internal("CREATE INDEX contains an empty key".into()))?;
        let NodeEnum::IndexElem(idx) = inner else {
            return Err(SQLError::Internal(format!(
                "CREATE INDEX expected IndexElem, got {inner:?}"
            )));
        };
        if idx.name.is_empty() {
            return Err(SQLError::Unsupported(
                "expression indexes are not supported".into(),
            ));
        }
        columns.push(idx.name.clone());
    }
    let name = if stmt.idxname.is_empty() {
        None
    } else {
        Some(stmt.idxname.clone())
    };
    let mut options = Vec::new();
    for opt in &stmt.options {
        let inner = opt
            .node
            .as_ref()
            .ok_or_else(|| SQLError::Internal("CREATE INDEX contains an empty option".into()))?;
        let NodeEnum::DefElem(elem) = inner else {
            return Err(SQLError::Internal(format!(
                "CREATE INDEX expected DefElem option, got {inner:?}"
            )));
        };
        let key = elem.defname.clone();
        let value = match elem.arg.as_ref().and_then(|node| node.node.as_ref()) {
            Some(NodeEnum::String(value)) => value.sval.clone(),
            Some(NodeEnum::Integer(value)) => value.ival.to_string(),
            Some(NodeEnum::Float(value)) => value.fval.clone(),
            Some(NodeEnum::TypeName(value)) => extract_strings(&value.names)?.join("."),
            Some(other) => {
                return Err(SQLError::Unsupported(format!(
                    "CREATE INDEX option `{key}` value {other:?}"
                )));
            }
            None => {
                return Err(SQLError::Internal(format!(
                    "CREATE INDEX option `{key}` has no value"
                )));
            }
        };
        options.push((key, value));
    }
    Ok(CreateIndex {
        name,
        table,
        access_method,
        columns,
        if_not_exists: stmt.if_not_exists,
        options,
    })
}

// -------------------------------------------------------------------------
// INSERT
// -------------------------------------------------------------------------
