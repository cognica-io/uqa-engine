//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind text-retrieval validation to relation and index metadata.

use crate::Engine;
use uqa_sql::SQLError;

pub(in crate::sql) fn validate_text_match_field(
    engine: &Engine,
    table: &str,
    field: &str,
    function_name: &str,
) -> Result<(), SQLError> {
    uqa_sql::semantics::text_indexes::validate_text_match_field(engine, table, field, function_name)
}
pub(in crate::sql) fn validate_text_match_all_fields(
    engine: &Engine,
    table: &str,
    function_name: &str,
) -> Result<(), SQLError> {
    uqa_sql::semantics::text_indexes::validate_text_match_all_fields(engine, table, function_name)
}
