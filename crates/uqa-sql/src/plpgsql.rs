//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PL/pgSQL` function bodies: typed AST, parser, and the variable
//! binding rewriter.
//!
//! Bodies are parsed with `libpg_query`'s `PL/pgSQL` parser
//! (`pg_query::parse_plpgsql`), which returns the same JSON dump
//! `PostgreSQL` itself produces. This module lowers that JSON into a
//! typed AST whose embedded SQL fragments are pre-compiled into
//! [`Expr`] / [`Statement`] values, ready for execution against the
//! engine.
//!
//! Variable references inside embedded SQL are plain column
//! references after compilation. At execution time the interpreter
//! rewrites them into literals through [`VariableResolver`] /
//! [`bind_expr`] / [`bind_statement`] before handing the statement to
//! the engine. This matches `plpgsql.variable_conflict =
//! use_variable` resolution: a name that is both a `PL/pgSQL`
//! variable and a column of a queried table resolves to the variable
//! (stock `PostgreSQL` raises an ambiguity error instead).

use serde_json::Value as JSONValue;
use uqa_core::Value;

use crate::ast::{
    CreateFunction, Expr, FromClause, FunctionBody, FunctionParamMode, FunctionReturns, MergeWhen,
    Projection, SelectStmt, Statement, CTE,
};
use crate::error::{Result, SQLError};

// ---------------------------------------------------------------------
// Typed AST
// ---------------------------------------------------------------------

/// A parsed `PL/pgSQL` function body: the flat datum table plus the
/// outermost block.
#[derive(Debug, Clone)]
pub struct PLpgSQLFunction {
    pub datums: Vec<PLpgSQLDatum>,
    pub action: PLpgSQLBlock,
    /// Index of the implicit `FOUND` variable in [`Self::datums`].
    pub found_datum: Option<usize>,
}

impl PLpgSQLFunction {
    /// Datum indices used as `FOR i IN a..b` loop counters. The
    /// interpreter binds these names only while their loop runs so an
    /// outer variable with the same name stays visible elsewhere.
    pub fn fori_variable_datums(&self) -> std::collections::BTreeSet<usize> {
        let mut out = std::collections::BTreeSet::new();
        collect_fori_vars_block(&self.action, &mut out);
        out
    }
}

fn collect_fori_vars_block(block: &PLpgSQLBlock, out: &mut std::collections::BTreeSet<usize>) {
    collect_fori_vars_stmts(&block.body, out);
    for arm in &block.exceptions {
        collect_fori_vars_stmts(&arm.body, out);
    }
}

fn collect_fori_vars_stmts(stmts: &[PLpgSQLStmt], out: &mut std::collections::BTreeSet<usize>) {
    for stmt in stmts {
        match stmt {
            PLpgSQLStmt::Block(block) => collect_fori_vars_block(block, out),
            PLpgSQLStmt::If {
                then_body,
                elsifs,
                else_body,
                ..
            } => {
                collect_fori_vars_stmts(then_body, out);
                for (_, body) in elsifs {
                    collect_fori_vars_stmts(body, out);
                }
                if let Some(body) = else_body {
                    collect_fori_vars_stmts(body, out);
                }
            }
            PLpgSQLStmt::Case {
                arms, else_body, ..
            } => {
                for (_, body) in arms {
                    collect_fori_vars_stmts(body, out);
                }
                if let Some(body) = else_body {
                    collect_fori_vars_stmts(body, out);
                }
            }
            PLpgSQLStmt::Loop { body, .. } | PLpgSQLStmt::While { body, .. } => {
                collect_fori_vars_stmts(body, out);
            }
            PLpgSQLStmt::ForI { var, body, .. } => {
                out.insert(*var);
                collect_fori_vars_stmts(body, out);
            }
            PLpgSQLStmt::ForQuery { body, .. } => collect_fori_vars_stmts(body, out),
            _ => {}
        }
    }
}

/// One entry in the function's flat datum table. `varno` / `dno`
/// references inside statements index into this table.
#[derive(Debug, Clone)]
pub enum PLpgSQLDatum {
    Var(PLpgSQLVar),
    /// `RECORD` variable (also `FOR rec IN ...` loop targets).
    Rec {
        name: String,
    },
    /// `rec.field` assignment target.
    RecField {
        field: String,
        parent: usize,
    },
    /// Multi-variable target list (`SELECT ... INTO a, b`).
    Row {
        fields: Vec<PLpgSQLRowField>,
    },
}

impl PLpgSQLDatum {
    pub fn name(&self) -> Option<&str> {
        match self {
            PLpgSQLDatum::Var(v) => Some(&v.name),
            PLpgSQLDatum::Rec { name } => Some(name),
            PLpgSQLDatum::RecField { .. } | PLpgSQLDatum::Row { .. } => None,
        }
    }
}

/// Scalar `PL/pgSQL` variable (declared variable, parameter, loop
/// counter, or an internal compiler temporary).
#[derive(Debug, Clone)]
pub struct PLpgSQLVar {
    pub name: String,
    /// Normalized type name (`integer`, `text`, ...). Types the value
    /// layer cannot cast (e.g. `%type`, `record`) are kept verbatim
    /// and skipped at assignment time.
    pub type_name: String,
    pub default: Option<Expr>,
    pub constant: bool,
    pub not_null: bool,
    /// Source line of the declaration; used to disambiguate loop
    /// variables that shadow outer names.
    pub lineno: Option<i64>,
}

/// `name -> datum` slot of a row target.
#[derive(Debug, Clone)]
pub struct PLpgSQLRowField {
    pub name: String,
    pub varno: usize,
}

/// `[DECLARE ...] BEGIN ... [EXCEPTION ...] END` block.
#[derive(Debug, Clone)]
pub struct PLpgSQLBlock {
    pub label: Option<String>,
    pub body: Vec<PLpgSQLStmt>,
    pub exceptions: Vec<PLpgSQLExceptionArm>,
}

/// One `WHEN cond [OR cond ...] THEN stmts` arm of an exception
/// section.
#[derive(Debug, Clone)]
pub struct PLpgSQLExceptionArm {
    /// Lower-cased condition names (`others`, `division_by_zero`,
    /// ...). Explicit `SQLSTATE 'xxxxx'` conditions arrive as the
    /// five-character code.
    pub conditions: Vec<String>,
    pub body: Vec<PLpgSQLStmt>,
}

/// `RAISE` severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RaiseLevel {
    Debug,
    Log,
    Info,
    Notice,
    Warning,
    Error,
}

impl RaiseLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            RaiseLevel::Debug => "DEBUG",
            RaiseLevel::Log => "LOG",
            RaiseLevel::Info => "INFO",
            RaiseLevel::Notice => "NOTICE",
            RaiseLevel::Warning => "WARNING",
            RaiseLevel::Error => "ERROR",
        }
    }
}

/// Assignment / `INTO` target.
#[derive(Debug, Clone)]
pub enum IntoTarget {
    /// A `RECORD` variable receives the whole row.
    Rec(usize),
    /// Positional list of scalar targets.
    Row(Vec<PLpgSQLRowField>),
}

/// Executable `PL/pgSQL` statement.
#[derive(Debug, Clone)]
pub enum PLpgSQLStmt {
    Block(PLpgSQLBlock),
    /// `target := expr` (also `=`). `target` indexes the datum table.
    Assign {
        target: usize,
        expr: Expr,
    },
    If {
        cond: Expr,
        then_body: Vec<PLpgSQLStmt>,
        elsifs: Vec<(Expr, Vec<PLpgSQLStmt>)>,
        else_body: Option<Vec<PLpgSQLStmt>>,
    },
    /// CASE statement. Simple form carries `t_expr` + the temporary
    /// datum the compiler references from each rewritten WHEN.
    Case {
        t_expr: Option<Expr>,
        t_varno: Option<usize>,
        arms: Vec<(Expr, Vec<PLpgSQLStmt>)>,
        else_body: Option<Vec<PLpgSQLStmt>>,
    },
    Loop {
        label: Option<String>,
        body: Vec<PLpgSQLStmt>,
    },
    While {
        label: Option<String>,
        cond: Expr,
        body: Vec<PLpgSQLStmt>,
    },
    /// `FOR i IN [REVERSE] lower..upper [BY step] LOOP`.
    ForI {
        label: Option<String>,
        var: usize,
        lower: Expr,
        upper: Expr,
        step: Option<Expr>,
        reverse: bool,
        body: Vec<PLpgSQLStmt>,
    },
    /// `FOR target IN <query> LOOP`.
    ForQuery {
        label: Option<String>,
        target: IntoTarget,
        query: Statement,
        body: Vec<PLpgSQLStmt>,
    },
    /// `EXIT` (`is_exit`) or `CONTINUE`, optionally labelled and
    /// conditional (`WHEN cond`).
    Exit {
        is_exit: bool,
        label: Option<String>,
        cond: Option<Expr>,
    },
    Return {
        expr: Option<Expr>,
    },
    /// `RETURN NEXT [expr]` - bare form emits the current OUT /
    /// TABLE column values.
    ReturnNext {
        expr: Option<Expr>,
    },
    ReturnQuery {
        query: Statement,
    },
    ReturnQueryExecute {
        query: Expr,
        params: Vec<Expr>,
    },
    Raise {
        level: RaiseLevel,
        condition: Option<String>,
        message: Option<String>,
        params: Vec<Expr>,
    },
    /// Embedded SQL statement, optionally `INTO [STRICT] target`.
    ExecSQL {
        stmt: Statement,
        into: Option<IntoTarget>,
        strict: bool,
    },
    /// `EXECUTE <string> [INTO [STRICT] target] [USING params]`.
    DynExecute {
        query: Expr,
        params: Vec<Expr>,
        into: Option<IntoTarget>,
        strict: bool,
    },
    Perform {
        query: Statement,
    },
    /// `GET DIAGNOSTICS var = KIND [, ...]` as `(kind, target datum)`.
    GetDiagnostics {
        items: Vec<(String, usize)>,
    },
}

