//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! PL/pgSQL parser invocation, datum lowering, and condition normalization.

use super::{
    condition_sqlstate, ensure_single_tag, expect_tag, json_bool_or_false, json_kind,
    json_optional_i64, json_usize_or_zero, lower_block, lower_expr, lower_full_statement,
    normalize_plpgsql_type, optional_array, require, require_nonempty_str,
    validate_assignable_datum, CreateFunction, FunctionBody, FunctionParamMode, FunctionReturns,
    JSONValue, PLpgSQLCursor, PLpgSQLDatum, PLpgSQLFunction, PLpgSQLRowField, PLpgSQLVar, Result,
    SQLError,
};

pub fn parse_function(def: &CreateFunction) -> Result<PLpgSQLFunction> {
    let FunctionBody::Source(body) = &def.body else {
        return Err(SQLError::Internal(
            "PL/pgSQL parser invoked on a SQL-standard body".into(),
        ));
    };
    let text = synthesize_create_text(def, body);
    parse_plpgsql_text(&text)
}

/// Parse a `DO $$ ... $$` body by wrapping it into an anonymous
/// void-returning function.
pub fn parse_do_block(body: &str) -> Result<PLpgSQLFunction> {
    let tag = fresh_dollar_tag(body);
    let text = format!(
        "CREATE FUNCTION __uqa_do_block__() RETURNS void AS {tag}{body}{tag} LANGUAGE plpgsql;"
    );
    parse_plpgsql_text(&text)
}

/// Canonical `CREATE FUNCTION` / `CREATE PROCEDURE` text used solely
/// to feed the `PL/pgSQL` parser (parameter DEFAULTs are resolved at
/// call time and intentionally omitted).
pub(super) fn synthesize_create_text(def: &CreateFunction, body: &str) -> String {
    let mut sql = String::new();
    sql.push_str(if def.is_procedure {
        "CREATE PROCEDURE "
    } else {
        "CREATE FUNCTION "
    });
    sql.push_str(&quote_ident(&def.name));
    sql.push('(');
    let mut first = true;
    for p in &def.params {
        if matches!(p.mode, FunctionParamMode::Table) {
            continue;
        }
        if !first {
            sql.push_str(", ");
        }
        first = false;
        match p.mode {
            FunctionParamMode::Out => sql.push_str("OUT "),
            FunctionParamMode::InOut => sql.push_str("INOUT "),
            FunctionParamMode::In | FunctionParamMode::Table => {}
        }
        if !p.name.is_empty() {
            sql.push_str(&quote_ident(&p.name));
            sql.push(' ');
        }
        sql.push_str(&p.type_name);
    }
    sql.push(')');
    match &def.returns {
        FunctionReturns::None => {}
        FunctionReturns::Scalar { type_name } => {
            sql.push_str(" RETURNS ");
            sql.push_str(type_name);
        }
        FunctionReturns::SetOf { type_name } => {
            sql.push_str(" RETURNS SETOF ");
            sql.push_str(type_name);
        }
        FunctionReturns::Table => {
            sql.push_str(" RETURNS TABLE(");
            let mut first_col = true;
            for p in &def.params {
                if !matches!(p.mode, FunctionParamMode::Table) {
                    continue;
                }
                if !first_col {
                    sql.push_str(", ");
                }
                first_col = false;
                sql.push_str(&quote_ident(&p.name));
                sql.push(' ');
                sql.push_str(&p.type_name);
            }
            sql.push(')');
        }
    }
    let tag = fresh_dollar_tag(body);
    sql.push_str(" AS ");
    sql.push_str(&tag);
    sql.push_str(body);
    sql.push_str(&tag);
    sql.push_str(" LANGUAGE plpgsql;");
    sql
}

pub(super) fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Dollar-quote tag guaranteed not to collide with the body text.
pub(super) fn fresh_dollar_tag(body: &str) -> String {
    let mut n = 0usize;
    loop {
        let tag = format!("$__uqa_plpgsql_{n}$");
        if !body.contains(&tag) {
            return tag;
        }
        n += 1;
    }
}

