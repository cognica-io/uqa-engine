//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

struct InspectFiles {
    schema: RowSchema,
    rows: std::vec::IntoIter<ResultRow>,
    directory: PathBuf,
}

impl PhysicalOperator for InspectFiles {
    fn row_schema(&self) -> &RowSchema {
        &self.schema
    }

    fn open(&mut self) -> ExecResult<()> {
        Ok(())
    }

    fn next(&mut self) -> ExecResult<Option<Batch>> {
        let files = std::fs::read_dir(&self.directory).unwrap().count();
        assert!(
            files <= 1,
            "initial sorted runs created {files} separate files"
        );
        Ok(self.rows.next().map(|row| {
            Batch::from_physical_rows(
                self.schema.clone(),
                vec![PhysicalRow::from_values(vec![
                    row["key"].clone(),
                    row["input"].clone(),
                ])],
            )
        }))
    }

    fn close(&mut self) -> ExecResult<()> {
        Ok(())
    }
}

#[test]
fn tiny_runs_share_each_pass_file_and_preserve_stable_output_and_cleanup() {
    for stop_early in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let rows: Vec<_> = (0..257).map(|id| row((256 - id) % 7, id)).collect();
        let mut expected = rows.clone();
        expected.sort_by_key(|row| match row["key"] {
            Value::Int(key) => key,
            _ => unreachable!(),
        });
        let mut operator = ExternalSort::new(
            Box::new(InspectFiles {
                schema: vec!["key".into(), "input".into()].into(),
                rows: rows.into_iter(),
                directory: directory.path().to_path_buf(),
            }),
            vec![SortKey {
                expr: ScalarExpr::Column("key".into()),
                descending: false,
                nulls_first: None,
            }],
            Arc::new(Columns),
            None,
            1,
        )
        .with_spill_directory(directory.path());
        operator.open().unwrap();
        assert_eq!(operator.initial_run_count(), 257);
        assert!(operator.merge_pass_count() >= 2);
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
        let mut actual = Vec::new();
        while let Some(batch) = operator.next().unwrap() {
            actual.extend(batch.into_result_rows());
            if stop_early {
                break;
            }
        }
        if !stop_early {
            assert_eq!(actual, expected);
        }
        drop(operator);
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    }
}