// ---------------------------------------------------------------------
// Parsing: definition -> canonical text -> libpg_query JSON -> AST
// ---------------------------------------------------------------------

/// Parse the `PL/pgSQL` body of a stored definition. The definition
/// is re-serialized into a canonical `CREATE FUNCTION` statement so
/// restore-from-catalog and fresh DDL take the same path.
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
fn synthesize_create_text(def: &CreateFunction, body: &str) -> String {
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

fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Dollar-quote tag guaranteed not to collide with the body text.
fn fresh_dollar_tag(body: &str) -> String {
    let mut n = 0usize;
    loop {
        let tag = format!("$__uqa_plpgsql_{n}$");
        if !body.contains(&tag) {
            return tag;
        }
        n += 1;
    }
}

fn parse_plpgsql_text(text: &str) -> Result<PLpgSQLFunction> {
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
fn lower_function(function: &JSONValue) -> Result<PLpgSQLFunction> {
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

fn lower_datum(raw: &JSONValue) -> Result<PLpgSQLDatum> {
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
        return Ok(PLpgSQLDatum::Var(PLpgSQLVar {
            name,
            type_name,
            default,
            constant: json_bool_or_false(var, "isconst")?,
            not_null: json_bool_or_false(var, "notnull")?,
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

fn lower_row_fields(row: &JSONValue) -> Result<Vec<PLpgSQLRowField>> {
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

fn validate_datums(datums: &[PLpgSQLDatum]) -> Result<()> {
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
            PLpgSQLDatum::Var(_) | PLpgSQLDatum::Rec { .. } => {}
        }
    }
    Ok(())
}

fn normalize_condition(value: String, allow_others: bool) -> Result<String> {
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

fn lower_block(block: &JSONValue, datums: &[PLpgSQLDatum]) -> Result<PLpgSQLBlock> {
    let body = lower_optional_stmt_list(block, "body", datums)?;
    let mut exceptions = Vec::new();
    if let Some(raw_exceptions) = block.get("exceptions") {
        let exc = expect_tag(raw_exceptions, "PLpgSQL_exception_block", "exception block")?;
        let list = optional_array(exc, "exc_list")?.ok_or_else(|| {
            SQLError::Internal("PL/pgSQL exception block without an arm list".into())
        })?;
        if list.is_empty() {
            return Err(SQLError::Internal(
                "PL/pgSQL exception block without arms".into(),
            ));
        }
        for arm in list {
            let arm = expect_tag(arm, "PLpgSQL_exception", "exception arm")?;
            let mut conditions = Vec::new();
            if let Some(conds) = optional_array(arm, "conditions")? {
                for cond in conds {
                    let cond = expect_tag(cond, "PLpgSQL_condition", "exception condition")?;
                    let name = json_optional_str(cond, "condname")?;
                    let state = json_optional_str(cond, "sqlstate")?;
                    let value = match (name, state) {
                        (Some(name), None) if !name.is_empty() => name,
                        (None, Some(state)) if !state.is_empty() => state,
                        _ => {
                            return Err(SQLError::Internal(
                                    "PL/pgSQL exception condition must have exactly one non-empty condition name or SQLSTATE"
                                        .into(),
                                ));
                        }
                    };
                    conditions.push(normalize_condition(value, true)?);
                }
            }
            if conditions.is_empty() {
                return Err(SQLError::Internal(
                    "PL/pgSQL exception arm without conditions".into(),
                ));
            }
            let body = lower_optional_stmt_list(arm, "action", datums)?;
            exceptions.push(PLpgSQLExceptionArm { conditions, body });
        }
    }
    Ok(PLpgSQLBlock {
        label: json_optional_str(block, "label")?,
        body,
        exceptions,
    })
}

fn lower_stmt_list(list: &JSONValue, datums: &[PLpgSQLDatum]) -> Result<Vec<PLpgSQLStmt>> {
    let items = list
        .as_array()
        .ok_or_else(|| SQLError::Internal("PL/pgSQL statement list is not an array".into()))?;
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        out.push(lower_stmt(item, datums)?);
    }
    Ok(out)
}

fn lower_optional_stmt_list(
    object: &JSONValue,
    key: &str,
    datums: &[PLpgSQLDatum],
) -> Result<Vec<PLpgSQLStmt>> {
    match object.get(key) {
        Some(list) => lower_stmt_list(list, datums),
        None => Ok(Vec::new()),
    }
}

fn lower_stmt(raw: &JSONValue, datums: &[PLpgSQLDatum]) -> Result<PLpgSQLStmt> {
    ensure_single_tag(raw, "statement")?;
    if let Some(block) = raw.get("PLpgSQL_stmt_block") {
        return Ok(PLpgSQLStmt::Block(lower_block(block, datums)?));
    }
    if let Some(stmt) = raw.get("PLpgSQL_stmt_assign") {
        // Zero-valued varno fields are omitted from the JSON dump.
        let target = json_usize_or_zero(stmt, "varno")?;
        validate_assignable_datum(datums, target, "assignment target")?;
        let expr =
            lower_expr(stmt.get("expr").ok_or_else(|| {
                SQLError::Internal("PL/pgSQL assignment without expression".into())
            })?)?;
        return Ok(PLpgSQLStmt::Assign { target, expr });
    }
    if let Some(stmt) = raw.get("PLpgSQL_stmt_if") {
        let cond = lower_expr(require(stmt, "cond")?)?;
        let then_body = lower_optional_stmt_list(stmt, "then_body", datums)?;
        let mut elsifs = Vec::new();
        if let Some(list) = optional_array(stmt, "elsif_list")? {
            for e in list {
                let e = expect_tag(e, "PLpgSQL_if_elsif", "ELSIF arm")?;
                let cond = lower_expr(require(e, "cond")?)?;
                let body = lower_optional_stmt_list(e, "stmts", datums)?;
                elsifs.push((cond, body));
            }
        }
        let else_body = match stmt.get("else_body") {
            Some(body) => Some(lower_stmt_list(body, datums)?),
            None => None,
        };
        return Ok(PLpgSQLStmt::If {
            cond,
            then_body,
            elsifs,
            else_body,
        });
    }
    if let Some(stmt) = raw.get("PLpgSQL_stmt_case") {
        let t_expr = match stmt.get("t_expr") {
            Some(node) => Some(lower_expr(node)?),
            None => None,
        };
        let t_varno = if t_expr.is_some() {
            let index = json_usize_or_zero(stmt, "t_varno")?;
            validate_scalar_datum(datums, index, "CASE temporary")?;
            Some(index)
        } else {
            if json_optional_usize(stmt, "t_varno")?.is_some() {
                return Err(SQLError::Internal(
                    "PL/pgSQL searched CASE has a temporary datum but no expression".into(),
                ));
            }
            None
        };
        let mut arms = Vec::new();
        if let Some(list) = optional_array(stmt, "case_when_list")? {
            for arm in list {
                let arm = expect_tag(arm, "PLpgSQL_case_when", "CASE arm")?;
                let cond = lower_expr(require(arm, "expr")?)?;
                let body = lower_optional_stmt_list(arm, "stmts", datums)?;
                arms.push((cond, body));
            }
        }
        if arms.is_empty() {
            return Err(SQLError::Internal("PL/pgSQL CASE without arms".into()));
        }
        let have_else = json_bool_or_false(stmt, "have_else")?;
        let else_body = if have_else {
            Some(lower_optional_stmt_list(stmt, "else_stmts", datums)?)
        } else {
            if stmt.get("else_stmts").is_some() {
                return Err(SQLError::Internal(
                    "PL/pgSQL CASE has ELSE statements while have_else is false".into(),
                ));
            }
            None
        };
        return Ok(PLpgSQLStmt::Case {
            t_expr,
            t_varno,
            arms,
            else_body,
        });
    }
    if let Some(stmt) = raw.get("PLpgSQL_stmt_loop") {
        return Ok(PLpgSQLStmt::Loop {
            label: json_optional_str(stmt, "label")?,
            body: lower_optional_stmt_list(stmt, "body", datums)?,
        });
    }
    if let Some(stmt) = raw.get("PLpgSQL_stmt_while") {
        return Ok(PLpgSQLStmt::While {
            label: json_optional_str(stmt, "label")?,
            cond: lower_expr(require(stmt, "cond")?)?,
            body: lower_optional_stmt_list(stmt, "body", datums)?,
        });
    }
    if let Some(stmt) = raw.get("PLpgSQL_stmt_fori") {
        let var_obj = require(stmt, "var")?
            .get("PLpgSQL_var")
            .ok_or_else(|| SQLError::Internal("FOR loop variable is not a var".into()))?;
        let name = require_nonempty_str(var_obj, "refname", "FOR loop variable")?;
        let lineno = json_optional_i64(var_obj, "lineno")?;
        let var = find_var_datum(datums, &name, lineno).ok_or_else(|| {
            SQLError::Internal(format!("FOR loop variable `{name}` has no datum"))
        })?;
        let step = match stmt.get("step") {
            Some(node) => Some(lower_expr(node)?),
            None => None,
        };
        return Ok(PLpgSQLStmt::ForI {
            label: json_optional_str(stmt, "label")?,
            var,
            lower: lower_expr(require(stmt, "lower")?)?,
            upper: lower_expr(require(stmt, "upper")?)?,
            step,
            reverse: json_bool_or_false(stmt, "reverse")?,
            body: lower_optional_stmt_list(stmt, "body", datums)?,
        });
    }
    if let Some(stmt) = raw.get("PLpgSQL_stmt_fors") {
        let target = lower_into_target(require(stmt, "var")?, datums)?;
        let query = lower_full_statement(require(stmt, "query")?)?;
        return Ok(PLpgSQLStmt::ForQuery {
            label: json_optional_str(stmt, "label")?,
            target,
            query,
            body: lower_optional_stmt_list(stmt, "body", datums)?,
        });
    }
    if let Some(stmt) = raw.get("PLpgSQL_stmt_exit") {
        let cond = match stmt.get("cond") {
            Some(node) => Some(lower_expr(node)?),
            None => None,
        };
        return Ok(PLpgSQLStmt::Exit {
            is_exit: json_bool_or_false(stmt, "is_exit")?,
            label: json_optional_str(stmt, "label")?,
            cond,
        });
    }
    if let Some(stmt) = raw.get("PLpgSQL_stmt_return") {
        let expr = match stmt.get("expr") {
            Some(node) => Some(lower_expr(node)?),
            None => None,
        };
        return Ok(PLpgSQLStmt::Return { expr });
    }
    if let Some(stmt) = raw.get("PLpgSQL_stmt_return_next") {
        let expr = match stmt.get("expr") {
            Some(node) => Some(lower_expr(node)?),
            None => None,
        };
        return Ok(PLpgSQLStmt::ReturnNext { expr });
    }
    if let Some(stmt) = raw.get("PLpgSQL_stmt_return_query") {
        if let Some(query) = stmt.get("query") {
            return Ok(PLpgSQLStmt::ReturnQuery {
                query: lower_full_statement(query)?,
            });
        }
        if let Some(dynquery) = stmt.get("dynquery") {
            return Ok(PLpgSQLStmt::ReturnQueryExecute {
                query: lower_expr(dynquery)?,
                params: lower_expr_list(stmt.get("params"))?,
            });
        }
        return Err(SQLError::Internal("RETURN QUERY without a query".into()));
    }
    if let Some(stmt) = raw.get("PLpgSQL_stmt_raise") {
        if stmt.get("options").is_some() {
            return Err(SQLError::Unsupported("RAISE ... USING options".into()));
        }
        let raw_level = require_i64(stmt, "elog_level", "RAISE statement")?;
        let level = match raw_level {
            10..=14 => RaiseLevel::Debug,
            15 | 16 => RaiseLevel::Log,
            17 => RaiseLevel::Info,
            18 => RaiseLevel::Notice,
            19 | 20 => RaiseLevel::Warning,
            21 => RaiseLevel::Error,
            other => {
                return Err(SQLError::Internal(format!(
                    "PL/pgSQL RAISE has invalid elog level {other}"
                )));
            }
        };
        let condition = json_optional_str(stmt, "condname")?
            .map(|condition| normalize_condition(condition, false))
            .transpose()?;
        return Ok(PLpgSQLStmt::Raise {
            level,
            condition,
            message: json_optional_str(stmt, "message")?,
            params: lower_expr_list(stmt.get("params"))?,
        });
    }
    if let Some(stmt) = raw.get("PLpgSQL_stmt_execsql") {
        let sql = lower_full_statement(require(stmt, "sqlstmt")?)?;
        let has_into = json_bool_or_false(stmt, "into")?;
        let strict = json_bool_or_false(stmt, "strict")?;
        if strict && !has_into {
            return Err(SQLError::Internal(
                "PL/pgSQL SQL statement is STRICT without INTO".into(),
            ));
        }
        let into = if has_into {
            Some(lower_into_target(
                require(stmt, "target").map_err(|_| {
                    SQLError::Internal("PL/pgSQL SQL statement has INTO but no target".into())
                })?,
                datums,
            )?)
        } else {
            if stmt.get("target").is_some() {
                return Err(SQLError::Internal(
                    "PL/pgSQL SQL statement has a target but INTO is false".into(),
                ));
            }
            None
        };
        return Ok(PLpgSQLStmt::ExecSQL {
            stmt: sql,
            into,
            strict,
        });
    }
    if let Some(stmt) = raw.get("PLpgSQL_stmt_dynexecute") {
        let query = lower_expr(require(stmt, "query")?)?;
        let has_into = json_bool_or_false(stmt, "into")?;
        let strict = json_bool_or_false(stmt, "strict")?;
        if strict && !has_into {
            return Err(SQLError::Internal(
                "PL/pgSQL EXECUTE is STRICT without INTO".into(),
            ));
        }
        let into = if has_into {
            Some(lower_into_target(
                require(stmt, "target").map_err(|_| {
                    SQLError::Internal("PL/pgSQL EXECUTE has INTO but no target".into())
                })?,
                datums,
            )?)
        } else {
            if stmt.get("target").is_some() {
                return Err(SQLError::Internal(
                    "PL/pgSQL EXECUTE has a target but INTO is false".into(),
                ));
            }
            None
        };
        return Ok(PLpgSQLStmt::DynExecute {
            query,
            params: lower_expr_list(stmt.get("params"))?,
            into,
            strict,
        });
    }
    if let Some(stmt) = raw.get("PLpgSQL_stmt_perform") {
        return Ok(PLpgSQLStmt::Perform {
            query: lower_full_statement(require(stmt, "expr")?)?,
        });
    }
    if let Some(stmt) = raw.get("PLpgSQL_stmt_call") {
        // CALL inside a body: run the CALL statement; INOUT results
        // flow back through the target row like an INTO clause.
        let call = lower_full_statement(require(stmt, "expr")?)?;
        let into = match stmt.get("target") {
            Some(target) => Some(lower_into_target(target, datums)?),
            None => None,
        };
        return Ok(PLpgSQLStmt::ExecSQL {
            stmt: call,
            into,
            strict: false,
        });
    }
    if let Some(stmt) = raw.get("PLpgSQL_stmt_getdiag") {
        if json_bool_or_false(stmt, "is_stacked")? {
            return Err(SQLError::Unsupported("GET STACKED DIAGNOSTICS".into()));
        }
        let mut items = Vec::new();
        if let Some(list) = optional_array(stmt, "diag_items")? {
            for item in list {
                let item = expect_tag(item, "PLpgSQL_diag_item", "diagnostics item")?;
                let kind =
                    require_nonempty_str(item, "kind", "diagnostics item")?.to_ascii_uppercase();
                let target = json_usize_or_zero(item, "target")?;
                validate_scalar_datum(datums, target, "diagnostics target")?;
                items.push((kind, target));
            }
        }
        if items.is_empty() {
            return Err(SQLError::Internal(
                "PL/pgSQL GET DIAGNOSTICS without items".into(),
            ));
        }
        return Ok(PLpgSQLStmt::GetDiagnostics { items });
    }
    Err(SQLError::Unsupported(format!(
        "PL/pgSQL statement {}",
        json_kind(raw)
    )))
}

/// Resolve a loop variable to its datum index. The JSON dump omits
/// `dno` on embedded vars, so match by name + declaration line, then
/// fall back to the last datum with that name.
fn find_var_datum(datums: &[PLpgSQLDatum], name: &str, lineno: Option<i64>) -> Option<usize> {
    if lineno.is_some() {
        for (idx, d) in datums.iter().enumerate() {
            if let PLpgSQLDatum::Var(v) = d {
                if v.name == name && v.lineno == lineno {
                    return Some(idx);
                }
            }
        }
    }
    datums
        .iter()
        .rposition(|d| matches!(d, PLpgSQLDatum::Var(v) if v.name == name))
}

fn lower_into_target(raw: &JSONValue, datums: &[PLpgSQLDatum]) -> Result<IntoTarget> {
    ensure_single_tag(raw, "INTO target")?;
    if let Some(rec) = raw.get("PLpgSQL_rec") {
        // Zero-valued dno fields are omitted from the JSON dump.
        let index = json_usize_or_zero(rec, "dno")?;
        validate_record_datum(datums, index, "INTO record target")?;
        return Ok(IntoTarget::Rec(index));
    }
    if let Some(row) = raw.get("PLpgSQL_row") {
        let fields = lower_row_fields(row)?;
        for field in &fields {
            validate_assignable_datum(datums, field.varno, "INTO row target")?;
        }
        if fields.is_empty() {
            return Err(SQLError::Internal(
                "PL/pgSQL INTO row target has no fields".into(),
            ));
        }
        return Ok(IntoTarget::Row(fields));
    }
    Err(SQLError::Unsupported(format!(
        "PL/pgSQL INTO target {}",
        json_kind(raw)
    )))
}

fn lower_expr_list(raw: Option<&JSONValue>) -> Result<Vec<Expr>> {
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    let list = raw
        .as_array()
        .ok_or_else(|| SQLError::Internal("PL/pgSQL expression list is not an array".into()))?;
    let mut out = Vec::with_capacity(list.len());
    for item in list {
        out.push(lower_expr(item)?);
    }
    Ok(out)
}

/// Lower a `PLpgSQL_expr` node whose text is a scalar expression
/// (parse modes 2 = expression, 3/4/5 = assignment source).
fn lower_expr(raw: &JSONValue) -> Result<Expr> {
    let (query, mode) = expr_text(raw)?;
    let text = match mode {
        3..=5 => strip_assignment_target(&query, (mode - 2) as usize)?,
        2 => query,
        other => {
            return Err(SQLError::Internal(format!(
                "PL/pgSQL scalar expression has invalid parse mode {other}"
            )));
        }
    };
    compile_expression_text(&text)
}

/// Lower a `PLpgSQL_expr` node holding a complete SQL statement
/// (parse mode 0: queries, PERFORM bodies, CALL statements).
fn lower_full_statement(raw: &JSONValue) -> Result<Statement> {
    let (query, mode) = expr_text(raw)?;
    if mode != 0 {
        return Err(SQLError::Internal(format!(
            "embedded PL/pgSQL statement has invalid parse mode {mode}"
        )));
    }
    let mut stmts = crate::compile(&query)?;
    match stmts.len() {
        1 => Ok(stmts.remove(0)),
        n => Err(SQLError::Internal(format!(
            "embedded PL/pgSQL query compiled to {n} statements"
        ))),
    }
}

fn expr_text(raw: &JSONValue) -> Result<(String, i64)> {
    let expr = expect_tag(raw, "PLpgSQL_expr", "expression")?;
    let query = require_nonempty_str(expr, "query", "PLpgSQL expression")?;
    // RAW_PARSE_DEFAULT is encoded as zero and therefore omitted by
    // libpg_query's JSON serializer.
    let mode = json_i64_or_zero(expr, "parseMode")?;
    Ok((query, mode))
}

/// Compile a bare expression by wrapping it into `SELECT <expr>`.
pub fn compile_expression_text(text: &str) -> Result<Expr> {
    let stmts = crate::compile(&format!("SELECT {text}"))?;
    let mut stmts = stmts;
    let stmt = match stmts.len() {
        1 => stmts.remove(0),
        n => {
            return Err(SQLError::Parse(format!(
                "expression compiled to {n} statements: {text}"
            )));
        }
    };
    let Statement::Select(select) = stmt else {
        return Err(SQLError::Parse(format!("not an expression: {text}")));
    };
    let mut select = *select;
    if select.projections.len() != 1 || select.from.is_some() {
        return Err(SQLError::Parse(format!("not a single expression: {text}")));
    }
    Ok(select.projections.remove(0).expr)
}

/// Strip the leading `name[.name[.name]] :=` (or `=`) target of an
/// assignment-mode expression, returning the source text.
fn strip_assignment_target(text: &str, name_parts: usize) -> Result<String> {
    let bytes = text.as_bytes();
    let mut pos = 0usize;
    let skip_ws = |pos: &mut usize| {
        while *pos < bytes.len() && bytes[*pos].is_ascii_whitespace() {
            *pos += 1;
        }
    };
    for part in 0..name_parts {
        skip_ws(&mut pos);
        if pos < bytes.len() && bytes[pos] == b'"' {
            pos += 1;
            loop {
                if pos >= bytes.len() {
                    return Err(SQLError::Parse(format!(
                        "unterminated quoted identifier in assignment: {text}"
                    )));
                }
                if bytes[pos] == b'"' {
                    if pos + 1 < bytes.len() && bytes[pos + 1] == b'"' {
                        pos += 2;
                        continue;
                    }
                    pos += 1;
                    break;
                }
                pos += 1;
            }
        } else {
            let start = pos;
            while pos < bytes.len()
                && (bytes[pos].is_ascii_alphanumeric()
                    || bytes[pos] == b'_'
                    || bytes[pos] == b'$'
                    || bytes[pos] >= 0x80)
            {
                pos += 1;
            }
            if pos == start {
                return Err(SQLError::Parse(format!(
                    "malformed assignment target: {text}"
                )));
            }
        }
        if part + 1 < name_parts {
            skip_ws(&mut pos);
            if pos >= bytes.len() || bytes[pos] != b'.' {
                return Err(SQLError::Parse(format!(
                    "malformed assignment target: {text}"
                )));
            }
            pos += 1;
        }
    }
    skip_ws(&mut pos);
    if pos < bytes.len() && bytes[pos] == b'[' {
        return Err(SQLError::Unsupported(
            "assignment to an array element".into(),
        ));
    }
    if pos + 1 < bytes.len() && bytes[pos] == b':' && bytes[pos + 1] == b'=' {
        pos += 2;
    } else if pos < bytes.len() && bytes[pos] == b'=' {
        pos += 1;
    } else {
        return Err(SQLError::Parse(format!(
            "assignment operator not found: {text}"
        )));
    }
    Ok(text[pos..].to_string())
}

fn normalize_plpgsql_type(raw: &str) -> String {
    let mut t = raw.trim().to_ascii_lowercase();
    if let Some(rest) = t.strip_prefix("pg_catalog.") {
        t = rest.to_string();
    }
    t.replace('"', "")
}

fn require<'a>(obj: &'a JSONValue, key: &str) -> Result<&'a JSONValue> {
    obj.get(key)
        .ok_or_else(|| SQLError::Internal(format!("PL/pgSQL node missing `{key}`")))
}

fn ensure_single_tag(raw: &JSONValue, context: &str) -> Result<()> {
    let object = raw
        .as_object()
        .ok_or_else(|| SQLError::Internal(format!("PL/pgSQL {context} node is not an object")))?;
    if object.len() != 1 {
        return Err(SQLError::Internal(format!(
            "PL/pgSQL {context} node must contain exactly one tag, found {}",
            object.len()
        )));
    }
    Ok(())
}

fn expect_tag<'a>(raw: &'a JSONValue, tag: &str, context: &str) -> Result<&'a JSONValue> {
    ensure_single_tag(raw, context)?;
    raw.get(tag).ok_or_else(|| {
        SQLError::Internal(format!(
            "PL/pgSQL {context} expected `{tag}`, got `{}`",
            json_kind(raw)
        ))
    })
}

fn require_nonempty_str(obj: &JSONValue, key: &str, context: &str) -> Result<String> {
    match obj.get(key) {
        Some(JSONValue::String(value)) if !value.is_empty() => Ok(value.clone()),
        Some(JSONValue::String(_)) => Err(SQLError::Internal(format!(
            "PL/pgSQL {context} has an empty `{key}`"
        ))),
        Some(other) => Err(SQLError::Internal(format!(
            "PL/pgSQL {context} `{key}` must be a string, got {other}"
        ))),
        None => Err(SQLError::Internal(format!(
            "PL/pgSQL {context} is missing `{key}`"
        ))),
    }
}

fn json_optional_str(obj: &JSONValue, key: &str) -> Result<Option<String>> {
    match obj.get(key) {
        Some(JSONValue::String(value)) => Ok(Some(value.clone())),
        Some(other) => Err(SQLError::Internal(format!(
            "PL/pgSQL `{key}` must be a string, got {other}"
        ))),
        None => Ok(None),
    }
}

fn json_bool_or_false(obj: &JSONValue, key: &str) -> Result<bool> {
    match obj.get(key) {
        Some(JSONValue::Bool(value)) => Ok(*value),
        Some(other) => Err(SQLError::Internal(format!(
            "PL/pgSQL `{key}` must be a boolean, got {other}"
        ))),
        None => Ok(false),
    }
}

fn require_i64(obj: &JSONValue, key: &str, context: &str) -> Result<i64> {
    match obj.get(key) {
        Some(value) => value.as_i64().ok_or_else(|| {
            SQLError::Internal(format!(
                "PL/pgSQL {context} `{key}` must be a signed integer, got {value}"
            ))
        }),
        None => Err(SQLError::Internal(format!(
            "PL/pgSQL {context} is missing `{key}`"
        ))),
    }
}

fn json_optional_i64(obj: &JSONValue, key: &str) -> Result<Option<i64>> {
    match obj.get(key) {
        Some(value) => value.as_i64().map(Some).ok_or_else(|| {
            SQLError::Internal(format!(
                "PL/pgSQL `{key}` must be a signed integer, got {value}"
            ))
        }),
        None => Ok(None),
    }
}

fn json_i64_or_zero(obj: &JSONValue, key: &str) -> Result<i64> {
    Ok(json_optional_i64(obj, key)?.unwrap_or(0))
}

fn json_optional_usize(obj: &JSONValue, key: &str) -> Result<Option<usize>> {
    match obj.get(key) {
        Some(value) => {
            let raw = value.as_u64().ok_or_else(|| {
                SQLError::Internal(format!(
                    "PL/pgSQL `{key}` must be a non-negative integer, got {value}"
                ))
            })?;
            usize::try_from(raw).map(Some).map_err(|_| {
                SQLError::Internal(format!(
                    "PL/pgSQL `{key}` value {raw} does not fit this platform"
                ))
            })
        }
        None => Ok(None),
    }
}

fn json_usize_or_zero(obj: &JSONValue, key: &str) -> Result<usize> {
    Ok(json_optional_usize(obj, key)?.unwrap_or(0))
}

fn optional_array<'a>(obj: &'a JSONValue, key: &str) -> Result<Option<&'a [JSONValue]>> {
    match obj.get(key) {
        Some(JSONValue::Array(values)) => Ok(Some(values)),
        Some(other) => Err(SQLError::Internal(format!(
            "PL/pgSQL `{key}` must be an array, got {other}"
        ))),
        None => Ok(None),
    }
}

