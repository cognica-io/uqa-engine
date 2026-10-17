//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog commands bind patterns as data and use the public SQL catalogs.

use super::{Session, Value, Write};

impl Session {
    pub(super) fn catalog_command(
        &mut self,
        command: &str,
        argument: &str,
        out: &mut impl Write,
    ) -> Result<bool, String> {
        let command = command.trim_end_matches('+');
        let command = if command == "describe" { "d" } else { command };
        let Some((query, schema, name)) = catalog_query(command) else {
            return Ok(false);
        };
        let arguments = crate::arguments::parse(argument, &self.shell.variables)?;
        if arguments.len() > 1 {
            return Err("expected one catalog pattern".into());
        }
        let argument = arguments.first().map_or("", String::as_str);
        let mut query = query.to_owned();
        let mut predicates = Vec::new();
        let mut params = Vec::new();
        if !argument.is_empty() {
            let (schema_pattern, name_pattern) = name_pattern(argument)?;
            if let Some(pattern) = schema_pattern {
                if schema.is_empty() {
                    return Err("this catalog has no schema-qualified names".into());
                }
                params.push(uqa_engine::SQLParam::Scalar(Value::Str(pattern)));
                predicates.push(format!("{schema} ~ ${}", params.len()));
            }
            params.push(uqa_engine::SQLParam::Scalar(Value::Str(name_pattern)));
            predicates.push(format!("{name} ~ ${}", params.len()));
        } else if !schema.is_empty() && command != "dS" {
            predicates.push(format!(
                "{schema} NOT IN ('pg_catalog', 'information_schema')"
            ));
        }
        if matches!(command, "dt" | "tables") {
            predicates.push("table_type <> 'VIEW'".into());
        }
        if !predicates.is_empty() {
            query.push_str(if query.contains(" WHERE ") {
                " AND "
            } else {
                " WHERE "
            });
            query.push_str(&predicates.join(" AND "));
        }
        query.push_str(" ORDER BY ");
        if !schema.is_empty() {
            query.push_str(schema);
            query.push_str(", ");
        }
        query.push_str(name);
        let result = self
            .engine
            .sql(&query, &params)
            .map_err(|e| crate::diagnostics::sql_error_text(&e))?;
        if command == "d" && !argument.is_empty() {
            if result.rows.is_empty() {
                return Err(format!("Did not find any relation named \"{argument}\"."));
            }
            for row in 0..result.rows.len() {
                if let (Some(Value::Str(schema)), Some(Value::Str(name))) =
                    (result.value_at(row, 0), result.value_at(row, 1))
                {
                    let qualified =
                        format!("{}.{}", quote_identifier(schema), quote_identifier(name));
                    if result.value_at(row, 2) == Some(&Value::Str("VIEW".into())) {
                        self.describe_view(schema, name, &qualified, out)?;
                    } else {
                        self.cmd_describe_table(&qualified, out)?;
                    }
                }
            }
        } else {
            self.render_result(&result, out)?;
        }
        Ok(true)
    }
    fn describe_view(
        &mut self,
        schema: &str,
        name: &str,
        qualified: &str,
        out: &mut impl Write,
    ) -> Result<(), String> {
        let columns = self
            .engine
            .sql(
                "SELECT column_name, data_type, is_nullable, column_default \
             FROM information_schema.columns \
             WHERE table_schema=$1 AND table_name=$2 ORDER BY ordinal_position",
                &[
                    uqa_engine::SQLParam::Scalar(Value::Str(schema.into())),
                    uqa_engine::SQLParam::Scalar(Value::Str(name.into())),
                ],
            )
            .map_err(|error| crate::diagnostics::sql_error_text(&error))?;
        writeln!(out, "View {}", crate::printing::terminal_text(qualified))
            .map_err(|error| error.to_string())?;
        self.render_result(&columns, out)
    }
}

fn catalog_query(command: &str) -> Option<(&'static str, &'static str, &'static str)> {
    Some(match command {
        "d" | "dt" | "tables" | "dS" => (
            "SELECT table_schema, table_name, table_type FROM information_schema.tables",
            "table_schema", "table_name",
        ),
        "dv" => (
            "SELECT table_schema, table_name FROM information_schema.views",
            "table_schema", "table_name",
        ),
        "di" => (
            "SELECT schemaname, tablename, indexname, indexdef FROM pg_catalog.pg_indexes",
            "schemaname", "indexname",
        ),
        "df" => (
            "SELECT routine_schema, routine_name, routine_type, data_type FROM information_schema.routines",
            "routine_schema", "routine_name",
        ),
        "dn" => ("SELECT schema_name FROM information_schema.schemata", "", "schema_name"),
        "du" | "dg" => (
            "SELECT rolname, rolsuper, rolcreaterole, rolcreatedb, rolcanlogin FROM pg_catalog.pg_roles",
            "", "rolname",
        ),
        "l" | "list" => ("SELECT datname, datallowconn FROM pg_catalog.pg_database", "", "datname"),
        "dx" => ("SELECT extname, extversion FROM pg_catalog.pg_extension", "", "extname"),
        "dp" | "z" => (
            "SELECT n.nspname AS schema, c.relname AS name, c.relacl AS access_privileges \
             FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON c.relnamespace=n.oid \
             WHERE c.relkind IN ('r','v','m','S','f','p')",
            "n.nspname", "c.relname",
        ),
        "da" => (
            "SELECT n.nspname, p.proname FROM pg_catalog.pg_proc p \
             JOIN pg_catalog.pg_namespace n ON p.pronamespace=n.oid WHERE p.prokind='a'",
            "n.nspname", "p.proname",
        ),
        _ => return None,
    })
}

pub(super) fn quote_identifier(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}
pub(super) use uqa_sql::expr::quote_ident as completion_identifier;

fn name_pattern(source: &str) -> Result<(Option<String>, String), String> {
    let mut parts = vec![String::new()];
    let mut quoted = false;
    let mut chars = source.trim().chars().peekable();
    while let Some(c) = chars.next() {
        if c == '"' {
            if quoted && chars.peek() == Some(&'"') {
                chars.next();
                parts.last_mut().unwrap().push('"');
            } else {
                quoted = !quoted;
            }
            continue;
        }
        if !quoted && c == '.' {
            parts.push(String::new());
            continue;
        }
        let part = parts.last_mut().unwrap();
        if quoted {
            if ".^$*+?()[]{}|\\".contains(c) {
                part.push('\\');
            }
            part.push(c);
        } else {
            match c {
                '*' => part.push_str(".*"),
                '?' => part.push('.'),
                _ => part.extend(c.to_lowercase()),
            }
        }
    }
    if quoted || parts.len() > 2 {
        return Err("invalid catalog pattern".into());
    }
    let name = format!("^({})$", parts.pop().unwrap());
    let schema = parts.pop().map(|part| format!("^({part})$"));
    Ok((schema, name))
}
