//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

struct Aggregate {
    borrowed: bool,
    count: usize,
}

impl crate::AggregateExecutor for Aggregate {
    fn consume(&mut self, _: crate::Batch) -> crate::ExecResult<()> {
        panic!("this executor consumes projected rows")
    }
    fn supports_projected_rows(&self) -> bool {
        true
    }
    fn supports_storage_borrowed_rows(&self) -> bool {
        self.borrowed
    }
    fn consume_projected_row(&mut self, _: &crate::ProjectedRow<'_, '_>) -> crate::ExecResult<()> {
        self.count += 1;
        Ok(())
    }
    fn finish(&mut self) -> crate::ExecResult<crate::spill::SpillBuffer> {
        unreachable!()
    }
}

#[test]
fn indexed_aggregate_requires_the_strict_storage_borrow_capability() {
    for borrowed in [false, true] {
        let source = ScoredDocumentSource::new(
            "t",
            table_with_shared(!borrowed),
            ScoredInput::All,
            vec!["key".into()],
            None,
            None,
        );
        let mut executor = Aggregate { borrowed, count: 0 };
        source
            .aggregate_entries(
                &[
                    uqa_core::ScoredEntry {
                        doc_id: 3,
                        score: 0.0,
                    },
                    uqa_core::ScoredEntry {
                        doc_id: 1,
                        score: 0.0,
                    },
                ],
                &mut executor,
            )
            .unwrap();
        assert_eq!(executor.count, 2);
    }
}
