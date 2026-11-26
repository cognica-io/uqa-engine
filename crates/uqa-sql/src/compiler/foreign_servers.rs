//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Preserve absent and explicitly empty server strings across protobuf's scalar-string representation.

use crate::{
    ast::{AlterForeignServer, CreateForeignServer, ForeignOptionChange, ForeignServerVersion},
    SQLError,
};
use pg_query::protobuf::{AlterForeignServerStmt, DefElemAction, Token};

pub(super) fn compile_alter(stmt: &AlterForeignServerStmt) -> Result<AlterForeignServer, SQLError> {
    let options = stmt
        .options
        .iter()
        .map(|node| {
            let elem = super::relations::option_element(node)?;
            let name = elem.defname.clone();
            Ok(match elem.defaction() {
                DefElemAction::DefelemDrop => ForeignOptionChange::Drop { name },
                DefElemAction::DefelemSet => ForeignOptionChange::Set {
                    name,
                    value: super::relations::option_value(elem)?,
                },
                DefElemAction::DefelemAdd | DefElemAction::DefelemUnspec => {
                    ForeignOptionChange::Add {
                        name,
                        value: super::relations::option_value(elem)?,
                    }
                }
                DefElemAction::Undefined => {
                    return Err(SQLError::Internal("malformed foreign option action".into()))
                }
            })
        })
        .collect::<Result<_, _>>()?;
    Ok(AlterForeignServer {
        name: stmt.servername.clone(),
        version: if stmt.has_version {
            ForeignServerVersion::Set(stmt.version.clone())
        } else {
            ForeignServerVersion::Unchanged
        },
        options,
    })
}

pub(super) fn retain_alter_version(
    sql: &str,
    statement: &mut AlterForeignServer,
) -> Result<(), SQLError> {
    if matches!(&statement.version, ForeignServerVersion::Set(version) if version.is_empty()) {
        let tokens = declaration_tokens(sql)?;
        let version = after_token(&tokens, 2);
        if tokens.get(version + 1) == Some(&Token::NullP) {
            statement.version = ForeignServerVersion::Clear;
        }
    }
    Ok(())
}

/// The parser has already validated the grammar. Token positions distinguish omitted TYPE/VERSION from empty literals without interpreting comments, quoted identifiers or string contents as keywords.
pub(super) fn retain_string_presence(
    sql: &str,
    statement: &mut CreateForeignServer,
) -> Result<(), SQLError> {
    let tokens = declaration_tokens(sql)?;
    // CREATE SERVER [IF NOT EXISTS] name [TYPE string] [VERSION string|NULL].
    let mut position = after_token(&tokens, if statement.if_not_exists { 5 } else { 2 });
    if tokens.get(position) == Some(&Token::TypeP) {
        statement.server_type.get_or_insert_with(String::new);
        position = after_token(&tokens, position + 1);
    }
    if tokens.get(position) == Some(&Token::VersionP)
        && tokens.get(position + 1) != Some(&Token::NullP)
    {
        statement.version.get_or_insert_with(String::new);
    }
    Ok(())
}

fn declaration_tokens(sql: &str) -> Result<Vec<Token>, SQLError> {
    let scanned = crate::parser::scan(sql)?;
    Ok(scanned
        .tokens
        .iter()
        .map(|token| token.token())
        .filter(|token| !matches!(token, Token::SqlComment | Token::CComment))
        .collect())
}

/// Unicode identifiers and string literals may carry an explicit UESCAPE string after the token itself.
fn after_token(tokens: &[Token], position: usize) -> usize {
    let next = position + 1;
    if tokens.get(next) == Some(&Token::Uescape) {
        next + 2
    } else {
        next
    }
}