pub(super) fn parse_plpgsql_text(text: &str) -> Result<PLpgSQLFunction> {
    let json = pg_query::parse_plpgsql(text)?;
    let functions = json
        .as_array()
        .ok_or_else(|| SQLError::Internal("PL/pgSQL parse returned no function list".into()))?;
    if functions.len() != 1 {
        return Err(SQLError::Internal(format!(
            "PL/pgSQL parse returned {} functions; expected exactly one",
            functions.len()
        )));
    }
    let function = expect_tag(&functions[0], "PLpgSQL_function", "parsed function")?;
    lower_function(function)
}

// ---------------------------------------------------------------------
// JSON lowering
// ---------------------------------------------------------------------

/// Divergence from `PostgreSQL`: the JSON dump does not carry each
/// block's `initvarnos`, so declared-variable defaults (including
/// those of nested `DECLARE` sections) are evaluated once at routine
/// entry rather than on every block entry, and a nested declaration
/// shadows its outer namesake for the whole body.
pub(super) fn lower_function(function: &JSONValue) -> Result<PLpgSQLFunction> {
    let raw_datums = function
        .get("datums")
        .and_then(JSONValue::as_array)
        .ok_or_else(|| SQLError::Internal("PL/pgSQL function without datums".into()))?;
    let mut datums = Vec::with_capacity(raw_datums.len());
    for raw in raw_datums {
        datums.push(lower_datum(raw)?);
    }
    validate_datums(&datums)?;
    let found_datum = datums
        .iter()
        .position(|d| matches!(d, PLpgSQLDatum::Var(v) if v.name.eq_ignore_ascii_case("found")));
    let raw_action = require(function, "action")?;
    let action = expect_tag(raw_action, "PLpgSQL_stmt_block", "function body")?;
    let action = lower_block(action, &datums)?;
    Ok(PLpgSQLFunction {
        datums,
        action,
        found_datum,
    })
}

pub(super) fn lower_datum(raw: &JSONValue) -> Result<PLpgSQLDatum> {
    ensure_single_tag(raw, "datum")?;
    if let Some(var) = raw.get("PLpgSQL_var") {
        let name = require_nonempty_str(var, "refname", "variable datum")?;
        let datatype = require(var, "datatype")?;
        let datatype = expect_tag(datatype, "PLpgSQL_type", "variable datatype")?;
        let type_name = normalize_plpgsql_type(&require_nonempty_str(
            datatype,
            "typname",
            "variable datatype",
        )?);
        if type_name.is_empty() {
            return Err(SQLError::Internal(format!(
                "PL/pgSQL variable `{name}` has an empty normalized type"
            )));
        }
        let default = match var.get("default_val") {
            Some(node) => Some(lower_expr(node)?),
            None => None,
        };
        let cursor = match var.get("cursor_explicit_expr") {
            Some(query) => Some(PLpgSQLCursor {
                query: lower_full_statement(query)?,
                argument_row: match json_optional_i64(var, "cursor_explicit_argrow")? {
                    None | Some(-1) => None,
                    Some(index) if index >= 0 => Some(usize::try_from(index).map_err(|_| {
                        SQLError::Internal(format!(
                            "PL/pgSQL cursor `{name}` argument row {index} does not fit this platform"
                        ))
                    })?),
                    Some(index) => {
                        return Err(SQLError::Internal(format!(
                            "PL/pgSQL cursor `{name}` has invalid argument row {index}"
                        )));
                    }
                },
            }),
            None => {
                if var.get("cursor_explicit_argrow").is_some() {
                    return Err(SQLError::Internal(format!(
                        "PL/pgSQL cursor variable `{name}` has arguments but no query"
                    )));
                }
                None
            }
        };
        return Ok(PLpgSQLDatum::Var(PLpgSQLVar {
            name,
            type_name,
            default,
            constant: json_bool_or_false(var, "isconst")?,
            not_null: json_bool_or_false(var, "notnull")?,
            cursor,
            lineno: json_optional_i64(var, "lineno")?,
        }));
    }
    if let Some(rec) = raw.get("PLpgSQL_rec") {
        return Ok(PLpgSQLDatum::Rec {
            name: require_nonempty_str(rec, "refname", "record datum")?,
        });
    }
    if let Some(field) = raw.get("PLpgSQL_recfield") {
        return Ok(PLpgSQLDatum::RecField {
            field: require_nonempty_str(field, "fieldname", "record-field datum")?,
            // libpg_query omits a zero-valued recparentno.
            parent: json_usize_or_zero(field, "recparentno")?,
        });
    }
    if let Some(row) = raw.get("PLpgSQL_row") {
        return Ok(PLpgSQLDatum::Row {
            fields: lower_row_fields(row)?,
        });
    }
    Err(SQLError::Unsupported(format!(
        "PL/pgSQL datum {}",
        json_kind(raw)
    )))
}

