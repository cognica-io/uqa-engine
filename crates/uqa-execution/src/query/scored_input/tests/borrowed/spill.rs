//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{predicate, table_with_shared, ScoredDocumentSource, ScoredInput};
use crate::query::{
    collection::collect_query_operator,
    consumer::QueryOutputMode,
    runtime::{QueryMemorySettings, QueryRuntimeView},
};
use crate::{
    Batch, ColumnSelection, PhysicalOperator, PhysicalRow, Project, RowSource, ScalarExpr,
    TableScan,
};
use parking_lot::RwLock;
use uqa_core::Value;

struct WorkMem(usize);

impl QueryMemorySettings for WorkMem {
    fn work_mem_bytes(&self) -> Result<usize, uqa_sql::SQLError> {
        Ok(self.0)
    }
}

fn source(shared: bool) -> ScoredDocumentSource {
    ScoredDocumentSource::new(
        "t",
        table_with_shared(shared),
        ScoredInput::All,
        vec!["key".into(), "missing".into()],
        None,
        Some(predicate()),
    )
}

fn projection(shared: bool) -> Box<dyn PhysicalOperator> {
    let scan = TableScan::new(Box::new(source(shared)));
    let project = Project::new(
        Box::new(scan),
        vec![
            ("renamed".into(), ScalarExpr::Column("key".into())),
            ("absent".into(), ScalarExpr::Column("missing".into())),
        ],
        Vec::new(),
    );
    Box::new(ColumnSelection::with_positions(
        Box::new(project),
        vec![("duplicate".into(), 1), ("duplicate".into(), 0)],
    ))
}

fn cursor(shared: bool, bytes: usize) -> crate::query::cursor::SQLCursor {
    let operator = projection(shared);
    let columns = operator.schema().to_vec();
    collect_query_operator(
        QueryRuntimeView {
            function_context: &crate::functions::SQLFunctionContext::empty(),
            diagnostics: &crate::query::diagnostics::QueryDiagnostics::default(),
            cancellation: &uqa_core::CancellationToken::new(),
            settings: &WorkMem(bytes),
            scalar_functions: &RwLock::default(),
            table_functions: &RwLock::default(),
            aggregate_functions: &RwLock::default(),
            notices: &crate::query::NoticeQueue::default(),
        },
        columns,
        operator,
        QueryOutputMode::SharedSpill,
    )
    .unwrap()
    .into_cursor()
    .unwrap()
}

#[test]
fn cursor_buffers_measured_scan_records_once_through_schema_only_projection() {
    for shared in [false, true] {
        let mut source = source(shared);
        let schema = source.physical_schema().unwrap().clone();
        let (_, scanned) = PhysicalRow::count_fragment_width_reads(|| loop {
            let rows = source
                .next_physical_batch(crate::DEFAULT_BATCH_SIZE)
                .unwrap();
            if rows.is_empty() {
                break;
            }
            std::hint::black_box(Batch::from_physical_rows(schema.clone(), rows));
        });
        let (result, buffered) =
            PhysicalRow::count_fragment_width_reads(|| cursor(shared, usize::MAX));
        assert!(scanned > 0);
        assert!(
            buffered <= scanned,
            "shared={shared}: buffer {buffered}, scan {scanned}"
        );
        assert!(!result.spilled_to_disk());
        let disk = cursor(shared, 1);
        assert!(disk.spilled_to_disk());
        for result in [result, disk] {
            assert_eq!(result.columns(), ["duplicate", "duplicate"]);
            assert_eq!(result.row_count(), 2);
            let rows: Vec<_> = result
                .flat_map(|batch| batch.unwrap().into_positional_rows())
                .collect();
            assert_eq!(
                rows,
                [
                    vec![Value::Null, Value::Int(7)],
                    vec![Value::Null, Value::Int(9)]
                ]
            );
        }
    }
}
