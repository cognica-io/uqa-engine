//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Explicit field analyzer assignments select an exact declared TEXT column.

use crate::{
    ast::{ColumnDef, ColumnType},
    catalog::work::{inline, Work},
    SQLError,
};
use std::fmt;

/// The original declaration diagnostic borrows its selected type and names; controlled consumers own any eventual formatting.
#[derive(Debug)]
pub struct AnalyzerColumnError<'a> {
    table: &'a str,
    field: &'a str,
    ty: Option<&'a ColumnType>,
}

impl fmt::Display for AnalyzerColumnError<'_> {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self { table, field, ty } = self;
        match ty {
            Some(ty) => write!(
                output,
                "set_table_analyzer: column `{table}`.`{field}` must be TEXT, got {ty:?}"
            ),
            None => write!(
                output,
                "set_table_analyzer: column `{table}`.`{field}` does not exist"
            ),
        }
    }
}

/// Preserve first exact-name selection and the declared type identity; a domain over TEXT is not a TEXT declaration.
pub fn validate_analyzer_column<'a>(
    columns: &'a [ColumnDef],
    table: &'a str,
    field: &'a str,
) -> Result<(), AnalyzerColumnError<'a>> {
    inline(validate(columns, table, field, &mut ()))
        .expect("inline column validation has no fallible work")
}

/// Suspend between declarations while retaining the same borrowed diagnostic. The outer error is input cancellation or work failure; the inner result is the column predicate.
pub async fn validate_analyzer_column_from<'a>(
    columns: &'a [ColumnDef],
    table: &'a str,
    field: &'a str,
    input: &mut uqa_core::image_io::AsyncImageInput<'_>,
) -> Result<Result<(), AnalyzerColumnError<'a>>, SQLError> {
    validate(columns, table, field, input).await
}

async fn validate<'a>(
    columns: &'a [ColumnDef],
    table: &'a str,
    field: &'a str,
    work: &mut (impl Work + Send),
) -> Result<Result<(), AnalyzerColumnError<'a>>, SQLError> {
    let mut ty = None;
    for column in columns {
        work.checkpoint().await?;
        if column.name == field {
            ty = Some(&column.ty);
            break;
        }
    }
    if ty == Some(&ColumnType::Text) {
        Ok(Ok(()))
    } else {
        Ok(Err(AnalyzerColumnError { table, field, ty }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn analyzer_columns_borrow_first_exact_declarations_and_original_type_errors() {
        let columns = [
            ColumnDef::nullable("body", ColumnType::Integer),
            ColumnDef::nullable("body", ColumnType::Text),
            ColumnDef::nullable(
                "domain_body",
                ColumnType::Domain {
                    schema: "public".into(),
                    name: "text_domain".into(),
                    oid: 10000,
                    array_oid: None,
                    collation_oid: None,
                    base: Box::new(ColumnType::Text),
                },
            ),
        ];
        let allocations = allocation_counter::measure(|| {
            let error = validate_analyzer_column(&columns, "public.docs", "body").unwrap_err();
            assert!(std::ptr::eq(error.ty.unwrap(), &raw const columns[0].ty));
            let error =
                validate_analyzer_column(&columns, "public.docs", "domain_body").unwrap_err();
            assert!(std::ptr::eq(error.ty.unwrap(), &raw const columns[2].ty));
            assert!(validate_analyzer_column(&columns, "public.docs", "Body")
                .unwrap_err()
                .ty
                .is_none());
        });
        assert_eq!(allocations.count_total, 0);
        assert_eq!(
            validate_analyzer_column(&columns, "public.docs", "body")
                .unwrap_err()
                .to_string(),
            "set_table_analyzer: column `public.docs`.`body` must be TEXT, got Integer"
        );
        assert_eq!(
            validate_analyzer_column(&columns, "public.docs", "Body")
                .unwrap_err()
                .to_string(),
            "set_table_analyzer: column `public.docs`.`Body` does not exist"
        );
        assert!(validate_analyzer_column(&columns[1..], "public.docs", "body").is_ok());
    }

    #[test]
    fn analyzer_column_input_bounds_wide_search_and_preserves_first_match() {
        use uqa_core::{
            image_io::{ImageDecodeWindow, ImageDecoder},
            memory::MemoryBudget,
            CancellationToken,
        };

        for cancelled in [false, true] {
            let mut columns: Vec<_> = (0..4096)
                .map(|index| ColumnDef::nullable(format!("prefix_{index}"), ColumnType::Text))
                .collect();
            columns.extend([
                ColumnDef::nullable("body", ColumnType::Integer),
                ColumnDef::nullable("body", ColumnType::Text),
            ]);
            let memory = MemoryBudget::new(1 << 20);
            let cancellation = CancellationToken::new();
            let mut input = ImageDecoder::<String, SQLError>::new(
                &memory,
                &cancellation,
                |mut reader| async move {
                    let mut work = reader.input();
                    let result =
                        validate_analyzer_column_from(&columns, "docs", "body", &mut work).await?;
                    Ok(result.unwrap_err().to_string())
                },
            )
            .unwrap();
            let window = ImageDecodeWindow { bytes: 1, steps: 1 };
            for _ in 0..2048 {
                assert!(input
                    .advance(&mut std::io::empty(), window)
                    .unwrap()
                    .is_none());
            }
            if cancelled {
                cancellation.cancel();
                assert!(input.advance(&mut std::io::empty(), window).is_err());
            } else {
                let message = loop {
                    if let Some(message) = input.advance(&mut std::io::empty(), window).unwrap() {
                        break message;
                    }
                };
                assert_eq!(
                    message,
                    "set_table_analyzer: column `docs`.`body` must be TEXT, got Integer"
                );
            }
            assert!(input.advance(&mut std::io::empty(), window).is_err());
            assert_eq!(memory.used(), 0);
        }
    }
}
