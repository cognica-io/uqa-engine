//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Domain declarations retain type identity and constraints until catalog binding.

use serde::{Deserialize, Serialize};

use super::{ColumnType, ConstraintCatalogIdentity, Expr};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateDomain {
    pub name: String,
    pub base: ColumnType,
    pub collation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collation_oid: Option<u32>,
    pub default: Option<Expr>,
    pub not_null: Option<DomainNotNull>,
    pub checks: Vec<DomainCheck>,
}

impl CreateDomain {
    /// Observe application lookups reached by this stored default and its CHECK conditions. Constraint validation status does not affect their use for future values.
    pub async fn visit_runtime_scalar_calls<'a, 'input: 'a, F>(
        &'a self,
        original: &'a uqa_core::CancellationToken,
        input: &'a mut uqa_core::image_io::AsyncImageInput<'input>,
        visit: &'a mut F,
    ) -> Result<(), crate::SQLError>
    where
        F: FnMut(
                &str,
                crate::binding::view_dependencies::StoredFunctionSite,
            ) -> Result<(), crate::SQLError>
            + Send,
    {
        use crate::binding::view_dependencies::StoredFunctionSite;
        let mut observe = |name: &str, binding: Option<&super::FunctionBinding>| {
            if let Some(site) = StoredFunctionSite::Domain.with_stored_binding(binding) {
                visit(name, site)?;
            }
            Ok(())
        };
        if let Some(default) = &self.default {
            default
                .visit_stored_scalar_calls_from_input(original, input, &mut observe)
                .await?;
        }
        for check in &self.checks {
            check
                .expression
                .visit_stored_scalar_calls_from_input(original, input, &mut observe)
                .await?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainNotNull {
    pub name: Option<String>,
    #[serde(default)]
    pub catalog_identity: Option<ConstraintCatalogIdentity>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainCheck {
    pub name: Option<String>,
    #[serde(default)]
    pub catalog_identity: Option<ConstraintCatalogIdentity>,
    pub expression: Expr,
    #[serde(default = "super::default_true")]
    pub validated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlterDomain {
    pub name: String,
    pub action: AlterDomainAction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AlterDomainAction {
    SetDefault {
        expression: Option<Box<Expr>>,
    },
    AddCheck {
        constraint: Box<DomainCheck>,
    },
    AddNotNull {
        constraint: DomainNotNull,
    },
    DropConstraint {
        name: String,
        if_exists: bool,
        cascade: bool,
    },
    ValidateConstraint {
        name: String,
    },
}
