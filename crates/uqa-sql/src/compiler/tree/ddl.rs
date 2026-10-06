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
use crate::ast::{
    AutoIncrement, ColumnType, DeclaredCheck, DeclaredElement, GeneratedColumn,
    GeneratedColumnKind, NotNullDeclaration, TableCheck,
};

#[expect(
    clippy::too_many_lines,
    reason = "ordered PostgreSQL lowering preserves syntax and error precedence"
)]
pub(in crate::compiler) fn compile_create_table(
    stmt: &pg_query::protobuf::CreateStmt,
) -> Result<CreateTable> {
    use crate::ast::ForeignKey;
    crate::compiler::validate_create_table_envelope(stmt, "CREATE TABLE")?;
    let relation = stmt
        .relation
        .as_ref()
        .ok_or_else(|| SQLError::Internal("CREATE TABLE without relation".into()))?;
    let persistence = crate::compiler::relation_persistence(relation, "CREATE TABLE")?;
    let on_commit = crate::compiler::compile_on_commit(stmt.oncommit(), persistence)?;
    let hierarchy = crate::compiler::compile_table_hierarchy(stmt)?;
    let name = range_var_name(relation);
    if name.is_empty() {
        return Err(SQLError::Internal("CREATE TABLE without name".into()));
    }
    let mut columns = Vec::new();
    let mut checks: Vec<TableCheck> = Vec::new();
    let mut check_order = Vec::new();
    let mut element_order = Vec::new();
    let mut untyped_columns = Vec::new();
    let mut foreign_keys: Vec<ForeignKey> = Vec::new();
    let mut foreign_key_order = Vec::new();
    let mut key_constraints: Vec<TableKeyConstraint> = Vec::new();
    for elt in &stmt.table_elts {
        let inner = elt
            .node
            .as_ref()
            .ok_or_else(|| SQLError::Internal("CREATE TABLE contains an empty element".into()))?;
        match inner {
            NodeEnum::ColumnDef(col) => {
                element_order.push(DeclaredElement::Column(super::compile_column_declaration(
                    col,
                )?));
                if col.type_name.is_none() {
                    untyped_columns.push(col.colname.clone());
                }
                key_constraints.extend(compile_column_key_constraints(col)?);
                let (column, column_checks, column_references) = compile_column_def(col)?;
                if column_checks.is_empty() {
                    if column.check.is_some() {
                        check_order.push(DeclaredCheck::Column(column.name.clone()));
                    }
                } else {
                    for check in column_checks {
                        check_order.push(DeclaredCheck::Table(checks.len()));
                        checks.push(check);
                    }
                }
                for reference in column_references {
                    foreign_key_order
                        .push(crate::ast::DeclaredForeignKey::Table(foreign_keys.len()));
                    foreign_keys.push(reference);
                }
                if column.references.is_some() {
                    foreign_key_order
                        .push(crate::ast::DeclaredForeignKey::Column(column.name.clone()));
                }
                columns.push(column);
            }
            NodeEnum::Constraint(cstr) => match cstr.contype() {
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
                    check_order.push(DeclaredCheck::Table(checks.len()));
                    checks.push(TableCheck {
                        catalog_oid: None,
                        name: cname,
                        expr,
                        enforced: cstr.is_enforced,
                        validated: cstr.initially_valid && cstr.is_enforced,
                        no_inherit: cstr.is_no_inherit,
                        object_id: None,
                        is_local: true,
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
                    let ref_table = cstr.pktable.as_ref().map(range_var_name).ok_or_else(|| {
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
                    foreign_key_order
                        .push(crate::ast::DeclaredForeignKey::Table(foreign_keys.len()));
                    foreign_keys.push(ForeignKey {
                        referenced_key: None,
                        referenced_index: None,
                        name: cname,
                        object_id: None,
                        catalog_identity: None,
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
                        referenced_partitions: Vec::new(),
                    });
                }
                pg_query::protobuf::ConstrType::ConstrPrimary
                | pg_query::protobuf::ConstrType::ConstrUnique => {
                    if cstr.deferrable || cstr.initdeferred {
                        element_order.push(DeclaredElement::DeferrableKey);
                    }
                    let kind = if cstr.contype() == pg_query::protobuf::ConstrType::ConstrPrimary {
                        TableKeyConstraintKind::PrimaryKey
                    } else {
                        TableKeyConstraintKind::Unique
                    };
                    let key_columns = extract_strings(&cstr.keys)?;
                    if kind == TableKeyConstraintKind::PrimaryKey {
                        element_order.push(DeclaredElement::PrimaryKey {
                            columns: key_columns.clone(),
                        });
                    }
                    key_constraints.push(TableKeyConstraint {
                        catalog_identity: None,
                        index_identity: None,
                        name: constraint_name(&cstr.conname),
                        kind,
                        columns: key_columns,
                        included_columns: extract_strings(&cstr.including)?,
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
                    // CREATE TABLE validates every NOT NULL constraint it creates; NOT VALID applies to ALTER TABLE.
                    element_order.push(DeclaredElement::NotNull(NotNullDeclaration {
                        column: column.clone(),
                        name: constraint_name(&cstr.conname),
                        no_inherit: cstr.is_no_inherit,
                        explicit: true,
                    }));
                }
                other => {
                    return Err(SQLError::Unsupported(format!(
                        "table constraint {other:?} is not supported"
                    )));
                }
            },
            other => {
                return Err(SQLError::Unsupported(format!(
                    "CREATE TABLE element {other:?} is not supported"
                )));
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
        crate::schema::foreign_keys::validate_period_foreign_key_actions(foreign_key)?;
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
    Ok(CreateTable {
        name,
        qualifier: relation.relname.clone(),
        columns,
        if_not_exists: stmt.if_not_exists,
        checks,
        foreign_keys,
        foreign_key_order,
        key_constraints,
        persistence,
        on_commit,
        hierarchy,
        check_order,
        not_null_declarations: Vec::new(),
        element_order,
        untyped_columns,
    })
}

pub(in crate::compiler) fn constraint_name(name: &str) -> Option<String> {
    (!name.is_empty()).then(|| name.to_string())
}

pub(in crate::compiler) fn compile_column_key_constraints(
    column: &pg_query::protobuf::ColumnDef,
) -> Result<Vec<TableKeyConstraint>> {
    let mut keys = Vec::new();
    for node in &column.constraints {
        let Some(NodeEnum::Constraint(constraint)) = node.node.as_ref() else {
            return Err(SQLError::Internal(
                "column contains an invalid constraint".into(),
            ));
        };
        let kind = match constraint.contype() {
            pg_query::protobuf::ConstrType::ConstrPrimary => TableKeyConstraintKind::PrimaryKey,
            pg_query::protobuf::ConstrType::ConstrUnique => TableKeyConstraintKind::Unique,
            _ => continue,
        };
        keys.push(TableKeyConstraint {
            catalog_identity: None,
            index_identity: None,
            name: constraint_name(&constraint.conname),
            kind,
            columns: vec![column.colname.clone()],
            included_columns: Vec::new(),
            nulls_not_distinct: constraint.nulls_not_distinct,
            without_overlaps: constraint.without_overlaps,
        });
    }
    Ok(keys)
}

pub(in crate::compiler) fn compile_column_def(
    col: &pg_query::protobuf::ColumnDef,
) -> Result<(ColumnDef, Vec<TableCheck>, Vec<crate::ast::ForeignKey>)> {
    // A column option of `PARTITION OF` has no type; it takes the parent column's when the columns merge.
    let ty = if col.type_name.is_some() {
        compile_type_name(col)?
    } else {
        ColumnType::Named(String::new())
    };
    compile_column_def_with_type(col, ty)
}

#[expect(
    clippy::too_many_lines,
    reason = "ordered PostgreSQL lowering preserves syntax and error precedence"
)]
pub(in crate::compiler) fn compile_column_def_with_type(
    col: &pg_query::protobuf::ColumnDef,
    ty: ColumnType,
) -> Result<(ColumnDef, Vec<TableCheck>, Vec<crate::ast::ForeignKey>)> {
    let name = col.colname.clone();
    let raw_type = raw_type_name(col)?;
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
    let mut checks = Vec::new();
    let mut references: Option<crate::ast::ForeignKeyRef> = None;
    let mut column_references = Vec::new();
    #[derive(Clone, Copy)]
    enum EnforceableConstraint {
        Check,
        ForeignKey,
    }
    let mut last_enforceable = None;
    let mut saw_deferrability = false;
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
                    let mut identity = match cstr.generated_when.as_str() {
                        "a" => AutoIncrement::identity_always(),
                        "d" => AutoIncrement::identity_by_default(),
                        other => {
                            return Err(SQLError::Internal(format!(
                                "identity constraint has unknown generation {other:?}"
                            )));
                        }
                    };
                    identity.declaration =
                        super::super::sequences::compile_identity_declaration(&cstr.options)?;
                    auto_increment = Some(identity);
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
                    if let Some(expr) = check.take() {
                        checks.push(TableCheck {
                            name: check_name.take(),
                            expr,
                            enforced: check_enforced,
                            validated: check_validated,
                            no_inherit: check_no_inherit,
                            object_id: None,
                            catalog_oid: None,
                            is_local: true,
                            partition_constraint: None,
                        });
                    }
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
                    if let Some(previous) = references.take() {
                        column_references.push(previous);
                    }
                    references = Some(crate::ast::ForeignKeyRef {
                        referenced_key: None,
                        referenced_index: None,
                        name: constraint_name(&cstr.conname),
                        object_id: None,
                        catalog_identity: None,
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
                        referenced_partitions: Vec::new(),
                    });
                    last_enforceable = Some(EnforceableConstraint::ForeignKey);
                    saw_deferrability = false;
                }
                pg_query::protobuf::ConstrType::ConstrAttrDeferrable
                | pg_query::protobuf::ConstrType::ConstrAttrNotDeferrable
                | pg_query::protobuf::ConstrType::ConstrAttrDeferred
                | pg_query::protobuf::ConstrType::ConstrAttrImmediate => {
                    use pg_query::protobuf::ConstrType;
                    // `transformColumnDefinition` reports a misplaced or repeated attribute once it finds the relation; a valid one applies to the REFERENCES before it.
                    if let (Some(EnforceableConstraint::ForeignKey), Some(reference)) =
                        (last_enforceable, references.as_mut())
                    {
                        match cstr.contype() {
                            ConstrType::ConstrAttrDeferrable => {
                                saw_deferrability = true;
                                reference.deferrable = true;
                            }
                            ConstrType::ConstrAttrNotDeferrable => {
                                saw_deferrability = true;
                                reference.deferrable = false;
                            }
                            ConstrType::ConstrAttrDeferred => {
                                reference.initially_deferred = true;
                                if !saw_deferrability {
                                    reference.deferrable = true;
                                }
                            }
                            _ => reference.initially_deferred = false,
                        }
                    }
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
                        // A misplaced enforcement attribute is reported with the column's other clauses.
                        None => {}
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
    let mut column = ColumnDef {
        name,
        ty,
        object_id: None,
        attribute_number: None,
        missing_value: None,
        primary_key,
        not_null,
        not_null_explicit,
        not_null_name,
        not_null_identity: None,
        not_null_validated,
        not_null_no_inherit,
        not_null_is_local: true,
        auto_increment,
        unique,
        default,
        generated,
        check,
        check_name,
        check_enforced,
        check_validated,
        check_no_inherit,
        check_is_local: true,
        check_object_id: None,
        check_catalog_oid: None,
        default_catalog_oid: None,
        references,
    };
    if !checks.is_empty() {
        checks.push(
            crate::schema::constraint_changes::take_column_check(&mut column)
                .ok_or_else(|| SQLError::Internal("final column CHECK disappeared".into()))?,
        );
    }
    let foreign_keys = if column_references.is_empty() {
        Vec::new()
    } else {
        column_references.extend(column.references.take());
        column_references
            .iter()
            .map(|reference| crate::schema::foreign_keys::column_foreign_key(&column, reference))
            .collect()
    };
    Ok((column, checks, foreign_keys))
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
    let mut column_order = Vec::new();
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
        let key = compile_index_key(idx)?;
        let descending = idx.ordering() == pg_query::protobuf::SortByDir::SortbyDesc;
        let nulls_first = match idx.nulls_ordering() {
            pg_query::protobuf::SortByNulls::SortbyNullsFirst => true,
            pg_query::protobuf::SortByNulls::SortbyNullsLast => false,
            _ => descending,
        };
        column_order.push(crate::ast::IndexColumnOrder {
            descending,
            nulls_first,
        });
        columns.push(key);
    }
    let name = if stmt.idxname.is_empty() {
        None
    } else {
        Some(stmt.idxname.clone())
    };
    let mut options = Vec::new();
    let mut option_namespaces = Vec::new();
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
        if !elem.defnamespace.is_empty() {
            option_namespaces.push(elem.defnamespace.clone());
        }
        let value = match elem.arg.as_ref().and_then(|node| node.node.as_ref()) {
            Some(NodeEnum::String(value)) => value.sval.clone(),
            Some(NodeEnum::Integer(value)) => value.ival.to_string(),
            Some(NodeEnum::Float(value)) => value.fval.clone(),
            Some(NodeEnum::Boolean(value)) => value.boolval.to_string(),
            Some(NodeEnum::TypeName(value)) => extract_strings(&value.names)?.join("."),
            Some(other) => {
                return Err(SQLError::Unsupported(format!(
                    "CREATE INDEX option `{key}` value {other:?}"
                )));
            }
            None => "true".into(),
        };
        options.push((key, value));
    }
    let included_columns = stmt
        .index_including_params
        .iter()
        .map(|node| match node.node.as_ref() {
            Some(NodeEnum::IndexElem(index)) if !index.name.is_empty() => Ok(index.name.clone()),
            _ => Err(SQLError::Unsupported(
                "expressions are not supported in included columns".into(),
            )),
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(CreateIndex {
        included_columns,
        column_order,
        predicate: stmt
            .where_clause
            .as_deref()
            .map(compile_expr)
            .transpose()?
            .map(Box::new),
        name,
        table,
        access_method,
        columns,
        unique: stmt.unique,
        nulls_not_distinct: stmt.nulls_not_distinct,
        if_not_exists: stmt.if_not_exists,
        options,
        option_namespaces,
    })
}

fn compile_index_key(index: &pg_query::protobuf::IndexElem) -> Result<crate::ast::IndexKey> {
    if !index.name.is_empty() {
        return Ok(crate::ast::IndexKey::Column(index.name.clone()));
    }
    let expression = index.expr.as_deref().ok_or_else(|| {
        SQLError::Internal("CREATE INDEX key has neither a column nor an expression".into())
    })?;
    Ok(crate::ast::IndexKey::from_expression(compile_expr(
        expression,
    )?))
}

// -------------------------------------------------------------------------
// INSERT
// -------------------------------------------------------------------------
