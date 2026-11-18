//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Column grant validation borrows the selected row descriptor and checks each attribute in original order.

use super::{
    acl, invalid, table_bits, AuthorityCatalog, BoundTableSecurity, ProductionControl, SQLError,
    Work,
};
use std::future::{ready, Future};

trait Columns<W> {
    fn contains(
        &mut self,
        name: &str,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> impl Future<Output = Result<bool, SQLError>> + Send;
}

struct Lookup<F>(F);

impl<W, F: FnMut(&str) -> Result<bool, SQLError>> Columns<W> for Lookup<F> {
    fn contains(
        &mut self,
        name: &str,
        _: &ProductionControl<'_>,
        _: &mut W,
    ) -> impl Future<Output = Result<bool, SQLError>> + Send {
        ready(self.0(name))
    }
}

struct Names<I>(I);

impl<'a, I: Iterator<Item = &'a str> + Clone + Send, W: Work + Send> Columns<W> for Names<I> {
    async fn contains(
        &mut self,
        name: &str,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<bool, SQLError> {
        crate::catalog::security::table::invariants::contains_column(
            self.0.clone(),
            name,
            control,
            work,
        )
        .await
    }
}

impl AuthorityCatalog<'_> {
    /// Column existence comes from the accompanying immutable row descriptor. Relation grants contribute column grant options, while each ACL retains its own ordered paths and duplicate-path boundary.
    pub fn table(
        &self,
        value: &BoundTableSecurity,
        column_exists: impl FnMut(&str) -> Result<bool, SQLError>,
    ) -> Result<(), SQLError> {
        super::inline(self.table_columns(value, &mut Lookup(column_exists), &mut ()))
    }

    pub async fn table_with_work<'a, W: Work + Send>(
        &self,
        value: &BoundTableSecurity,
        names: impl Iterator<Item = &'a str> + Clone + Send,
        work: &mut W,
    ) -> Result<(), SQLError> {
        self.table_columns(value, &mut Names(names), work).await
    }

    async fn table_columns<W: Work>(
        &self,
        value: &BoundTableSecurity,
        columns: &mut impl Columns<W>,
        work: &mut W,
    ) -> Result<(), SQLError> {
        if value.acl_revisions.relation == Some([0; 16]) {
            return Err(invalid("invalid relation ACL tuple identity"));
        }
        self.grants(
            value.role_owner,
            acl(value.acl.as_deref(), table_bits),
            std::iter::empty(),
            work,
        )
        .await?;
        let control = ProductionControl::new(self.memory, self.cancellation, self.cancellation);
        for (column, revision) in &value.acl_revisions.columns {
            self.checkpoint(work).await?;
            if *revision == [0; 16] || !columns.contains(column, &control, work).await? {
                return Err(invalid("invalid column ACL tuple identity"));
            }
        }
        for (column, entries) in &value.column_acls {
            self.checkpoint(work).await?;
            if !columns.contains(column, &control, work).await? {
                return Err(invalid("column ACL references a missing column"));
            }
            for entry in entries {
                self.checkpoint(work).await?;
                if (table_bits(entry.privileges) | table_bits(entry.grant_options)) & !0x27 != 0 {
                    return Err(invalid("column ACL contains a relation-only privilege"));
                }
            }
            self.grants(
                value.role_owner,
                acl(Some(entries), table_bits),
                acl(value.acl.as_deref(), table_bits),
                work,
            )
            .await?;
        }
        self.checkpoint(work).await?;
        Ok(())
    }
}
