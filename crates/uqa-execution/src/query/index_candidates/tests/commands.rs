//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::index::value::ColumnValueIndex;
use crate::mutation::overlay::CommandMutationOverlay;
use parking_lot::Mutex;
use std::{collections::BTreeMap, sync::Arc};
use uqa_storage::{read_control::StorageReadControl, DocumentMetadata, StoredDocument};

struct View {
    index: ColumnValueIndex,
    frames: Mutex<Vec<CommandMutationOverlay>>,
    fixed: DocumentChanges,
    control: StorageReadControl,
}

impl QueryIndexRead for View {
    fn value_index_scan(
        &self,
        _: &str,
        _: &str,
        _: &Predicate,
    ) -> Result<Option<PostingList>, SQLError> {
        panic!("candidate capture must use the visitor")
    }

    fn visit_value_index(
        &self,
        _: &str,
        _: &str,
        predicate: &Predicate,
        visit: &mut dyn FnMut(DocId) -> Result<(), SQLError>,
    ) -> Result<bool, SQLError> {
        self.index
            .visit_observing_with_catalog(predicate, None, || Ok(()), visit)
    }

    fn command_overlay_changes(&self, table: &str) -> Result<Option<DocumentChanges>, SQLError> {
        CommandMutationOverlay::changes(
            &self.frames.lock(),
            table,
            self.fixed.clone(),
            &self.control,
        )
        .map(Some)
    }

    fn exact_command_matches(
        &self,
        _: &str,
        _: &str,
        _: &Value,
    ) -> Result<CommandIndexProbe, SQLError> {
        panic!("candidate capture must use the command visitor")
    }

    fn visit_exact_command_matches(
        &self,
        table: &str,
        field: &str,
        value: &Value,
        visit: &mut dyn FnMut(DocId) -> Result<(), SQLError>,
    ) -> Result<DocumentChanges, SQLError> {
        CommandMutationOverlay::visit_column_matches_with_catalog(
            &mut self.frames.lock(),
            table,
            &[field.into()],
            std::slice::from_ref(value),
            &self.control,
            None,
            visit,
        )
    }
}

fn stage(view: &View, frame: usize, id: DocId, value: Option<Value>) {
    view.frames.lock()[frame]
        .stage(
            "t",
            id,
            value.map(|value| {
                (
                    Arc::new(BTreeMap::from([("v".into(), value)])),
                    DocumentMetadata::default(),
                )
            }),
            &view.control,
        )
        .unwrap();
}

fn layered_view() -> View {
    let control = StorageReadControl::with_limit(1024 * 1024);
    let fixed = DocumentChanges::from_rows(
        [
            (
                1,
                Some(StoredDocument::new([("v".into(), Value::Int(30))].into())),
            ),
            (2, None),
            (
                6,
                Some(StoredDocument::new([("v".into(), Value::Int(10))].into())),
            ),
        ]
        .into(),
        &control,
    )
    .unwrap();
    View {
        index: ColumnValueIndex::build(
            "v",
            [
                (1, Value::Int(10)),
                (2, Value::Int(10)),
                (4, Value::Int(10)),
                (7, Value::Int(10)),
            ]
            .into_iter(),
        ),
        frames: Mutex::new(vec![
            CommandMutationOverlay::default(),
            CommandMutationOverlay::default(),
        ]),
        fixed,
        control,
    }
}

#[test]
fn selected_index_candidates_freeze_newest_command_and_fixed_versions() {
    let uqa_sql::Statement::CreateTable(table) = uqa_sql::compile("CREATE TABLE t(v integer)")
        .unwrap()
        .remove(0)
    else {
        unreachable!()
    };
    for bytes in [0, 64 * 1024] {
        let view = layered_view();
        stage(&view, 0, 1, Some(Value::Int(20)));
        stage(&view, 0, 3, Some(Value::Int(10)));
        stage(&view, 0, 4, Some(Value::Int(10)));
        stage(&view, 1, 3, None);
        stage(&view, 1, 5, Some(Value::Int(10)));
        stage(&view, 1, 8, Some(Value::Null));
        let context = IndexCandidates {
            reads: &view,
            table: "t",
            columns: &table.columns,
            visible: &str::to_owned,
            params: &[],
            command_visible: true,
            cancellation: view.control.cancellation(),
            bytes,
        };
        let predicate = Predicate::Equals(Value::Int(10));
        let frozen = capture::capture(&context, "v", &predicate)
            .unwrap()
            .unwrap();
        assert_eq!(
            frozen.cursor().collect::<Result<Vec<_>, _>>().unwrap(),
            [4, 5, 6, 7]
        );
        let checkpoint = view.frames.lock()[1].checkpoint();
        stage(&view, 1, 5, None);
        assert_eq!(
            capture::capture(&context, "v", &predicate)
                .unwrap()
                .unwrap()
                .cursor()
                .collect::<Result<Vec<_>, _>>()
                .unwrap(),
            [4, 6, 7],
        );
        assert_eq!(
            frozen.cursor().collect::<Result<Vec<_>, _>>().unwrap(),
            [4, 5, 6, 7]
        );
        view.frames.lock()[1].restore(&checkpoint);
        for (predicate, expected) in [
            (Predicate::Equals(Value::Int(10)), vec![4, 5, 6, 7]),
            (Predicate::GreaterThan(Value::Int(15)), vec![1]),
            (Predicate::IsNull, vec![8]),
            (Predicate::IsNotNull, vec![1, 4, 5, 6, 7]),
            (Predicate::Equals(Value::Null), vec![]),
        ] {
            let selected = capture::capture(&context, "v", &predicate)
                .unwrap()
                .unwrap();
            assert_eq!(
                selected.cursor().collect::<Result<Vec<_>, _>>().unwrap(),
                expected
            );
        }
        let hidden = IndexCandidates {
            command_visible: false,
            ..context
        };
        assert_eq!(
            capture::capture(&hidden, "v", &Predicate::Equals(Value::Int(10)))
                .unwrap()
                .unwrap()
                .cursor()
                .collect::<Result<Vec<_>, _>>()
                .unwrap(),
            [1, 2, 4, 7],
        );
    }
}
