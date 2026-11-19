//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::MemoryBudget,
    CancellationToken,
};
use uqa_sql::catalog::roles::RoleDefinition;

#[test]
fn physical_routine_records_resume_overloads_and_release_their_private_inputs() {
    let mut overloads = Vec::new();
    for sql in ["CREATE FUNCTION saved() RETURNS int LANGUAGE SQL RETURN supplied(1)::int", "CREATE FUNCTION saved(v int DEFAULT extra(1)::int) RETURNS int LANGUAGE SQL AS $$ SELECT deferred(v) $$"] {
        let uqa_sql::Statement::CreateFunction(mut definition) = uqa_sql::compile(sql).unwrap().remove(0) else { panic!("routine") };
        definition.object_id = Some([1; 16]);
        definition.owner = Some(RoleDefinition::bootstrap().identity());
        overloads.push(*definition);
    }
    let record = std::sync::Arc::new(
        crate::routines::catalog::encoding::encode(std::collections::BTreeMap::from([(
            "saved".to_string(),
            overloads,
        )]))
        .unwrap(),
    );
    let memory = MemoryBudget::new(8 << 20);
    for cancel in [true, false] {
        let cancellation = CancellationToken::new();
        let source = StorageReadControl::new(&memory, &cancellation);
        let bytes = std::sync::Arc::clone(&record);
        let mut decoder = ImageDecoder::new(&memory, &cancellation, move |mut reader| async move {
            let mut observed = Vec::new();
            visit_stored_routine_runtime_calls(
                bytes.as_bytes(),
                &source,
                &mut reader.input(),
                &mut |name, site| {
                    observed.push((name.to_string(), site));
                    Ok(())
                },
            )
            .await?;
            Ok::<_, StorageBackendError>(observed)
        })
        .unwrap();
        let window = ImageDecodeWindow { bytes: 1, steps: 7 };
        let mut polls = 0;
        loop {
            polls += 1;
            if cancel && polls == 10 {
                cancellation.cancel();
            }
            match decoder.advance(&mut std::io::empty(), window) {
                Ok(None) => assert!(polls < 100_000),
                Ok(Some(observed)) => {
                    assert!(!cancel);
                    assert_eq!(
                        observed,
                        [
                            ("supplied".into(), StoredFunctionSite::Expression),
                            ("extra".into(), StoredFunctionSite::ScalarOnly)
                        ]
                    );
                    assert!(polls > 10);
                    break;
                }
                Err(StorageBackendError::Cancelled(_)) if cancel => break,
                other => panic!("unexpected routine input result: {other:?}"),
            }
        }
        drop(decoder);
        assert_eq!(memory.used(), 0);
    }
}
