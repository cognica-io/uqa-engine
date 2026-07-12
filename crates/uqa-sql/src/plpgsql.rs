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
    let function = functions
        .first()
        .and_then(|f| f.get("PLpgSQL_function"))
        .ok_or_else(|| SQLError::Internal("PL/pgSQL parse returned no function".into()))?;
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
    let found_datum = datums
        .iter()
        .position(|d| matches!(d, PLpgSQLDatum::Var(v) if v.name.eq_ignore_ascii_case("found")));
    let action = function
        .get("action")
        .and_then(|a| a.get("PLpgSQL_stmt_block"))
        .ok_or_else(|| SQLError::Internal("PL/pgSQL function without a body block".into()))?;
    let action = lower_block(action, &datums)?;
    Ok(PLpgSQLFunction {
        datums,
        action,
        found_datum,
    })
}

fn lower_datum(raw: &JSONValue) -> Result<PLpgSQLDatum> {
    if let Some(var) = raw.get("PLpgSQL_var") {
        let name = json_str(var, "refname").unwrap_or_default();
        let type_name = var
            .get("datatype")
            .and_then(|d| d.get("PLpgSQL_type"))
            .and_then(|t| json_str(t, "typname"))
            .map(|t| normalize_plpgsql_type(&t))
            .unwrap_or_default();
        let default = match var.get("default_val") {
            Some(node) => Some(lower_expr(node)?),
            None => None,
        };
        return Ok(PLpgSQLDatum::Var(PLpgSQLVar {
            name,
            type_name,
            default,
            constant: json_bool(var, "isconst"),
            not_null: json_bool(var, "notnull"),
            lineno: var.get("lineno").and_then(JSONValue::as_i64),
        }));
    }
    if let Some(rec) = raw.get("PLpgSQL_rec") {
        return Ok(PLpgSQLDatum::Rec {
            name: json_str(rec, "refname").unwrap_or_default(),
        });
    }
    if let Some(field) = raw.get("PLpgSQL_recfield") {
        return Ok(PLpgSQLDatum::RecField {
            field: json_str(field, "fieldname").unwrap_or_default(),
            parent: json_usize(field, "recparentno").unwrap_or_default(),
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
    if let Some(fields) = row.get("fields").and_then(JSONValue::as_array) {
        for f in fields {
            // libpg_query's JSON dump omits zero-valued fields, so a
            // missing varno means datum 0.
            out.push(PLpgSQLRowField {
                name: json_str(f, "name").unwrap_or_default(),
                varno: json_usize(f, "varno").unwrap_or(0),
            });
        }
    }
    Ok(out)
}

fn lower_block(block: &JSONValue, datums: &[PLpgSQLDatum]) -> Result<PLpgSQLBlock> {
    let body = match block.get("body") {
        Some(body) => lower_stmt_list(body, datums)?,
        None => Vec::new(),
    };
    let mut exceptions = Vec::new();
    if let Some(exc) = block
        .get("exceptions")
        .and_then(|e| e.get("PLpgSQL_exception_block"))
    {
        if let Some(list) = exc.get("exc_list").and_then(JSONValue::as_array) {
            for arm in list {
                let Some(arm) = arm.get("PLpgSQL_exception") else {
                    continue;
                };
                let mut conditions = Vec::new();
                if let Some(conds) = arm.get("conditions").and_then(JSONValue::as_array) {
                    for cond in conds {
                        if let Some(cond) = cond.get("PLpgSQL_condition") {
                            if let Some(name) = json_str(cond, "condname") {
                                conditions.push(name.to_ascii_lowercase());
                            } else if let Some(state) = json_str(cond, "sqlstate") {
                                conditions.push(state);
                            }
                        }
                    }
                }
                let body = match arm.get("action") {
                    Some(action) => lower_stmt_list(action, datums)?,
                    None => Vec::new(),
                };
                exceptions.push(PLpgSQLExceptionArm { conditions, body });
            }
        }
    }
    Ok(PLpgSQLBlock {
        label: json_str(block, "label"),
        body,
        exceptions,
    })
}

fn lower_stmt_list(list: &JSONValue, datums: &[PLpgSQLDatum]) -> Result<Vec<PLpgSQLStmt>> {
    let Some(items) = list.as_array() else {
        return Ok(Vec::new());
    };
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        out.push(lower_stmt(item, datums)?);
    }
    Ok(out)
}

fn lower_stmt(raw: &JSONValue, datums: &[PLpgSQLDatum]) -> Result<PLpgSQLStmt> {
    if let Some(block) = raw.get("PLpgSQL_stmt_block") {
        return Ok(PLpgSQLStmt::Block(lower_block(block, datums)?));
    }
    if let Some(stmt) = raw.get("PLpgSQL_stmt_assign") {
        // Zero-valued varno fields are omitted from the JSON dump.
        let target = json_usize(stmt, "varno").unwrap_or(0);
        let expr =
            lower_expr(stmt.get("expr").ok_or_else(|| {
                SQLError::Internal("PL/pgSQL assignment without expression".into())
            })?)?;
        return Ok(PLpgSQLStmt::Assign { target, expr });
    }
    if let Some(stmt) = raw.get("PLpgSQL_stmt_if") {
        let cond = lower_expr(require(stmt, "cond")?)?;
        let then_body = match stmt.get("then_body") {
            Some(body) => lower_stmt_list(body, datums)?,
            None => Vec::new(),
        };
        let mut elsifs = Vec::new();
        if let Some(list) = stmt.get("elsif_list").and_then(JSONValue::as_array) {
            for e in list {
                let Some(e) = e.get("PLpgSQL_if_elsif") else {
                    continue;
                };
                let cond = lower_expr(require(e, "cond")?)?;
                let body = match e.get("stmts") {
                    Some(body) => lower_stmt_list(body, datums)?,
                    None => Vec::new(),
                };
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
        let t_varno = json_usize(stmt, "t_varno");
        let mut arms = Vec::new();
        if let Some(list) = stmt.get("case_when_list").and_then(JSONValue::as_array) {
            for arm in list {
                let Some(arm) = arm.get("PLpgSQL_case_when") else {
                    continue;
                };
                let cond = lower_expr(require(arm, "expr")?)?;
                let body = match arm.get("stmts") {
                    Some(body) => lower_stmt_list(body, datums)?,
                    None => Vec::new(),
                };
                arms.push((cond, body));
            }
        }
        let else_body = if json_bool(stmt, "have_else") {
            Some(match stmt.get("else_stmts") {
                Some(body) => lower_stmt_list(body, datums)?,
                None => Vec::new(),
            })
        } else {
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
            label: json_str(stmt, "label"),
            body: lower_stmt_list(require(stmt, "body")?, datums)?,
        });
    }
    if let Some(stmt) = raw.get("PLpgSQL_stmt_while") {
        return Ok(PLpgSQLStmt::While {
            label: json_str(stmt, "label"),
            cond: lower_expr(require(stmt, "cond")?)?,
            body: lower_stmt_list(require(stmt, "body")?, datums)?,
        });
    }
    if let Some(stmt) = raw.get("PLpgSQL_stmt_fori") {
        let var_obj = require(stmt, "var")?
            .get("PLpgSQL_var")
            .ok_or_else(|| SQLError::Internal("FOR loop variable is not a var".into()))?;
        let name = json_str(var_obj, "refname").unwrap_or_default();
        let lineno = var_obj.get("lineno").and_then(JSONValue::as_i64);
        let var = find_var_datum(datums, &name, lineno).ok_or_else(|| {
            SQLError::Internal(format!("FOR loop variable `{name}` has no datum"))
        })?;
        let step = match stmt.get("step") {
            Some(node) => Some(lower_expr(node)?),
            None => None,
        };
        return Ok(PLpgSQLStmt::ForI {
            label: json_str(stmt, "label"),
            var,
            lower: lower_expr(require(stmt, "lower")?)?,
            upper: lower_expr(require(stmt, "upper")?)?,
            step,
            reverse: json_bool(stmt, "reverse"),
            body: lower_stmt_list(require(stmt, "body")?, datums)?,
        });
    }
    if let Some(stmt) = raw.get("PLpgSQL_stmt_fors") {
        let target = lower_into_target(require(stmt, "var")?)?;
        let query = lower_full_statement(require(stmt, "query")?)?;
        return Ok(PLpgSQLStmt::ForQuery {
            label: json_str(stmt, "label"),
            target,
            query,
            body: lower_stmt_list(require(stmt, "body")?, datums)?,
        });
    }
    if let Some(stmt) = raw.get("PLpgSQL_stmt_exit") {
        let cond = match stmt.get("cond") {
            Some(node) => Some(lower_expr(node)?),
            None => None,
        };
        return Ok(PLpgSQLStmt::Exit {
            is_exit: json_bool(stmt, "is_exit"),
            label: json_str(stmt, "label"),
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
        let level = match stmt.get("elog_level").and_then(JSONValue::as_i64) {
            Some(l) if l <= 14 => RaiseLevel::Debug,
            Some(15 | 16) => RaiseLevel::Log,
            Some(17) => RaiseLevel::Info,
            Some(18) => RaiseLevel::Notice,
            Some(19 | 20) => RaiseLevel::Warning,
            _ => RaiseLevel::Error,
        };
        return Ok(PLpgSQLStmt::Raise {
            level,
            condition: json_str(stmt, "condname").map(|c| c.to_ascii_lowercase()),
            message: json_str(stmt, "message"),
            params: lower_expr_list(stmt.get("params"))?,
        });
    }
    if let Some(stmt) = raw.get("PLpgSQL_stmt_execsql") {
        let sql = lower_full_statement(require(stmt, "sqlstmt")?)?;
        let into = if json_bool(stmt, "into") {
            match stmt.get("target") {
                Some(target) => Some(lower_into_target(target)?),
                None => None,
            }
        } else {
            None
        };
        return Ok(PLpgSQLStmt::ExecSQL {
            stmt: sql,
            into,
            strict: json_bool(stmt, "strict"),
        });
    }
    if let Some(stmt) = raw.get("PLpgSQL_stmt_dynexecute") {
        let query = lower_expr(require(stmt, "query")?)?;
        let into = if json_bool(stmt, "into") {
            match stmt.get("target") {
                Some(target) => Some(lower_into_target(target)?),
                None => None,
            }
        } else {
            None
        };
        return Ok(PLpgSQLStmt::DynExecute {
            query,
            params: lower_expr_list(stmt.get("params"))?,
            into,
            strict: json_bool(stmt, "strict"),
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
            Some(target) => Some(lower_into_target(target)?),
            None => None,
        };
        return Ok(PLpgSQLStmt::ExecSQL {
            stmt: call,
            into,
            strict: false,
        });
    }
    if let Some(stmt) = raw.get("PLpgSQL_stmt_getdiag") {
        if json_bool(stmt, "is_stacked") {
            return Err(SQLError::Unsupported("GET STACKED DIAGNOSTICS".into()));
        }
        let mut items = Vec::new();
        if let Some(list) = stmt.get("diag_items").and_then(JSONValue::as_array) {
            for item in list {
                let Some(item) = item.get("PLpgSQL_diag_item") else {
                    continue;
                };
                let kind = json_str(item, "kind")
                    .unwrap_or_default()
                    .to_ascii_uppercase();
                let target = json_usize(item, "target").unwrap_or(0);
                items.push((kind, target));
            }
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

fn lower_into_target(raw: &JSONValue) -> Result<IntoTarget> {
    if let Some(rec) = raw.get("PLpgSQL_rec") {
        // Zero-valued dno fields are omitted from the JSON dump.
        return Ok(IntoTarget::Rec(json_usize(rec, "dno").unwrap_or(0)));
    }
    if let Some(row) = raw.get("PLpgSQL_row") {
        return Ok(IntoTarget::Row(lower_row_fields(row)?));
    }
    Err(SQLError::Unsupported(format!(
        "PL/pgSQL INTO target {}",
        json_kind(raw)
    )))
}

fn lower_expr_list(raw: Option<&JSONValue>) -> Result<Vec<Expr>> {
    let Some(list) = raw.and_then(JSONValue::as_array) else {
        return Ok(Vec::new());
    };
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
        _ => query,
    };
    compile_expression_text(&text)
}

/// Lower a `PLpgSQL_expr` node holding a complete SQL statement
/// (parse mode 0: queries, PERFORM bodies, CALL statements).
fn lower_full_statement(raw: &JSONValue) -> Result<Statement> {
    let (query, _mode) = expr_text(raw)?;
    let mut stmts = crate::compile(&query)?;
    match stmts.len() {
        1 => Ok(stmts.remove(0)),
        n => Err(SQLError::Internal(format!(
            "embedded PL/pgSQL query compiled to {n} statements"
        ))),
    }
}

fn expr_text(raw: &JSONValue) -> Result<(String, i64)> {
    let expr = raw
        .get("PLpgSQL_expr")
        .ok_or_else(|| SQLError::Internal("expected a PLpgSQL_expr node".into()))?;
    let query = json_str(expr, "query")
        .ok_or_else(|| SQLError::Internal("PLpgSQL_expr without query text".into()))?;
    let mode = expr
        .get("parseMode")
        .and_then(JSONValue::as_i64)
        .unwrap_or(0);
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

fn json_str(obj: &JSONValue, key: &str) -> Option<String> {
    obj.get(key)
        .and_then(JSONValue::as_str)
        .map(ToString::to_string)
}

fn json_bool(obj: &JSONValue, key: &str) -> bool {
    obj.get(key).and_then(JSONValue::as_bool).unwrap_or(false)
}

fn json_usize(obj: &JSONValue, key: &str) -> Option<usize> {
    obj.get(key)
        .and_then(JSONValue::as_i64)
        .and_then(|v| usize::try_from(v).ok())
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

/// Map a `PL/pgSQL` condition name to its `SQLSTATE`. Returns `None`
/// for unknown names (the caller then treats the condition text as a
/// literal state code when it looks like one).
///
/// Note: the engine evaluates `x / 0` to NULL instead of raising, so
/// `division_by_zero` is reachable only through an explicit `RAISE`.
pub fn condition_sqlstate(name: &str) -> Option<&'static str> {
    Some(match name {
        "division_by_zero" => "22012",
        "numeric_value_out_of_range" => "22003",
        "invalid_text_representation" => "22P02",
        "null_value_not_allowed" => "22004",
        "unique_violation" => "23505",
        "foreign_key_violation" => "23503",
        "not_null_violation" => "23502",
        "check_violation" => "23514",
        "restrict_violation" => "23001",
        "syntax_error" => "42601",
        "undefined_column" => "42703",
        "undefined_function" => "42883",
        "undefined_table" => "42P01",
        "duplicate_table" => "42P07",
        "datatype_mismatch" => "42804",
        "insufficient_privilege" => "42501",
        "feature_not_supported" => "0A000",
        "query_canceled" => "57014",
        "no_data_found" => "P0002",
        "too_many_rows" => "P0003",
        "raise_exception" => "P0001",
        "assert_failure" => "P0004",
        "plpgsql_error" => "P0000",
        "internal_error" => "XX000",
        "data_exception" => "22000",
        "integrity_constraint_violation" => "23000",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
