//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Effective namespace order shares its selection while native ACLs and names retain bounded work.

use super::{CatalogReadView, RelationNameResolution, RoleSubject, SchemaAclPrivilege};
use std::{cmp::Ordering, future::Future};
use uqa_core::{
    image_io::AsyncImageInput,
    memory::{MapKeyInput, Produced, ProductionControl, ProductionString, ProductionVec},
};
use uqa_sql::{
    catalog::{
        roles::RoleReference,
        security::schema::{role_has_schema_privilege_from_input, SchemaPrivilegeView},
    },
    SQLError,
};

#[cfg(test)]
mod tests;

trait Source {
    fn checkpoint(
        &mut self,
        control: &ProductionControl<'_>,
    ) -> impl Future<Output = Result<(), SQLError>>;
    fn text(
        &mut self,
        source: &str,
        control: &ProductionControl<'_>,
    ) -> impl Future<Output = Result<Produced<String>, SQLError>>;
    fn usable(
        &mut self,
        name: &str,
        control: &ProductionControl<'_>,
    ) -> impl Future<Output = Result<bool, SQLError>>;
    async fn equal(
        &mut self,
        left: &str,
        right: &str,
        control: &ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        if left.len() != right.len() {
            return Ok(false);
        }
        for (left, right) in left
            .as_bytes()
            .chunks(4096)
            .zip(right.as_bytes().chunks(4096))
        {
            self.checkpoint(control).await?;
            if left != right {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

struct Ordinary<'a, S: ?Sized> {
    catalog: &'a CatalogReadView,
    resolution: &'a RelationNameResolution,
    role: &'a S,
}
impl<S: RoleSubject + ?Sized> Source for Ordinary<'_, S> {
    async fn checkpoint(&mut self, control: &ProductionControl<'_>) -> Result<(), SQLError> {
        Ok(control.check()?)
    }
    async fn text(
        &mut self,
        source: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<String>, SQLError> {
        Ok(control.copy_text(source)?)
    }
    async fn usable(
        &mut self,
        name: &str,
        control: &ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        super::production::schema_privilege(
            self.catalog,
            &self.resolution.temporary_schema,
            name,
            self.role,
            SchemaAclPrivilege::Usage,
            false,
            control,
        )
    }
}

struct Retained<'a, 'b, 'c> {
    catalog: &'a CatalogReadView,
    resolution: &'a RelationNameResolution,
    role: &'a RoleReference,
    input: &'b mut AsyncImageInput<'c>,
}
impl Source for Retained<'_, '_, '_> {
    async fn checkpoint(&mut self, control: &ProductionControl<'_>) -> Result<(), SQLError> {
        crate::catalog::selection_work::checkpoint(control, self.input).await
    }
    async fn text(
        &mut self,
        source: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<String>, SQLError> {
        let mut value = ProductionString::new(*control);
        value.reserve(source.len())?;
        let mut begin = 0;
        while begin < source.len() {
            self.checkpoint(control).await?;
            let mut end = begin.saturating_add(4096).min(source.len());
            while !source.is_char_boundary(end) {
                end -= 1;
            }
            value.push_str(&source[begin..end])?;
            begin = end;
        }
        Ok(value.finish()?)
    }
    async fn usable(
        &mut self,
        name: &str,
        control: &ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        let definitions = &self.catalog.snapshot().definitions;
        let security = definitions
            .schemas
            .get_from(
                name,
                &mut Comparison {
                    control: *control,
                    input: self.input,
                },
            )
            .await?;
        let same_temporary = crate::catalog::selection_work::equal(
            name,
            &self.resolution.temporary_schema,
            control,
            self.input,
        )
        .await?;
        let public = super::production::public_acl(same_temporary);
        let security = if let Some(security) = security {
            SchemaPrivilegeView::from(security)
        } else {
            let mut graph = false;
            for candidate in self.catalog.graph_names_iter() {
                crate::catalog::selection_work::checkpoint(control, self.input).await?;
                if crate::catalog::selection_work::equal(name, candidate, control, self.input)
                    .await?
                {
                    graph = true;
                    break;
                }
            }
            let Some(security) = super::production::fallback_security(
                name,
                !self.resolution.temporary_schema.is_empty() && same_temporary,
                graph,
                false,
                &public,
            ) else {
                return Ok(false);
            };
            security
        };
        role_has_schema_privilege_from_input(
            security,
            self.role,
            SchemaAclPrivilege::Usage,
            &definitions.roles,
            &definitions.role_memberships,
            control,
            self.input,
        )
        .await
    }
}

pub(super) fn ordinary(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    role: &(impl RoleSubject + ?Sized),
    include_implicit: bool,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<String>>, SQLError> {
    crate::catalog::selection_work::inline(select(
        resolution,
        include_implicit,
        control,
        &mut Ordinary {
            catalog,
            resolution,
            role,
        },
    ))
}

pub async fn current_schema_names_from_input(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    role: &RoleReference,
    include_implicit: bool,
    control: &ProductionControl<'_>,
    input: &mut AsyncImageInput<'_>,
) -> Result<Produced<Vec<String>>, SQLError> {
    select(
        resolution,
        include_implicit,
        control,
        &mut Retained {
            catalog,
            resolution,
            role,
            input,
        },
    )
    .await
}

async fn contains(
    names: &[&str],
    candidate: &str,
    control: &ProductionControl<'_>,
    source: &mut impl Source,
) -> Result<bool, SQLError> {
    for name in names {
        source.checkpoint(control).await?;
        if source.equal(name, candidate, control).await? {
            return Ok(true);
        }
    }
    Ok(false)
}
async fn select(
    resolution: &RelationNameResolution,
    include_implicit: bool,
    control: &ProductionControl<'_>,
    source: &mut impl Source,
) -> Result<Produced<Vec<String>>, SQLError> {
    source.checkpoint(control).await?;
    let mut explicit = ProductionVec::new(*control);
    for name in resolution.search_path() {
        source.checkpoint(control).await?;
        let name = if name == "pg_temp" {
            &resolution.temporary_schema
        } else {
            name
        };
        if (source
            .equal(name, &resolution.temporary_schema, control)
            .await?
            && !resolution.temporary_namespace_allocated)
            || contains(&explicit, name, control, source).await?
        {
            continue;
        }
        let usable = match source.usable(name, control).await {
            Err(SQLError::Internal(_)) => false,
            result => result?,
        };
        if usable {
            explicit.push_copy(name.as_str())?;
        }
    }
    let mut names = ProductionVec::new(*control);
    if include_implicit {
        if resolution.temporary_namespace_allocated
            && !contains(&explicit, &resolution.temporary_schema, control, source).await?
        {
            names.push_produced(source.text(&resolution.temporary_schema, control).await?)?;
        }
        if !contains(&explicit, "pg_catalog", control, source).await? {
            names.push_produced(source.text("pg_catalog", control).await?)?;
        }
    }
    for name in explicit.iter() {
        names.push_produced(source.text(name, control).await?)?;
    }
    Ok(names.finish()?)
}

struct Comparison<'a, 'b, 'c> {
    control: ProductionControl<'a>,
    input: &'b mut AsyncImageInput<'c>,
}
impl MapKeyInput<str, String> for Comparison<'_, '_, '_> {
    type Error = SQLError;
    async fn checkpoint(&mut self) -> Result<(), Self::Error> {
        crate::catalog::selection_work::checkpoint(&self.control, self.input).await
    }
    async fn compare(&mut self, query: &str, stored: &String) -> Result<Ordering, Self::Error> {
        for (left, right) in query
            .as_bytes()
            .chunks(4096)
            .zip(stored.as_bytes().chunks(4096))
        {
            self.checkpoint().await?;
            let order = left.cmp(right);
            if order.is_ne() {
                return Ok(order);
            }
        }
        Ok(query.len().cmp(&stored.len()))
    }
}