fn validate_datum<'a>(
    datums: &'a [PLpgSQLDatum],
    index: usize,
    context: &str,
) -> Result<&'a PLpgSQLDatum> {
    datums.get(index).ok_or_else(|| {
        SQLError::Internal(format!(
            "PL/pgSQL {context} references missing datum {index}"
        ))
    })
}

fn validate_assignable_datum(datums: &[PLpgSQLDatum], index: usize, context: &str) -> Result<()> {
    match validate_datum(datums, index, context)? {
        PLpgSQLDatum::Var(_) | PLpgSQLDatum::Rec { .. } | PLpgSQLDatum::RecField { .. } => Ok(()),
        PLpgSQLDatum::Row { .. } => Err(SQLError::Internal(format!(
            "PL/pgSQL {context} datum {index} is a row container, not an assignable value"
        ))),
    }
}

fn validate_scalar_datum(datums: &[PLpgSQLDatum], index: usize, context: &str) -> Result<()> {
    match validate_datum(datums, index, context)? {
        PLpgSQLDatum::Var(_) | PLpgSQLDatum::RecField { .. } => Ok(()),
        PLpgSQLDatum::Rec { .. } | PLpgSQLDatum::Row { .. } => Err(SQLError::Internal(format!(
            "PL/pgSQL {context} datum {index} is not scalar"
        ))),
    }
}

