//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Structural constructors retain moved query/source owners beneath newly admitted containers.

use super::{Copying, Result, RewriteField};
use crate::plan::{MutableQueryPlan, MutableSourcePlan, QueryPlan, RelationalPlan, SourcePlan};
use uqa_core::memory::Produced;

impl Copying<'_> {
    pub(in crate::plan) fn boxed_query(
        &mut self,
        query: MutableQueryPlan,
    ) -> Result<Box<QueryPlan>> {
        self.boxed(|copying| copying.take_query(query))
    }

    pub(in crate::plan) fn insert_values_query(
        &mut self,
        rows: crate::plan::mutable::values::OwnedRows,
        subqueries: &[QueryPlan],
        relations_bound: bool,
    ) -> Result<QueryPlan> {
        let mut shell = crate::plan::mutable::construction::empty_projection_query();
        shell.relations_bound = relations_bound;
        self.query_fields_using(&shell, |copying| {
            copying.fields.as_mut().expect("VALUES fields").reserve(1)?;
            let crate::plan::mutable::values::OwnedRows { rows, memory } = rows;
            copying
                .fields
                .as_mut()
                .expect("VALUES fields")
                .push(RewriteField {
                    name: "rows",
                    memory,
                })?;
            Ok(RelationalPlan::Values {
                rows,
                subqueries: copying.field("subqueries", |copying| {
                    copying.map(subqueries.iter(), Self::query)
                })?,
            })
        })
    }

    pub(in crate::plan) fn implicit_insert_columns<'a>(
        &mut self,
        names: impl ExactSizeIterator<Item = &'a str>,
    ) -> Result<Vec<crate::ast::AssignmentTarget<crate::ScalarExpr>>> {
        self.map(names, |copying, name| {
            Ok(crate::ast::AssignmentTarget {
                column: copying.text(name)?,
                indirection: copying.field("indirection", |_| Ok(Vec::new()))?,
            })
        })
    }

    pub(crate) fn boxed_source(&mut self, source: MutableSourcePlan) -> Result<Box<SourcePlan>> {
        self.boxed(|copying| copying.take_source(source))
    }

    pub(crate) fn take_privilege_subject(
        &mut self,
        subject: Produced<crate::catalog::roles::RoleReference>,
    ) -> Result<crate::catalog::roles::RoleReference> {
        self.take_checked(subject)
    }
    pub(crate) fn query_block_root(
        &mut self,
        block: &crate::plan::QueryBlockPlan,
    ) -> Result<QueryPlan> {
        let query = super::super::mutable::construction::empty_projection_query();
        self.query_fields_using(&query, |copying| {
            Ok(RelationalPlan::QueryBlock(
                copying.boxed(|copying| copying.block(block))?,
            ))
        })
    }

    pub(crate) fn take_query(&mut self, query: MutableQueryPlan) -> Result<QueryPlan> {
        self.check()?;
        self.fields
            .as_mut()
            .expect("mutable query destination")
            .reserve(1)?;
        let (query, memory) = query.into_rewrite_parts(&self.control)?;
        self.fields
            .as_mut()
            .expect("mutable query destination")
            .push(RewriteField {
                name: "query",
                memory,
            })
            .expect("query owner slot was admitted");
        Ok(query)
    }

    pub(crate) fn take_source(&mut self, source: MutableSourcePlan) -> Result<SourcePlan> {
        self.check()?;
        self.fields
            .as_mut()
            .expect("mutable source destination")
            .reserve(1)?;
        let (source, memory) = source.into_rewrite_parts(&self.control)?;
        self.fields
            .as_mut()
            .expect("mutable source destination")
            .push(RewriteField {
                name: "source",
                memory,
            })
            .expect("source owner slot was admitted");
        Ok(source)
    }

    pub(crate) fn projected_query(&mut self) -> Result<QueryPlan> {
        let query = super::super::mutable::construction::empty_projection_query();
        self.query_fields_using(&query, |copying| {
            Ok(RelationalPlan::QueryBlock(copying.boxed(|copying| {
                copying.block(&super::super::mutable::construction::empty_projection_block())
            })?))
        })
    }

    pub(crate) fn unit_source(&mut self) -> Result<SourcePlan> {
        Ok(SourcePlan::Values {
            lateral: false,
            rows: self.field("rows", |copying| {
                copying.map(std::iter::once(()), |copying, ()| {
                    copying.map(std::iter::empty::<()>(), |_, ()| unreachable!())
                })
            })?,
            alias: None,
            column_aliases: Vec::new(),
            internal_relation: None,
            internal_column_types: Vec::new(),
            internal_column_collations: Vec::new(),
        })
    }

    pub(crate) fn subquery_source(
        &mut self,
        query: MutableQueryPlan,
        alias: Option<Produced<String>>,
        columns: Produced<Vec<String>>,
        captured_output: Option<crate::ast::InternalRelationId>,
        lateral: bool,
    ) -> Result<SourcePlan> {
        Ok(SourcePlan::Subquery {
            captured_output,
            lateral,
            body: self.field("body", |copying| {
                copying.boxed(|copying| copying.take_query(query))
            })?,
            alias: alias.map(|alias| self.take_checked(alias)).transpose()?,
            column_aliases: self.take_checked(columns)?,
        })
    }

    fn take_checked<T>(&mut self, value: Produced<T>) -> Result<T> {
        let (value, memory) = value.into_parts();
        let value = self.control.finish(value, memory)?;
        Ok(self.take(value))
    }
}

impl Copying<'_> {
    pub(in crate::plan) fn take_fields<T>(
        &mut self,
        input: crate::plan::mutable::field_vectors::OwnedFields<T>,
    ) -> Result<T> {
        self.check()?;
        assert!(
            self.fields.as_ref().expect("field destination").is_empty(),
            "new field destination"
        );
        let crate::plan::mutable::field_vectors::OwnedFields { value, memory } = input;
        let super::RewriteMemory::Fields { direct, fields } =
            memory.expect("controlled field owner")
        else {
            unreachable!("metadata field constructor");
        };
        self.memory.absorb(direct);
        self.fields = Some(fields);
        Ok(value)
    }
}
