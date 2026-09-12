//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Score provenance and highlight rendering for scalar projection rows.

use std::sync::Arc;
use uqa_analysis::{AnalysisError, AnalysisResult, CompiledAnalyzer};
use uqa_core::{
    memory::{BudgetedVec, MemoryBudget},
    Value,
};
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
    run_uqa_highlight_with_runtime(row, args, evaluate, analyzers, None)
}

pub(crate) fn run_uqa_highlight_with_runtime(
    row: &dyn RowLookup,
    args: &[ScalarExpr],
    evaluate: &mut dyn FnMut(&ScalarExpr) -> Result<Value, SQLError>,
    analyzers: Option<&dyn AnalyzerRevisions>,
    runtime: Option<super::runtime::QueryRuntimeView<'_>>,
) -> Result<Value, SQLError> {
    let arguments = match highlight_arguments(row, args, evaluate)? {
        HighlightInput::Value(value) => return Ok(value),
        HighlightInput::Arguments(arguments) => arguments,
    };
    let budget = MemoryBudget::new(match runtime {
        Some(runtime) => runtime.work_mem_bytes()?,
        None => usize::MAX,
    });
    let mut poll = || {
        if let Some(runtime) = runtime {
            runtime
                .cancellation
                .check()
                .map_err(|_| AnalysisError::Cancelled)?;
        }
        Ok(())
    };
    poll().map_err(highlight_error)?;
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
        let out = uqa_analysis::highlight_compiled_budgeted(
            &text,
            &[query_str],
            &revision,
            &opts,
            &budget,
            &mut poll,
        )
        .map_err(highlight_error)?;
        return Ok(Value::Str(out.into_parts().0));
    }
    let terms = query_candidates(&query_str, &budget, &mut poll).map_err(highlight_error)?;
    let analyzer = uqa_analysis::standard_analyzer("english");
    let out = uqa_analysis::highlight::highlight_words_budgeted(
        &text,
        terms.iter().copied(),
        Some(&analyzer),
        &opts,
        &budget,
        poll,
    )
    .map_err(highlight_error)?;
    // The completed string transfers to the scalar result owner after rendering.
    Ok(Value::Str(out.into_parts().0))
}

fn query_candidates<'a>(
    query: &'a str,
    budget: &MemoryBudget,
    poll: &mut dyn FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<BudgetedVec<&'a str>> {
    let mut terms = BudgetedVec::new(budget);
    let mut start = None;
    let mut push = |start, end| {
        let term = &query[start..end];
        if !["and", "or", "not"]
            .iter()
            .any(|word| term.eq_ignore_ascii_case(word))
        {
            terms.push(term)?;
        }
        Ok::<(), AnalysisError>(())
    };
    for (index, (byte, character)) in query.char_indices().enumerate() {
        if index % 1024 == 0 {
            poll()?;
        }
        if character.is_whitespace() {
            if let Some(start) = start.take() {
                push(start, byte)?;
            }
        } else {
            start.get_or_insert(byte);
        }
    }
    if let Some(start) = start {
        push(start, query.len())?;
    }
    poll()?;
    Ok(terms)
}

fn highlight_error(error: AnalysisError) -> SQLError {
    match error {
        AnalysisError::Cancelled => SQLError::Cancelled(uqa_core::QueryCancelled),
        AnalysisError::Memory(error) => SQLError::Routine {
            sqlstate: "53200".into(),
            message: format!("highlight analysis failed: {error}"),
        },
        error => SQLError::Internal(format!("highlight analysis failed: {error}")),
    }
}

#[cfg(test)]
mod tests;