pub(super) fn lower_row_fields(row: &JSONValue) -> Result<Vec<PLpgSQLRowField>> {
    let mut out = Vec::new();
    if let Some(fields) = optional_array(row, "fields")? {
        for f in fields {
            // libpg_query's JSON dump omits zero-valued fields, so a
            // missing varno means datum 0.
            out.push(PLpgSQLRowField {
                name: require_nonempty_str(f, "name", "row target field")?,
                varno: json_usize_or_zero(f, "varno")?,
            });
        }
    }
    Ok(out)
}

pub(super) fn validate_datums(datums: &[PLpgSQLDatum]) -> Result<()> {
    for (idx, datum) in datums.iter().enumerate() {
        match datum {
            PLpgSQLDatum::RecField { parent, .. } => {
                let Some(parent_datum) = datums.get(*parent) else {
                    return Err(SQLError::Internal(format!(
                        "PL/pgSQL record-field datum {idx} references missing parent datum {parent}"
                    )));
                };
                if !matches!(parent_datum, PLpgSQLDatum::Rec { .. }) {
                    return Err(SQLError::Internal(format!(
                        "PL/pgSQL record-field datum {idx} parent {parent} is not a record"
                    )));
                }
            }
            PLpgSQLDatum::Row { fields } => {
                if fields.is_empty() {
                    return Err(SQLError::Internal(format!(
                        "PL/pgSQL row datum {idx} has no fields"
                    )));
                }
                for field in fields {
                    validate_assignable_datum(datums, field.varno, "row target field")?;
                }
            }
            PLpgSQLDatum::Var(var) => {
                if let Some(cursor) = &var.cursor {
                    if var.type_name != "refcursor" {
                        return Err(SQLError::Internal(format!(
                            "PL/pgSQL bound cursor `{}` is not a refcursor datum",
                            var.name
                        )));
                    }
                    if let Some(argument_row) = cursor.argument_row {
                        if !matches!(datums.get(argument_row), Some(PLpgSQLDatum::Row { .. })) {
                            return Err(SQLError::Internal(format!(
                                "PL/pgSQL cursor `{}` references invalid argument row {argument_row}",
                                var.name
                            )));
                        }
                    }
                }
            }
            PLpgSQLDatum::Rec { .. } => {}
        }
    }
    Ok(())
}

pub(super) fn normalize_condition(value: String, allow_others: bool) -> Result<String> {
    let lower = value.to_ascii_lowercase();
    if allow_others && lower == "others" {
        return Ok(lower);
    }
    if condition_sqlstate(&lower).is_some() {
        return Ok(lower);
    }
    let upper = value.to_ascii_uppercase();
    if upper.len() == 5
        && upper
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
    {
        return Ok(upper);
    }
    Err(SQLError::Internal(format!(
        "unrecognized PL/pgSQL exception condition `{value}`"
    )))
}
