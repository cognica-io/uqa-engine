//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL help is separate from client-command help and points to the canonical contract.

use std::io::Write;

const COMMANDS: &[(&str, &str, &str)] = &[
    (
        "SELECT",
        "SELECT [DISTINCT] expression [, ...] [FROM relation] [WHERE condition] [GROUP BY expression] [HAVING condition] [ORDER BY expression] [LIMIT count] [OFFSET count]",
        "03-queries-and-dml.md",
    ),
    (
        "INSERT",
        "INSERT INTO table [(column, ...)] { DEFAULT VALUES | VALUES (expression, ...) [, ...] | query } [ON CONFLICT ...] [RETURNING expression, ...]",
        "03-queries-and-dml.md",
    ),
    (
        "UPDATE",
        "UPDATE table SET column = expression [, ...] [FROM relation] [WHERE condition] [RETURNING expression, ...]",
        "03-queries-and-dml.md",
    ),
    (
        "DELETE",
        "DELETE FROM table [USING relation] [WHERE condition] [RETURNING expression, ...]",
        "03-queries-and-dml.md",
    ),
    (
        "CREATE TABLE",
        "CREATE [TEMPORARY] TABLE [IF NOT EXISTS] table (column type [constraint ...] [, ...])",
        "02-ddl.md",
    ),
    (
        "CREATE INDEX",
        "CREATE [UNIQUE] INDEX name ON table [USING method] (expression [, ...]) [WHERE predicate]",
        "02-ddl.md",
    ),
    (
        "CREATE VIEW",
        "CREATE [OR REPLACE] VIEW name [(column, ...)] AS query",
        "02-ddl.md",
    ),
    (
        "ALTER TABLE",
        "ALTER TABLE [ONLY] table action [, ...]",
        "02-ddl.md",
    ),
    (
        "DROP TABLE",
        "DROP TABLE [IF EXISTS] table [, ...] [CASCADE | RESTRICT]",
        "02-ddl.md",
    ),
    (
        "CREATE FUNCTION",
        "CREATE [OR REPLACE] FUNCTION name ([argument, ...]) RETURNS type LANGUAGE language AS definition",
        "08-transactions-and-routines.md",
    ),
    (
        "BEGIN",
        "BEGIN [WORK | TRANSACTION] [transaction_mode, ...]",
        "08-transactions-and-routines.md",
    ),
    (
        "COMMIT",
        "COMMIT [WORK | TRANSACTION] [AND [NO] CHAIN]",
        "08-transactions-and-routines.md",
    ),
    (
        "ROLLBACK",
        "ROLLBACK [WORK | TRANSACTION] [AND [NO] CHAIN] | ROLLBACK TO [SAVEPOINT] name",
        "08-transactions-and-routines.md",
    ),
    (
        "SAVEPOINT",
        "SAVEPOINT name",
        "08-transactions-and-routines.md",
    ),
    (
        "RELEASE",
        "RELEASE [SAVEPOINT] name",
        "08-transactions-and-routines.md",
    ),
    (
        "SET",
        "SET [SESSION | LOCAL] parameter { TO | = } value",
        "08-transactions-and-routines.md",
    ),
    (
        "SHOW",
        "SHOW parameter | SHOW ALL",
        "08-transactions-and-routines.md",
    ),
    (
        "EXPLAIN",
        "EXPLAIN [(option [, ...])] statement",
        "03-queries-and-dml.md",
    ),
    (
        "ANALYZE",
        "ANALYZE [table [(column, ...)]]",
        "02-ddl.md",
    ),
    (
        "VACUUM",
        "VACUUM [(option [, ...])] [table [(column, ...)]]",
        "02-ddl.md",
    ),
    (
        "COPY",
        "COPY { table [(column, ...)] | (query) } { FROM STDIN | TO STDOUT } [WITH (option [, ...])]\nUse \\copy for client-side file streams.",
        "03-queries-and-dml.md",
    ),
    (
        "LISTEN",
        "LISTEN channel",
        "08-transactions-and-routines.md",
    ),
    (
        "UNLISTEN",
        "UNLISTEN { channel | * }",
        "08-transactions-and-routines.md",
    ),
    (
        "NOTIFY",
        "NOTIFY channel [, 'payload']",
        "08-transactions-and-routines.md",
    ),
];

pub(super) fn print(topic: &str, out: &mut impl Write) -> Result<(), String> {
    let topic = topic.trim().trim_end_matches(';').to_ascii_uppercase();
    let mut found = false;
    for (command, syntax, manual) in COMMANDS {
        if topic.is_empty() {
            writeln!(out, "{command}").map_err(|e| e.to_string())?;
            found = true;
        } else if topic == "*" || command.starts_with(&topic) {
            writeln!(
                out,
                "Command: {command}\nSyntax:\n{syntax}\nManual: https://github.com/cognica-io/uqa-engine/blob/main/docs/manual/sql/{manual}\n",
            ).map_err(|error| error.to_string())?;
            found = true;
        }
    }
    if found {
        Ok(())
    } else {
        Err(format!(
            "No help available for \"{topic}\". Try \\h with no arguments."
        ))
    }
}
