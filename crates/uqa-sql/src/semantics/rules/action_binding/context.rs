//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Read-only relation shapes used to bind stored rule actions.
use super::{
    cte_output_columns,
    names::{self, Columns, Named},
    SQLError,
};
use crate::ast::{ColumnType, Statement, CTE};
use uqa_core::memory::{OwnedProductionControl, Produced, ProductionControl, ProductionVec};

pub trait RuleSourceCatalog {
    fn query_source_columns(
        &self,
        name: &str,
        relations_bound: bool,
    ) -> Result<Option<Vec<String>>, SQLError>;
    fn query_source_columns_with_control(
        &self,
        name: &str,
        relations_bound: bool,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<Vec<String>>>, SQLError> {
        control.check()?;
        let Some(columns) = self.query_source_columns(name, relations_bound)? else {
            return Ok(None);
        };
        if control.budget().is_none() {
            return Ok(Some(control.finish(columns, None)?));
        }
        let mut output = ProductionVec::new(*control);
        for column in columns {
            output.push_produced(control.copy_text(&column)?)?;
        }
        Ok(Some(output.finish()?))
    }
    fn rule_relation_columns_with_control(
        &self,
        name: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Vec<String>>, SQLError> {
        control.check()?;
        let columns = self.rule_relation_definitions(name)?;
        let mut output = ProductionVec::new(*control);
        for column in columns {
            output.push_produced(control.copy_text(&column.name)?)?;
        }
        Ok(output.finish()?)
    }
    fn rule_relation_definitions(&self, name: &str)
        -> Result<Vec<crate::ast::ColumnDef>, SQLError>;
}

pub(super) struct RuleBindingContext<'a> {
    pub(super) catalog: Option<&'a dyn RuleSourceCatalog>,
    pub(super) relations_bound: bool,
    pub(super) ctes: Named<Columns>,
    control: OwnedProductionControl,
}
impl Default for RuleBindingContext<'_> {
    fn default() -> Self {
        Self::from_control(&ProductionControl::uncontrolled())
    }
}
impl<'a> RuleBindingContext<'a> {
    pub(super) fn from_control(control: &ProductionControl<'_>) -> Self {
        Self {
            catalog: None,
            relations_bound: false,
            ctes: Named::new(),
            control: control.capture(),
        }
    }
    pub(super) fn with_catalog(catalog: &'a dyn RuleSourceCatalog, relations_bound: bool) -> Self {
        Self::with_control(catalog, relations_bound, &ProductionControl::uncontrolled())
    }
    pub(super) fn with_control(
        catalog: &'a dyn RuleSourceCatalog,
        relations_bound: bool,
        control: &ProductionControl<'_>,
    ) -> Self {
        Self {
            catalog: Some(catalog),
            relations_bound,
            ctes: Named::new(),
            control: control.capture(),
        }
    }
    pub(super) fn control(&self) -> ProductionControl<'_> {
        self.control.control()
    }
    pub(super) fn copy(&self) -> Result<Self, SQLError> {
        let mut ctes = Named::new();
        let control = self.control();
        for (name, columns) in &self.ctes {
            names::insert(&mut ctes, name, columns.copy(&control)?, &control)?;
        }
        Ok(Self {
            catalog: self.catalog,
            relations_bound: self.relations_bound,
            ctes,
            control: self.control.clone(),
        })
    }
    pub(super) fn relation_columns(&self, name: &str) -> Result<Columns, SQLError> {
        let control = self.control();
        control.check()?;
        if let Some(columns) = self.ctes.get_by(|key| names::folded_cmp(name, key)) {
            return columns.copy(&control);
        }
        let Some(catalog) = self.catalog else {
            return Ok(Columns::new(&control));
        };
        let columns = catalog
            .query_source_columns_with_control(name, self.relations_bound, &control)?
            .ok_or_else(|| SQLError::UnknownTable(name.to_string()))?;
        Columns::from_produced(columns, &control)
    }
    fn recursive_placeholder(&mut self, cte: &CTE) -> Result<(), SQLError> {
        if cte.recursive
            && self
                .ctes
                .get_by(|key| names::folded_cmp(&cte.name, key))
                .is_none()
        {
            let control = self.control.clone();
            let control = control.control();
            names::insert(&mut self.ctes, &cte.name, Columns::new(&control), &control)?;
        }
        Ok(())
    }
    pub(super) fn prepare_recursive_cte(&mut self, cte: &CTE) -> Result<(), SQLError> {
        if cte.recursive {
            self.recursive_placeholder(cte)?;
            self.record_cte_columns(cte, false)?;
        }
        Ok(())
    }
    pub(super) fn record_cte(&mut self, cte: &CTE) -> Result<(), SQLError> {
        self.record_cte_columns(cte, true)
    }
    fn record_cte_columns(&mut self, cte: &CTE, extra: bool) -> Result<(), SQLError> {
        let mut columns = cte_output_columns(&cte.body, self)?;
        let owner = self.control.clone();
        let control = owner.control();
        columns.rename(&cte.columns, &control)?;
        if extra {
            if let Some(search) = &cte.search {
                columns.push(&search.sequence_column, &control)?;
            }
            if let Some(cycle) = &cte.cycle {
                columns.push(&cycle.mark_column, &control)?;
                columns.push(&cycle.path_column, &control)?;
            }
        }
        names::insert(&mut self.ctes, &cte.name, columns, &control)
    }
    pub(super) fn with_ctes(&self, ctes: &[CTE]) -> Result<Self, SQLError> {
        let mut context = self.copy()?;
        for cte in ctes {
            context.recursive_placeholder(cte)?;
            context.record_cte(cte)?;
        }
        Ok(context)
    }
}

pub fn rule_action_target_row_type(
    catalog: &dyn RuleSourceCatalog,
    action: &Statement,
) -> Result<Vec<(String, ColumnType)>, SQLError> {
    let table = match action {
        Statement::Insert(statement) => &statement.table,
        Statement::Update(statement) => &statement.table,
        Statement::Delete(statement) => &statement.table,
        _ => return Ok(Vec::new()),
    };
    Ok(catalog
        .rule_relation_definitions(table)?
        .into_iter()
        .map(|column| (column.name, column.ty))
        .collect())
}
pub fn rule_action_target_columns(
    catalog: &dyn RuleSourceCatalog,
    action: &Statement,
) -> Result<std::collections::BTreeSet<String>, SQLError> {
    Ok(rule_action_target_row_type(catalog, action)?
        .into_iter()
        .map(|(column, _)| column)
        .collect())
}