fn validate_record_datum(datums: &[PLpgSQLDatum], index: usize, context: &str) -> Result<()> {
    match validate_datum(datums, index, context)? {
        PLpgSQLDatum::Rec { .. } => Ok(()),
        _ => Err(SQLError::Internal(format!(
            "PL/pgSQL {context} datum {index} is not a record"
        ))),
    }
}

fn json_kind(obj: &JSONValue) -> String {
    obj.as_object()
        .and_then(|m| m.keys().next().cloned())
        .unwrap_or_else(|| "<unknown>".into())
}

// ---------------------------------------------------------------------
// Variable binding
// ---------------------------------------------------------------------

/// Resolves routine variables while a compiled expression / statement
/// is being specialized for one execution.
pub trait VariableResolver {
    /// Current value of an unqualified name. `Ok(None)` leaves the
    /// column reference for the engine to resolve.
    fn resolve_name(&mut self, name: &str) -> Result<Option<Value>>;
    /// Current value of `qualifier.column` (record field access).
    fn resolve_qualified(&mut self, qualifier: &str, column: &str) -> Result<Option<Value>>;
    /// Value of a positional `$n` reference (function arguments).
    fn resolve_param(&mut self, index: usize) -> Result<Option<Value>>;
}

/// Rewrite an expression, substituting resolvable variable references
/// with literals. References the resolver declines stay untouched.
pub fn bind_expr(expr: &Expr, r: &mut dyn VariableResolver) -> Result<Expr> {
    Ok(match expr {
        Expr::Column(name) => match r.resolve_name(name)? {
            Some(value) => Expr::Literal(value),
            None => expr.clone(),
        },
        Expr::QualifiedColumn {
            qualifier, column, ..
        } => match r.resolve_qualified(qualifier, column)? {
            Some(value) => Expr::Literal(value),
            None => expr.clone(),
        },
        Expr::Param(index) => match r.resolve_param(*index)? {
            Some(value) => Expr::Literal(value),
            None => expr.clone(),
        },
        Expr::Literal(_) | Expr::Star => expr.clone(),
        Expr::Func {
            name,
            args,
            distinct,
            order_by,
            filter,
        } => Expr::Func {
            name: name.clone(),
            args: bind_exprs(args, r)?,
            distinct: *distinct,
            order_by: bind_order_by(order_by, r)?,
            filter: match filter {
                Some(f) => Some(Box::new(bind_expr(f, r)?)),
                None => None,
            },
        },
        Expr::Array(items) => Expr::Array(bind_exprs(items, r)?),
        Expr::Binary { op, lhs, rhs } => Expr::Binary {
            op: *op,
            lhs: Box::new(bind_expr(lhs, r)?),
            rhs: Box::new(bind_expr(rhs, r)?),
        },
        Expr::Not(inner) => Expr::Not(Box::new(bind_expr(inner, r)?)),
        Expr::And(items) => Expr::And(bind_exprs(items, r)?),
        Expr::Or(items) => Expr::Or(bind_exprs(items, r)?),
        Expr::IsNull { expr, negated } => Expr::IsNull {
            expr: Box::new(bind_expr(expr, r)?),
            negated: *negated,
        },
        Expr::Between { expr, low, high } => Expr::Between {
            expr: Box::new(bind_expr(expr, r)?),
            low: Box::new(bind_expr(low, r)?),
            high: Box::new(bind_expr(high, r)?),
        },
        Expr::InList {
            expr,
            list,
            negated,
        } => Expr::InList {
            expr: Box::new(bind_expr(expr, r)?),
            list: bind_exprs(list, r)?,
            negated: *negated,
        },
        Expr::WindowCall { name, args, spec } => Expr::WindowCall {
            name: name.clone(),
            args: bind_exprs(args, r)?,
            spec: crate::ast::WindowSpec {
                partition_by: bind_exprs(&spec.partition_by, r)?,
                order_by: bind_order_by(&spec.order_by, r)?,
                frame: spec.frame.clone(),
            },
        },
        Expr::Case {
            base,
            when,
            else_branch,
        } => Expr::Case {
            base: match base {
                Some(b) => Some(Box::new(bind_expr(b, r)?)),
                None => None,
            },
            when: when
                .iter()
                .map(|(c, v)| Ok((bind_expr(c, r)?, bind_expr(v, r)?)))
                .collect::<Result<Vec<_>>>()?,
            else_branch: match else_branch {
                Some(e) => Some(Box::new(bind_expr(e, r)?)),
                None => None,
            },
        },
        Expr::Cast { expr, ty } => Expr::Cast {
            expr: Box::new(bind_expr(expr, r)?),
            ty: ty.clone(),
        },
        Expr::ScalarSubquery(body) => Expr::ScalarSubquery(Box::new(bind_select(body, r)?)),
        Expr::Exists { body, negated } => Expr::Exists {
            body: Box::new(bind_select(body, r)?),
            negated: *negated,
        },
        Expr::InSubquery {
            expr,
            body,
            negated,
        } => Expr::InSubquery {
            expr: Box::new(bind_expr(expr, r)?),
            body: Box::new(bind_select(body, r)?),
            negated: *negated,
        },
    })
}

