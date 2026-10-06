//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Preserve absent and explicitly empty server strings across protobuf's scalar-string representation.

use crate::{ast::CreateForeignServer, SQLError};
use pg_query::protobuf::Token;

/// The parser has already validated the grammar. Token positions distinguish omitted TYPE/VERSION from empty literals without interpreting comments, quoted identifiers or string contents as keywords.
pub(super) fn retain_string_presence(
    sql: &str,
    statement: &mut CreateForeignServer,
) -> Result<(), SQLError> {
    let scanned = crate::parser::scan(sql)?;
    let tokens = scanned
        .tokens
        .iter()
        .map(|token| token.token())
        .filter(|token| !matches!(token, Token::SqlComment | Token::CComment))
        .collect::<Vec<_>>();
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

/// Unicode identifiers and string literals may carry an explicit UESCAPE string after the token itself.
fn after_token(tokens: &[Token], position: usize) -> usize {
    let next = position + 1;
    if tokens.get(next) == Some(&Token::Uescape) {
        next + 2
    } else {
        next
    }
}
