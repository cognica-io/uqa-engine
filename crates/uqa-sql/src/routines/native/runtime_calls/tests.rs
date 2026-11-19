//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::plan::UnifiedPlan;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::MemoryBudget,
};

fn calls(
    sql: &str,
    retained: bool,
    steps: usize,
) -> Result<(Vec<(String, StoredFunctionSite)>, usize), SQLError> {
    let crate::Statement::CreateFunction(definition) = crate::compile(sql)?.remove(0) else {
        panic!("routine")
    };
    let compiled = match &definition.body {
        FunctionBody::Statements(statements) if retained => Some(CompiledFunctionBody::SQL(
            statements.iter().cloned().map(UnifiedPlan::lower).collect(),
        )),
        _ => None,
    };
    let memory = MemoryBudget::new(8 << 20);
    let token = CancellationToken::new();
    let original = token.clone();
    let mut decoder = ImageDecoder::new(&memory, &token, move |mut reader| async move {
        let mut selected = Vec::new();
        visit_stored_routine_runtime_calls(
            &definition,
            compiled.as_ref(),
            &original,
            &mut reader.input(),
            &mut |name, site| {
                selected.push((name.to_owned(), site));
                Ok(())
            },
        )
        .await?;
        Ok::<_, SQLError>(selected)
    })?;
    let mut polls = 0;
    let result = loop {
        polls += 1;
        if let Some(selected) =
            decoder.advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps })?
        {
            break selected;
        }
    };
    drop(decoder);
    assert_eq!(memory.used(), 0);
    Ok((result, polls))
}

#[test]
fn stored_routine_defaults_and_bound_bodies_keep_their_call_placements_while_source_remains_deferred(
) {
    use StoredFunctionSite::{Expression, From, ScalarOnly, Window};
    let sql = "CREATE FUNCTION saved(v int DEFAULT supplied(1)::int) RETURNS int LANGUAGE SQL BEGIN ATOMIC SELECT window_input() OVER () FROM table_input(1); SELECT expression_input(v)::int; END";
    let eager = vec![
        ("supplied".into(), ScalarOnly),
        ("window_input".into(), Window),
        ("table_input".into(), From),
        ("expression_input".into(), Expression),
    ];
    for retained in [false, true] {
        let (selected, single) = calls(sql, retained, 1).unwrap();
        let (coarse, multiple) = calls(sql, retained, 16).unwrap();
        assert_eq!(selected, eager);
        assert_eq!(coarse, eager);
        assert!(single > multiple);
        let (lazy, _) = calls("CREATE FUNCTION saved(v int DEFAULT supplied(1)::int) RETURNS int LANGUAGE SQL AS $$ SELECT hidden() $$", retained, 1).unwrap();
        assert_eq!(lazy, [("supplied".into(), ScalarOnly)]);
    }
    assert!(calls(
        "CREATE FUNCTION saved() RETURNS int LANGUAGE SQL BEGIN ATOMIC SHOW work_mem; END",
        false,
        1
    )
    .unwrap_err()
    .to_string()
    .contains("not yet supported in unquoted SQL function body"));
}