fn bind_exprs(exprs: &[Expr], r: &mut dyn VariableResolver) -> Result<Vec<Expr>> {
    exprs.iter().map(|e| bind_expr(e, r)).collect()
}

fn bind_opt_expr(expr: Option<&Expr>, r: &mut dyn VariableResolver) -> Result<Option<Expr>> {
    match expr {
        Some(e) => Ok(Some(bind_expr(e, r)?)),
        None => Ok(None),
    }
}

fn bind_order_by(
    items: &[crate::ast::OrderBy],
    r: &mut dyn VariableResolver,
) -> Result<Vec<crate::ast::OrderBy>> {
    items
        .iter()
        .map(|o| {
            Ok(crate::ast::OrderBy {
                expr: bind_expr(&o.expr, r)?,
                descending: o.descending,
                nulls: o.nulls,
            })
        })
        .collect()
}

fn bind_projections(items: &[Projection], r: &mut dyn VariableResolver) -> Result<Vec<Projection>> {
    items
        .iter()
        .map(|p| {
            Ok(Projection {
                expr: bind_expr(&p.expr, r)?,
                alias: p.alias.clone(),
            })
        })
        .collect()
}

fn bind_assignments(
    items: &[(String, Expr)],
    r: &mut dyn VariableResolver,
) -> Result<Vec<(String, Expr)>> {
    items
        .iter()
        .map(|(name, e)| Ok((name.clone(), bind_expr(e, r)?)))
        .collect()
}

fn bind_ctes(items: &[CTE], r: &mut dyn VariableResolver) -> Result<Vec<CTE>> {
    items
        .iter()
        .map(|cte| {
            Ok(CTE {
                name: cte.name.clone(),
                columns: cte.columns.clone(),
                recursive: cte.recursive,
                query: Box::new(bind_select(&cte.query, r)?),
            })
        })
        .collect()
}

fn bind_rows(rows: &[Vec<Expr>], r: &mut dyn VariableResolver) -> Result<Vec<Vec<Expr>>> {
    rows.iter().map(|row| bind_exprs(row, r)).collect()
}

/// Rewrite a `SELECT` body, substituting resolvable variables.
pub fn bind_select(stmt: &SelectStmt, r: &mut dyn VariableResolver) -> Result<SelectStmt> {
    Ok(SelectStmt {
        projections: bind_projections(&stmt.projections, r)?,
        from: match stmt.from.as_ref() {
            Some(f) => Some(bind_from(f, r)?),
            None => None,
        },
        r#where: bind_opt_expr(stmt.r#where.as_ref(), r)?,
        group_by: bind_exprs(&stmt.group_by, r)?,
        grouping_sets: stmt
            .grouping_sets
            .iter()
            .map(|set| bind_exprs(set, r))
            .collect::<Result<Vec<_>>>()?,
        having: bind_opt_expr(stmt.having.as_ref(), r)?,
        order_by: bind_order_by(&stmt.order_by, r)?,
        limit: bind_opt_expr(stmt.limit.as_ref(), r)?,
        offset: bind_opt_expr(stmt.offset.as_ref(), r)?,
        with: bind_ctes(&stmt.with, r)?,
        set_op: match stmt.set_op.as_ref() {
            Some(op) => Some(Box::new(crate::ast::SetOp {
                kind: op.kind,
                all: op.all,
                left: op
                    .left
                    .as_ref()
                    .map(|left| bind_select(left, r).map(Box::new))
                    .transpose()?,
                right: bind_select(&op.right, r)?,
                combined_order_by: bind_order_by(&op.combined_order_by, r)?,
                combined_limit: bind_opt_expr(op.combined_limit.as_ref(), r)?,
                combined_offset: bind_opt_expr(op.combined_offset.as_ref(), r)?,
            })),
            None => None,
        },
        distinct: stmt.distinct,
        distinct_on: bind_exprs(&stmt.distinct_on, r)?,
    })
}

