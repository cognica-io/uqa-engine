//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Analyzer bindings preserve legacy phase replacement and durable descriptor invalidation atomically.

use super::{optional_text, string, text, Catalog, Family, Result};
use rusqlite::types::ValueRef;

#[derive(Clone, Copy)]
pub(in crate::catalog) enum FieldWrite<'a> {
    Legacy,
    Replace(Option<&'a str>),
}

type FieldAnalyzerRow = (String, String, String, String);

impl Catalog {
    pub(in crate::catalog) fn write_native_analyzer_field(
        &self,
        table: &str,
        field: &str,
        phase: &str,
        name: &str,
        write: FieldWrite<'_>,
    ) -> Result<Option<()>> {
        self.conn.with_native_write(|snapshot, batch| {
            let owner = snapshot.ensure_table_owner(table, batch)?;
            let binding = match write {
                FieldWrite::Legacy => {
                    snapshot.visit_rows(
                        Family::TableFieldAnalyzers,
                        Some(owner),
                        &[text(field)],
                        |row| {
                            if row[4] != ValueRef::Null {
                                snapshot.put_row(
                                    batch,
                                    Family::TableFieldAnalyzers,
                                    owner,
                                    &[row[0], row[1], row[2], row[3], ValueRef::Null],
                                )?;
                            }
                            Ok(())
                        },
                    )?;
                    None
                }
                FieldWrite::Replace(binding) => {
                    snapshot.delete_prefix(
                        batch,
                        Family::TableFieldAnalyzers,
                        owner,
                        &[text(field)],
                    )?;
                    binding
                }
            };
            snapshot.put_row(
                batch,
                Family::TableFieldAnalyzers,
                owner,
                &[
                    text(table),
                    text(field),
                    text(phase),
                    text(name),
                    optional_text(binding),
                ],
            )
        })
    }

    pub(in crate::catalog) fn drop_native_analyzer_fields(
        &self,
        table: &str,
        field: Option<&str>,
    ) -> Result<Option<()>> {
        self.conn.with_native_write(|snapshot, batch| {
            if let Some(owner) = snapshot.table_owner(table)? {
                match field {
                    Some(field) => snapshot.delete_prefix(
                        batch,
                        Family::TableFieldAnalyzers,
                        owner,
                        &[text(field)],
                    )?,
                    None => {
                        snapshot.delete_prefix(batch, Family::TableFieldAnalyzers, owner, &[])?;
                    }
                }
            }
            Ok(())
        })
    }

    pub(in crate::catalog) fn load_native_analyzer_fields(
        &self,
    ) -> Result<Option<Vec<FieldAnalyzerRow>>> {
        self.read_native(|snapshot| {
            let mut fields = Vec::new();
            snapshot.visit_rows(Family::TableFieldAnalyzers, None, &[], |row| {
                fields.push((
                    string(row[0])?,
                    string(row[1])?,
                    string(row[2])?,
                    string(row[3])?,
                ));
                Ok(())
            })?;
            fields.sort_unstable();
            Ok(fields)
        })
    }

    pub(in crate::catalog) fn load_native_analyzer_bindings(
        &self,
    ) -> Result<Option<Vec<(String, String, String)>>> {
        self.read_native(|snapshot| {
            let mut fields = Vec::new();
            snapshot.visit_rows(Family::TableFieldAnalyzers, None, &[], |row| {
                if row[4] != ValueRef::Null {
                    fields.push((string(row[0])?, string(row[1])?, string(row[4])?));
                }
                Ok(())
            })?;
            fields.sort_unstable();
            Ok(fields)
        })
    }
}
