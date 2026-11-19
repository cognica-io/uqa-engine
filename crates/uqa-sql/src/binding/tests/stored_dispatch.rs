//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{column, ColumnType, EmptyRoutineResolution};
use crate::{
    binding::{fixture, stored_routines::StoredCallOrigin, BindingContext},
    plan::UnifiedPlan,
    RelationIdentity, SQLError,
};
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::{MemoryBudget, ProductionControl},
    CancellationToken,
};

fn context() -> BindingContext<'static> {
    BindingContext {
        catalog: fixture::catalog(
            [(
                RelationIdentity::new("public", "items"),
                fixture::table_definition(vec![column("id", ColumnType::Integer)]),
            )]
            .into_iter()
            .collect(),
        ),
        ..fixture::empty_binding_context()
    }
}

fn bind(sql: &str, steps: usize, cancel: Option<usize>) -> (Result<(), SQLError>, usize) {
    let UnifiedPlan::Query(mut selected) =
        UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
    else {
        panic!("query");
    };
    let mut ordinary = selected.clone();
    let expected = StoredCallOrigin::Definition.bind_query(
        &EmptyRoutineResolution,
        &mut ordinary,
        &[],
        &context(),
        None,
    );
    let memory = MemoryBudget::new(64 << 20);
    let token = CancellationToken::new();
    let original = CancellationToken::new();
    let mut reader = ImageDecoder::new(&memory, &token, |mut reader| async move {
        let mut input = reader.input();
        let control = ProductionControl::new(input.memory, &original, input.cancellation);
        let lease = StoredCallOrigin::Definition
            .bind_query_from(
                &EmptyRoutineResolution,
                &mut selected,
                &[],
                context().copy_with_control(&control)?,
                &control,
                &mut input,
            )
            .await?;
        assert_eq!(
            serde_json::to_vec(&selected).unwrap(),
            serde_json::to_vec(&ordinary).unwrap()
        );
        drop((selected, lease));
        Ok::<_, SQLError>(())
    })
    .unwrap();
    let mut polls = 0;
    let result = loop {
        polls += 1;
        if Some(polls) == cancel {
            token.cancel();
        }
        match reader.advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps }) {
            Ok(Some(())) => break Ok(()),
            Ok(None) => (),
            Err(error) => break Err(error),
        }
    };
    drop(reader);
    assert_eq!(memory.used(), 0);
    if cancel.is_none() {
        assert_eq!(
            result.as_ref().err().map(|e| (e.sqlstate(), e.to_string())),
            expected
                .as_ref()
                .err()
                .map(|e| (e.sqlstate(), e.to_string()))
        );
    }
    (result, polls)
}

#[test]
fn stored_call_selection_yields_within_variadic_arguments_and_data_modifying_ctes() {
    let args = std::iter::repeat_n("1", 4096).collect::<Vec<_>>().join(",");
    let sql = format!("SELECT coalesce({args})");
    let one = bind(&sql, 1, None);
    one.0.unwrap();
    let batch = bind(&sql, 32, None);
    batch.0.unwrap();
    assert!(one.1 > 4096 && batch.1 < one.1 / 8);
    assert_eq!(
        bind(&sql, 1, Some(256)).0.unwrap_err().sqlstate(),
        Some("57014")
    );

    let sql = |failure| {
        let mut rows = vec!["(abs(1))"; 128];
        rows[failure] = "(array_length(1, 1))";
        format!(
            "WITH saved AS (INSERT INTO items(id) VALUES {} RETURNING id) SELECT id FROM saved",
            rows.join(",")
        )
    };
    let first = bind(&sql(0), 1, None);
    let last = bind(&sql(127), 1, None);
    assert_eq!(first.0.unwrap_err().sqlstate(), Some("42883"));
    assert_eq!(last.0.unwrap_err().sqlstate(), Some("42883"));
    assert!(last.1 > first.1 + 127);
}

#[test]
fn stored_routine_call_selection_preserves_each_mutation_cte_namespace() {
    for body in [
        "INSERT INTO items(id) VALUES (abs(1)) RETURNING id",
        "UPDATE items SET id = abs(id) WHERE id > 0 RETURNING id",
        "DELETE FROM items WHERE abs(id) > 0 RETURNING id",
        "MERGE INTO items AS dst USING (VALUES (1)) AS src(v) ON dst.id = src.v WHEN MATCHED THEN UPDATE SET id = abs(src.v) WHEN NOT MATCHED THEN INSERT(id) VALUES (abs(src.v)) RETURNING dst.id",
    ] {
        let sql = format!("WITH saved AS ({body}) SELECT id FROM saved");
        bind(&sql, 8, None).0.unwrap_or_else(|error| panic!("{body}: {error}"));
    }
}