fn bind_from(from: &FromClause, r: &mut dyn VariableResolver) -> Result<FromClause> {
    Ok(match from {
        FromClause::Table { .. } => from.clone(),
        FromClause::Join {
            left,
            right,
            kind,
            on,
            lateral,
        } => FromClause::Join {
            left: Box::new(bind_from(left, r)?),
            right: Box::new(bind_from(right, r)?),
            kind: *kind,
            on: bind_opt_expr(on.as_ref(), r)?,
            lateral: *lateral,
        },
        FromClause::Values {
            rows,
            alias,
            column_aliases,
        } => FromClause::Values {
            rows: bind_rows(rows, r)?,
            alias: alias.clone(),
            column_aliases: column_aliases.clone(),
        },
        FromClause::Function {
            name,
            args,
            alias,
            column_aliases,
            column_types,
        } => FromClause::Function {
            name: name.clone(),
            args: bind_exprs(args, r)?,
            alias: alias.clone(),
            column_aliases: column_aliases.clone(),
            column_types: column_types.clone(),
        },
        FromClause::Subquery {
            body,
            alias,
            column_aliases,
        } => FromClause::Subquery {
            body: Box::new(bind_select(body, r)?),
            alias: alias.clone(),
            column_aliases: column_aliases.clone(),
        },
    })
}

/// Rewrite a full statement, substituting resolvable variables in
/// every expression position. Statements without expression payloads
/// pass through unchanged.
pub fn bind_statement(stmt: &Statement, r: &mut dyn VariableResolver) -> Result<Statement> {
    Ok(match stmt {
        Statement::Select(body) => Statement::Select(Box::new(bind_select(body, r)?)),
        Statement::Insert(insert) => {
            let mut out = insert.clone();
            out.with = bind_ctes(&insert.with, r)?;
            out.rows = bind_rows(&insert.rows, r)?;
            out.select_source = match insert.select_source.as_ref() {
                Some(body) => Some(Box::new(bind_select(body, r)?)),
                None => None,
            };
            out.on_conflict = match insert.on_conflict.as_ref() {
                Some(oc) => Some(crate::ast::OnConflict {
                    conflict_columns: oc.conflict_columns.clone(),
                    action: match &oc.action {
                        crate::ast::OnConflictAction::Nothing => {
                            crate::ast::OnConflictAction::Nothing
                        }
                        crate::ast::OnConflictAction::Update {
                            assignments,
                            r#where,
                        } => crate::ast::OnConflictAction::Update {
                            assignments: bind_assignments(assignments, r)?,
                            r#where: bind_opt_expr(r#where.as_ref(), r)?,
                        },
                    },
                }),
                None => None,
            };
            out.returning = bind_projections(&insert.returning, r)?;
            Statement::Insert(out)
        }
        Statement::Update(update) => {
            let mut out = update.clone();
            out.assignments = bind_assignments(&update.assignments, r)?;
            out.r#where = bind_opt_expr(update.r#where.as_ref(), r)?;
            out.with = bind_ctes(&update.with, r)?;
            out.from = match update.from.as_ref() {
                Some(f) => Some(bind_from(f, r)?),
                None => None,
            };
            out.returning = bind_projections(&update.returning, r)?;
            Statement::Update(out)
        }
        Statement::Delete(delete) => {
            let mut out = delete.clone();
            out.r#where = bind_opt_expr(delete.r#where.as_ref(), r)?;
            out.with = bind_ctes(&delete.with, r)?;
            out.using = match delete.using.as_ref() {
                Some(f) => Some(bind_from(f, r)?),
                None => None,
            };
            out.returning = bind_projections(&delete.returning, r)?;
            Statement::Delete(out)
        }
        Statement::Values { rows } => Statement::Values {
            rows: bind_rows(rows, r)?,
        },
        Statement::CreateTableAs {
            name,
            if_not_exists,
            body,
        } => Statement::CreateTableAs {
            name: name.clone(),
            if_not_exists: *if_not_exists,
            body: Box::new(bind_select(body, r)?),
        },
        Statement::Explain {
            analyze,
            verbose,
            format,
            body,
        } => Statement::Explain {
            analyze: *analyze,
            verbose: *verbose,
            format: format.clone(),
            body: Box::new(bind_statement(body, r)?),
        },
        Statement::Merge(merge) => {
            let mut out = merge.clone();
            out.source = bind_from(&merge.source, r)?;
            out.join_condition = bind_expr(&merge.join_condition, r)?;
            out.when_clauses = merge
                .when_clauses
                .iter()
                .map(|w| bind_merge_when(w, r))
                .collect::<Result<Vec<_>>>()?;
            out.returning = bind_projections(&merge.returning, r)?;
            Statement::Merge(out)
        }
        Statement::Call { name, args } => Statement::Call {
            name: name.clone(),
            args: bind_exprs(args, r)?,
        },
        other => other.clone(),
    })
}

fn bind_merge_when(when: &MergeWhen, r: &mut dyn VariableResolver) -> Result<MergeWhen> {
    Ok(match when {
        MergeWhen::UpdateMatched {
            condition,
            assignments,
        } => MergeWhen::UpdateMatched {
            condition: bind_opt_expr(condition.as_ref(), r)?,
            assignments: bind_assignments(assignments, r)?,
        },
        MergeWhen::DeleteMatched { condition } => MergeWhen::DeleteMatched {
            condition: bind_opt_expr(condition.as_ref(), r)?,
        },
        MergeWhen::InsertNotMatched {
            condition,
            columns,
            values,
        } => MergeWhen::InsertNotMatched {
            condition: bind_opt_expr(condition.as_ref(), r)?,
            columns: columns.clone(),
            values: bind_exprs(values, r)?,
        },
        MergeWhen::NothingMatched { condition } => MergeWhen::NothingMatched {
            condition: bind_opt_expr(condition.as_ref(), r)?,
        },
        MergeWhen::NothingNotMatched { condition } => MergeWhen::NothingNotMatched {
            condition: bind_opt_expr(condition.as_ref(), r)?,
        },
    })
}

