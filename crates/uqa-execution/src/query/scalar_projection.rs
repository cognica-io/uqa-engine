//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Score provenance and highlight rendering for scalar projection rows.

use std::sync::Arc;
use uqa_analysis::CompiledAnalyzer;
use uqa_core::Value;
use uqa_sql::{
    expr::RowLookup,
    semantics::scalar_projection::{highlight_arguments, HighlightArguments, HighlightInput},
    SQLError, ScalarExpr,
};

pub fn score_projection_value(
    function: &str,
    args: &[ScalarExpr],
    row: &dyn RowLookup,
) -> Result<Value, SQLError> {
    let qualifier = (args.len() == 2)
        .then(|| match &args[0] {
            ScalarExpr::QualifiedColumn { qualifier, .. } => Some(qualifier.as_str()),
            _ => None,
        })
        .flatten();
    if row.score_source_is_ambiguous(qualifier) {
        return Err(SQLError::Unsupported(format!(
            "{function}() has multiple score-bearing retrieval rows; qualify its field argument"
        )));
    }
    if let Some(Value::Float(score)) = row.score_source(qualifier) {
        return Ok(Value::Float(*score));
    }
    Err(score_projection_context_error(function))
}

fn score_projection_context_error(function: &str) -> SQLError {
    SQLError::Unsupported(format!(
        "{function}() requires a score-bearing retrieval row"
    ))
}

/// Named resources retained independently of later registry replacement.
pub trait AnalyzerRevisions {
    fn analyzer_revision(&self, name: &str) -> Result<Arc<CompiledAnalyzer>, String>;
}

pub fn run_uqa_highlight(
    row: &dyn RowLookup,
    args: &[ScalarExpr],
    evaluate: &mut dyn FnMut(&ScalarExpr) -> Result<Value, SQLError>,
    analyzers: Option<&dyn AnalyzerRevisions>,
) -> Result<Value, SQLError> {
    let arguments = match highlight_arguments(row, args, evaluate)? {
        HighlightInput::Value(value) => return Ok(value),
        HighlightInput::Arguments(arguments) => arguments,
    };
    let HighlightArguments {
        text,
        query: query_str,
        start_tag,
        end_tag,
        max_fragments,
        fragment_size,
        analyzer,
    } = arguments;
    let opts = uqa_analysis::HighlightOptions {
        start_tag,
        end_tag,
        max_fragments,
        fragment_size,
    };
    if let Some(name) = analyzer {
        let revisions = analyzers.ok_or_else(|| {
            SQLError::Unsupported(
                "uqa_highlight with an analyzer requires named analyzer resources".into(),
            )
        })?;
        let revision = revisions
            .analyzer_revision(&name)
            .map_err(SQLError::Unsupported)?;
        let out = uqa_analysis::highlight_compiled(&text, &[query_str], &revision, &opts)
            .map_err(|error| SQLError::Internal(format!("highlight analysis failed: {error}")))?;
        return Ok(Value::Str(out));
    }
    // Pull every whitespace-separated token from the query string as a
    // candidate match term. A simple split matches the documented highlighting
    // surface and its regression fixtures.
    let terms: Vec<String> = query_str
        .split_whitespace()
        .filter(|t| !matches!(t.to_ascii_lowercase().as_str(), "and" | "or" | "not"))
        .map(std::string::ToString::to_string)
        .collect();
    let analyzer = uqa_analysis::standard_analyzer("english");
    let out = uqa_analysis::highlight::highlight_words(&text, &terms, Some(&analyzer), &opts)
        .map_err(|error| SQLError::Internal(format!("highlight analysis failed: {error}")))?;
    Ok(Value::Str(out))
}

#[cfg(test)]
mod tests;