/// `PostgreSQL` 17.4's PL/pgSQL condition-name table. Duplicate names are
/// intentional: exception handlers match every SQLSTATE assigned to the
/// name, while `RAISE condition_name` uses `PostgreSQL`'s first entry.
const PLPGSQL_CONDITION_SQLSTATES: &[(&str, &str)] = &[
    ("sql_statement_not_yet_complete", "03000"),
    ("connection_exception", "08000"),
    ("connection_does_not_exist", "08003"),
    ("connection_failure", "08006"),
    ("sqlclient_unable_to_establish_sqlconnection", "08001"),
    ("sqlserver_rejected_establishment_of_sqlconnection", "08004"),
    ("transaction_resolution_unknown", "08007"),
    ("protocol_violation", "08P01"),
    ("triggered_action_exception", "09000"),
    ("feature_not_supported", "0A000"),
    ("invalid_transaction_initiation", "0B000"),
    ("locator_exception", "0F000"),
    ("invalid_locator_specification", "0F001"),
    ("invalid_grantor", "0L000"),
    ("invalid_grant_operation", "0LP01"),
    ("invalid_role_specification", "0P000"),
    ("diagnostics_exception", "0Z000"),
    (
        "stacked_diagnostics_accessed_without_active_handler",
        "0Z002",
    ),
    ("case_not_found", "20000"),
    ("cardinality_violation", "21000"),
    ("data_exception", "22000"),
    ("array_subscript_error", "2202E"),
    ("character_not_in_repertoire", "22021"),
    ("datetime_field_overflow", "22008"),
    ("division_by_zero", "22012"),
    ("error_in_assignment", "22005"),
    ("escape_character_conflict", "2200B"),
    ("indicator_overflow", "22022"),
    ("interval_field_overflow", "22015"),
    ("invalid_argument_for_logarithm", "2201E"),
    ("invalid_argument_for_ntile_function", "22014"),
    ("invalid_argument_for_nth_value_function", "22016"),
    ("invalid_argument_for_power_function", "2201F"),
    ("invalid_argument_for_width_bucket_function", "2201G"),
    ("invalid_character_value_for_cast", "22018"),
    ("invalid_datetime_format", "22007"),
    ("invalid_escape_character", "22019"),
    ("invalid_escape_octet", "2200D"),
    ("invalid_escape_sequence", "22025"),
    ("nonstandard_use_of_escape_character", "22P06"),
    ("invalid_indicator_parameter_value", "22010"),
    ("invalid_parameter_value", "22023"),
    ("invalid_preceding_or_following_size", "22013"),
    ("invalid_regular_expression", "2201B"),
    ("invalid_row_count_in_limit_clause", "2201W"),
    ("invalid_row_count_in_result_offset_clause", "2201X"),
    ("invalid_tablesample_argument", "2202H"),
    ("invalid_tablesample_repeat", "2202G"),
    ("invalid_time_zone_displacement_value", "22009"),
    ("invalid_use_of_escape_character", "2200C"),
    ("most_specific_type_mismatch", "2200G"),
    ("null_value_not_allowed", "22004"),
    ("null_value_no_indicator_parameter", "22002"),
    ("numeric_value_out_of_range", "22003"),
    ("sequence_generator_limit_exceeded", "2200H"),
    ("string_data_length_mismatch", "22026"),
    ("string_data_right_truncation", "22001"),
    ("substring_error", "22011"),
    ("trim_error", "22027"),
    ("unterminated_c_string", "22024"),
    ("zero_length_character_string", "2200F"),
    ("floating_point_exception", "22P01"),
    ("invalid_text_representation", "22P02"),
    ("invalid_binary_representation", "22P03"),
    ("bad_copy_file_format", "22P04"),
    ("untranslatable_character", "22P05"),
    ("not_an_xml_document", "2200L"),
    ("invalid_xml_document", "2200M"),
    ("invalid_xml_content", "2200N"),
    ("invalid_xml_comment", "2200S"),
    ("invalid_xml_processing_instruction", "2200T"),
    ("duplicate_json_object_key_value", "22030"),
    ("invalid_argument_for_sql_json_datetime_function", "22031"),
    ("invalid_json_text", "22032"),
    ("invalid_sql_json_subscript", "22033"),
    ("more_than_one_sql_json_item", "22034"),
    ("no_sql_json_item", "22035"),
    ("non_numeric_sql_json_item", "22036"),
    ("non_unique_keys_in_a_json_object", "22037"),
    ("singleton_sql_json_item_required", "22038"),
    ("sql_json_array_not_found", "22039"),
    ("sql_json_member_not_found", "2203A"),
    ("sql_json_number_not_found", "2203B"),
    ("sql_json_object_not_found", "2203C"),
    ("too_many_json_array_elements", "2203D"),
    ("too_many_json_object_members", "2203E"),
    ("sql_json_scalar_required", "2203F"),
    ("sql_json_item_cannot_be_cast_to_target_type", "2203G"),
    ("integrity_constraint_violation", "23000"),
    ("restrict_violation", "23001"),
    ("not_null_violation", "23502"),
    ("foreign_key_violation", "23503"),
    ("unique_violation", "23505"),
    ("check_violation", "23514"),
    ("exclusion_violation", "23P01"),
    ("invalid_cursor_state", "24000"),
    ("invalid_transaction_state", "25000"),
    ("active_sql_transaction", "25001"),
    ("branch_transaction_already_active", "25002"),
    ("held_cursor_requires_same_isolation_level", "25008"),
    ("inappropriate_access_mode_for_branch_transaction", "25003"),
    (
        "inappropriate_isolation_level_for_branch_transaction",
        "25004",
    ),
    ("no_active_sql_transaction_for_branch_transaction", "25005"),
    ("read_only_sql_transaction", "25006"),
    ("schema_and_data_statement_mixing_not_supported", "25007"),
    ("no_active_sql_transaction", "25P01"),
    ("in_failed_sql_transaction", "25P02"),
    ("idle_in_transaction_session_timeout", "25P03"),
    ("transaction_timeout", "25P04"),
    ("invalid_sql_statement_name", "26000"),
    ("triggered_data_change_violation", "27000"),
    ("invalid_authorization_specification", "28000"),
    ("invalid_password", "28P01"),
    ("dependent_privilege_descriptors_still_exist", "2B000"),
    ("dependent_objects_still_exist", "2BP01"),
    ("invalid_transaction_termination", "2D000"),
    ("sql_routine_exception", "2F000"),
    ("function_executed_no_return_statement", "2F005"),
    ("modifying_sql_data_not_permitted", "2F002"),
    ("prohibited_sql_statement_attempted", "2F003"),
    ("reading_sql_data_not_permitted", "2F004"),
    ("invalid_cursor_name", "34000"),
    ("external_routine_exception", "38000"),
    ("containing_sql_not_permitted", "38001"),
    ("modifying_sql_data_not_permitted", "38002"),
    ("prohibited_sql_statement_attempted", "38003"),
    ("reading_sql_data_not_permitted", "38004"),
    ("external_routine_invocation_exception", "39000"),
    ("invalid_sqlstate_returned", "39001"),
    ("null_value_not_allowed", "39004"),
    ("trigger_protocol_violated", "39P01"),
    ("srf_protocol_violated", "39P02"),
    ("event_trigger_protocol_violated", "39P03"),
    ("savepoint_exception", "3B000"),
    ("invalid_savepoint_specification", "3B001"),
    ("invalid_catalog_name", "3D000"),
    ("invalid_schema_name", "3F000"),
    ("transaction_rollback", "40000"),
    ("transaction_integrity_constraint_violation", "40002"),
    ("serialization_failure", "40001"),
    ("statement_completion_unknown", "40003"),
    ("deadlock_detected", "40P01"),
    ("syntax_error_or_access_rule_violation", "42000"),
    ("syntax_error", "42601"),
    ("insufficient_privilege", "42501"),
    ("cannot_coerce", "42846"),
    ("grouping_error", "42803"),
    ("windowing_error", "42P20"),
    ("invalid_recursion", "42P19"),
    ("invalid_foreign_key", "42830"),
    ("invalid_name", "42602"),
    ("name_too_long", "42622"),
    ("reserved_name", "42939"),
    ("datatype_mismatch", "42804"),
    ("indeterminate_datatype", "42P18"),
    ("collation_mismatch", "42P21"),
    ("indeterminate_collation", "42P22"),
    ("wrong_object_type", "42809"),
    ("generated_always", "428C9"),
    ("undefined_column", "42703"),
    ("undefined_function", "42883"),
    ("undefined_table", "42P01"),
    ("undefined_parameter", "42P02"),
    ("undefined_object", "42704"),
    ("duplicate_column", "42701"),
    ("duplicate_cursor", "42P03"),
    ("duplicate_database", "42P04"),
    ("duplicate_function", "42723"),
    ("duplicate_prepared_statement", "42P05"),
    ("duplicate_schema", "42P06"),
    ("duplicate_table", "42P07"),
    ("duplicate_alias", "42712"),
    ("duplicate_object", "42710"),
    ("ambiguous_column", "42702"),
    ("ambiguous_function", "42725"),
    ("ambiguous_parameter", "42P08"),
    ("ambiguous_alias", "42P09"),
    ("invalid_column_reference", "42P10"),
    ("invalid_column_definition", "42611"),
    ("invalid_cursor_definition", "42P11"),
    ("invalid_database_definition", "42P12"),
    ("invalid_function_definition", "42P13"),
    ("invalid_prepared_statement_definition", "42P14"),
    ("invalid_schema_definition", "42P15"),
    ("invalid_table_definition", "42P16"),
    ("invalid_object_definition", "42P17"),
    ("with_check_option_violation", "44000"),
    ("insufficient_resources", "53000"),
    ("disk_full", "53100"),
    ("out_of_memory", "53200"),
    ("too_many_connections", "53300"),
    ("configuration_limit_exceeded", "53400"),
    ("program_limit_exceeded", "54000"),
    ("statement_too_complex", "54001"),
    ("too_many_columns", "54011"),
    ("too_many_arguments", "54023"),
    ("object_not_in_prerequisite_state", "55000"),
    ("object_in_use", "55006"),
    ("cant_change_runtime_param", "55P02"),
    ("lock_not_available", "55P03"),
    ("unsafe_new_enum_value_usage", "55P04"),
    ("operator_intervention", "57000"),
    ("query_canceled", "57014"),
    ("admin_shutdown", "57P01"),
    ("crash_shutdown", "57P02"),
    ("cannot_connect_now", "57P03"),
    ("database_dropped", "57P04"),
    ("idle_session_timeout", "57P05"),
    ("system_error", "58000"),
    ("io_error", "58030"),
    ("undefined_file", "58P01"),
    ("duplicate_file", "58P02"),
    ("config_file_error", "F0000"),
    ("lock_file_exists", "F0001"),
    ("fdw_error", "HV000"),
    ("fdw_column_name_not_found", "HV005"),
    ("fdw_dynamic_parameter_value_needed", "HV002"),
    ("fdw_function_sequence_error", "HV010"),
    ("fdw_inconsistent_descriptor_information", "HV021"),
    ("fdw_invalid_attribute_value", "HV024"),
    ("fdw_invalid_column_name", "HV007"),
    ("fdw_invalid_column_number", "HV008"),
    ("fdw_invalid_data_type", "HV004"),
    ("fdw_invalid_data_type_descriptors", "HV006"),
    ("fdw_invalid_descriptor_field_identifier", "HV091"),
    ("fdw_invalid_handle", "HV00B"),
    ("fdw_invalid_option_index", "HV00C"),
    ("fdw_invalid_option_name", "HV00D"),
    ("fdw_invalid_string_length_or_buffer_length", "HV090"),
    ("fdw_invalid_string_format", "HV00A"),
    ("fdw_invalid_use_of_null_pointer", "HV009"),
    ("fdw_too_many_handles", "HV014"),
    ("fdw_out_of_memory", "HV001"),
    ("fdw_no_schemas", "HV00P"),
    ("fdw_option_name_not_found", "HV00J"),
    ("fdw_reply_handle", "HV00K"),
    ("fdw_schema_not_found", "HV00Q"),
    ("fdw_table_not_found", "HV00R"),
    ("fdw_unable_to_create_execution", "HV00L"),
    ("fdw_unable_to_create_reply", "HV00M"),
    ("fdw_unable_to_establish_connection", "HV00N"),
    ("plpgsql_error", "P0000"),
    ("raise_exception", "P0001"),
    ("no_data_found", "P0002"),
    ("too_many_rows", "P0003"),
    ("assert_failure", "P0004"),
    ("internal_error", "XX000"),
    ("data_corrupted", "XX001"),
    ("index_corrupted", "XX002"),
];

/// Map a PL/pgSQL condition name to the SQLSTATE used by
/// `RAISE condition_name`.
pub fn condition_sqlstate(name: &str) -> Option<&'static str> {
    PLPGSQL_CONDITION_SQLSTATES
        .iter()
        .find(|(condition, _)| *condition == name)
        .map(|(_, state)| *state)
}

/// Return every SQLSTATE matched by a PL/pgSQL exception condition.
pub fn condition_sqlstates(name: &str) -> impl Iterator<Item = &'static str> + '_ {
    PLPGSQL_CONDITION_SQLSTATES
        .iter()
        .filter(move |(condition, _)| *condition == name)
        .map(|(_, state)| *state)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scalar_datum(name: &str) -> PLpgSQLDatum {
        PLpgSQLDatum::Var(PLpgSQLVar {
            name: name.into(),
            type_name: "integer".into(),
            default: None,
            constant: false,
            not_null: false,
            lineno: None,
        })
    }

    fn json_expr(query: &str, mode: i64) -> JSONValue {
        serde_json::json!({
            "PLpgSQL_expr": {
                "query": query,
                "parseMode": mode,
            }
        })
    }

    #[test]
    fn representative_parser_output_lowers_without_silent_defaults() {
        let parsed = parse_plpgsql_text(
            "CREATE FUNCTION audit_shape(x int) RETURNS int AS $$\n\
             DECLARE r record; y int := 1; z int;\n\
             BEGIN\n\
               IF x > 0 THEN y := x; ELSIF x = 0 THEN y := 2; END IF;\n\
               CASE x WHEN 1 THEN y := 3; ELSE y := 4; END CASE;\n\
               FOR y IN 0..2 LOOP CONTINUE WHEN y = 1; END LOOP;\n\
               GET DIAGNOSTICS y = ROW_COUNT;\n\
               SELECT 1, 2 INTO y, z; SELECT 3 AS f INTO r; r.f := 4;\n\
               BEGIN RAISE NOTICE 'x'; EXCEPTION WHEN OTHERS THEN y := 5; END;\n\
               RETURN y;\n\
             END; $$ LANGUAGE plpgsql;",
        )
        .unwrap();

        assert!(parsed.datums.len() >= 9);
        assert_eq!(parsed.action.body.len(), 9);
        assert!(parsed
            .datums
            .iter()
            .any(|datum| matches!(datum, PLpgSQLDatum::RecField { field, .. } if field == "f")));
        assert!(parsed.action.body.iter().any(|stmt| matches!(
            stmt,
            PLpgSQLStmt::GetDiagnostics { items }
                if items == &vec![("ROW_COUNT".to_string(), 3)]
        )));
    }

    #[test]
    fn omitted_zero_datum_references_remain_valid_but_malformed_values_fail() {
        let datums = vec![scalar_datum("target")];
        let assignment = serde_json::json!({
            "PLpgSQL_stmt_assign": { "expr": json_expr("target := 1", 3) }
        });
        assert!(matches!(
            lower_stmt(&assignment, &datums).unwrap(),
            PLpgSQLStmt::Assign { target: 0, .. }
        ));

        let diagnostics = serde_json::json!({
            "PLpgSQL_stmt_getdiag": {
                "diag_items": [{ "PLpgSQL_diag_item": { "kind": "ROW_COUNT" } }]
            }
        });
        assert!(matches!(
            lower_stmt(&diagnostics, &datums).unwrap(),
            PLpgSQLStmt::GetDiagnostics { items } if items == vec![("ROW_COUNT".into(), 0)]
        ));

        for bad in [
            serde_json::json!(-1),
            serde_json::json!("0"),
            serde_json::json!(1.5),
        ] {
            let malformed = serde_json::json!({
                "PLpgSQL_stmt_assign": {
                    "varno": bad,
                    "expr": json_expr("target := 1", 3),
                }
            });
            assert!(matches!(
                lower_stmt(&malformed, &datums),
                Err(SQLError::Internal(_))
            ));
        }
    }

    #[test]
    fn malformed_datum_identity_type_and_cross_references_are_rejected() {
        let missing_name = serde_json::json!({
            "PLpgSQL_var": {
                "datatype": { "PLpgSQL_type": { "typname": "integer" } }
            }
        });
        assert!(
            matches!(lower_datum(&missing_name), Err(SQLError::Internal(message)) if message.contains("refname"))
        );

        let missing_type = serde_json::json!({ "PLpgSQL_var": { "refname": "x" } });
        assert!(
            matches!(lower_datum(&missing_type), Err(SQLError::Internal(message)) if message.contains("datatype"))
        );

        let wrong_parent = vec![
            scalar_datum("not_a_record"),
            PLpgSQLDatum::RecField {
                field: "f".into(),
                parent: 0,
            },
        ];
        assert!(
            matches!(validate_datums(&wrong_parent), Err(SQLError::Internal(message)) if message.contains("not a record"))
        );

        let missing_row_target = vec![PLpgSQLDatum::Row {
            fields: vec![PLpgSQLRowField {
                name: "x".into(),
                varno: 9,
            }],
        }];
        assert!(
            matches!(validate_datums(&missing_row_target), Err(SQLError::Internal(message)) if message.contains("missing datum 9"))
        );
    }

    #[test]
    fn malformed_nested_statement_tags_and_lists_are_never_skipped() {
        let datums = vec![scalar_datum("target")];
        let cases = [
            serde_json::json!({
                "PLpgSQL_stmt_if": {
                    "cond": json_expr("true", 2),
                    "elsif_list": [{ "wrong_elsif_tag": {} }]
                }
            }),
            serde_json::json!({
                "PLpgSQL_stmt_case": {
                    "case_when_list": [{ "wrong_case_tag": {} }]
                }
            }),
            serde_json::json!({
                "PLpgSQL_stmt_getdiag": {
                    "diag_items": [{ "wrong_diagnostic_tag": {} }]
                }
            }),
        ];
        for malformed in cases {
            assert!(matches!(
                lower_stmt(&malformed, &datums),
                Err(SQLError::Internal(_))
            ));
        }

        assert!(matches!(
            lower_stmt_list(&serde_json::json!({ "not": "an array" }), &datums),
            Err(SQLError::Internal(message)) if message.contains("not an array")
        ));

        let malformed_exception = serde_json::json!({
            "body": [],
            "exceptions": {
                "PLpgSQL_exception_block": {
                    "exc_list": [{ "wrong_exception_tag": {} }]
                }
            }
        });
        assert!(matches!(
            lower_block(&malformed_exception, &datums),
            Err(SQLError::Internal(message)) if message.contains("exception arm")
        ));

        let unknown_exception_condition = serde_json::json!({
            "body": [],
            "exceptions": {
                "PLpgSQL_exception_block": {
                    "exc_list": [{
                        "PLpgSQL_exception": {
                            "conditions": [{
                                "PLpgSQL_condition": { "condname": "not_a_condition" }
                            }],
                            "action": []
                        }
                    }]
                }
            }
        });
        assert!(matches!(
            lower_block(&unknown_exception_condition, &datums),
            Err(SQLError::Internal(message)) if message.contains("not_a_condition")
        ));

        let unknown_raise_condition = serde_json::json!({
            "PLpgSQL_stmt_raise": {
                "elog_level": 21,
                "condname": "not_a_condition"
            }
        });
        assert!(matches!(
            lower_stmt(&unknown_raise_condition, &datums),
            Err(SQLError::Internal(message)) if message.contains("not_a_condition")
        ));
    }

    #[test]
    fn postgres_condition_table_preserves_full_and_duplicate_mappings() {
        assert_eq!(condition_sqlstate("serialization_failure"), Some("40001"));
        assert_eq!(condition_sqlstate("disk_full"), Some("53100"));
        assert_eq!(
            condition_sqlstates("modifying_sql_data_not_permitted").collect::<Vec<_>>(),
            vec!["2F002", "38002"]
        );
        assert_eq!(condition_sqlstate("not_a_condition"), None);
    }

    #[test]
    fn malformed_into_diagnostics_and_expression_modes_fail_at_lowering() {
        let datums = vec![scalar_datum("target")];
        let missing_into_target = serde_json::json!({
            "PLpgSQL_stmt_execsql": {
                "into": true,
                "sqlstmt": json_expr("SELECT 1", 0),
            }
        });
        assert!(matches!(
            lower_stmt(&missing_into_target, &datums),
            Err(SQLError::Internal(message)) if message.contains("INTO but no target")
        ));

        let missing_kind = serde_json::json!({
            "PLpgSQL_stmt_getdiag": {
                "diag_items": [{ "PLpgSQL_diag_item": {} }]
            }
        });
        assert!(matches!(
            lower_stmt(&missing_kind, &datums),
            Err(SQLError::Internal(message)) if message.contains("kind")
        ));

        assert!(matches!(
            lower_expr(&json_expr("1", 0)),
            Err(SQLError::Internal(message)) if message.contains("parse mode 0")
        ));
        assert!(matches!(
            lower_full_statement(&json_expr("SELECT 1", 2)),
            Err(SQLError::Internal(message)) if message.contains("parse mode 2")
        ));
    }

    #[test]
    fn strip_assignment_single_name() {
        assert_eq!(
            strip_assignment_target("total := a + b", 1).unwrap().trim(),
            "a + b"
        );
        assert_eq!(strip_assignment_target("x = 1", 1).unwrap().trim(), "1");
    }

    #[test]
    fn strip_assignment_quoted_and_dotted() {
        assert_eq!(
            strip_assignment_target("\"my var\" := 7", 1)
                .unwrap()
                .trim(),
            "7"
        );
        assert_eq!(
            strip_assignment_target("rec.fld := rec.fld + 1", 2)
                .unwrap()
                .trim(),
            "rec.fld + 1"
        );
    }

    #[test]
    fn array_element_assignment_is_unsupported() {
        assert!(matches!(
            strip_assignment_target("arr[1] := 2", 1),
            Err(SQLError::Unsupported(_))
        ));
    }
}
