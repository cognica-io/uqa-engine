//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Correlation namespaces retain admitted names, ordered columns and map nodes until their lexical scope ends.

use super::{CorrelationContext, SQLError};
use crate::{ast::InternalRelationId, production::Buffer, RowSchema};
use std::sync::Arc;
use uqa_core::memory::{OwnedMap, OwnedSet, Produced, ProductionControl, ProductionVec};

#[derive(Clone, Copy)]
pub(super) struct Analysis<'a, 'c> {
    pub context: CorrelationContext<'a>,
    pub control: ProductionControl<'c>,
}

impl<'a> std::ops::Deref for Analysis<'a, '_> {
    type Target = CorrelationContext<'a>;
    fn deref(&self) -> &Self::Target {
        &self.context
    }
}

pub(super) type Columns = Arc<Produced<RelationColumns>>;
pub(super) type Ctes = OwnedMap<Produced<String>, Columns>;

pub(super) struct RelationColumns {
    pub names: OwnedSet<Produced<String>>,
    pub ordered: Produced<Vec<String>>,
    pub complete: bool,
}

impl RelationColumns {
    pub fn known(
        ordered: Produced<Vec<String>>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let mut names = OwnedSet::new();
        for name in ordered.iter() {
            insert_set(&mut names, control.copy_text(name)?, control)?;
        }
        Ok(Self {
            names,
            ordered,
            complete: true,
        })
    }

    pub fn empty(complete: bool, control: &ProductionControl<'_>) -> Result<Self, SQLError> {
        Ok(Self {
            names: OwnedSet::new(),
            ordered: control.finish(Vec::new(), control.empty_reservation())?,
            complete,
        })
    }

    pub fn apply_positional_aliases(
        self,
        aliases: &[String],
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        if aliases.is_empty() {
            control.check()?;
            return Ok(self);
        }
        let names = copy_names(
            self.ordered
                .iter()
                .enumerate()
                .map(|(position, name)| aliases.get(position).unwrap_or(name).as_str()),
            control,
        )?;
        let mut columns = Self::known(names, control)?;
        columns.complete = self.complete;
        Ok(columns)
    }

    pub fn share(self, control: &ProductionControl<'_>) -> Result<Columns, SQLError> {
        Ok(control
            .finish(self, control.empty_reservation())?
            .into_shared(control)?)
    }

    pub fn copy_with_control(&self, control: &ProductionControl<'_>) -> Result<Self, SQLError> {
        let mut names = OwnedSet::new();
        for name in &self.names {
            insert_set(&mut names, control.copy_text(name)?, control)?;
        }
        Ok(Self {
            names,
            ordered: copy_names(self.ordered.iter().map(String::as_str), control)?,
            complete: self.complete,
        })
    }
}

pub(super) struct QueryScope {
    pub qualifiers: OwnedMap<Produced<String>, Columns>,
    pub internal_relations: OwnedSet<InternalRelationId>,
    pub columns: Columns,
    pub enclosing: Option<RowSchema>,
}

impl QueryScope {
    pub fn relation(
        qualifier: Option<&str>,
        columns: RelationColumns,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let columns = columns.share(control)?;
        let mut qualifiers = OwnedMap::new();
        if let Some(name) = qualifier {
            insert(
                &mut qualifiers,
                control.copy_text(name)?,
                Arc::clone(&columns),
                control,
            )?;
        }
        Ok(Self {
            qualifiers,
            internal_relations: OwnedSet::new(),
            columns,
            enclosing: None,
        })
    }

    pub fn empty(control: &ProductionControl<'_>) -> Result<Self, SQLError> {
        Self::relation(None, RelationColumns::empty(true, control)?, control)
    }

    pub fn copy_with_control(&self, control: &ProductionControl<'_>) -> Result<Self, SQLError> {
        let mut qualifiers = OwnedMap::new();
        for (name, columns) in &self.qualifiers {
            insert(
                &mut qualifiers,
                control.copy_text(name)?,
                Arc::clone(columns),
                control,
            )?;
        }
        let mut internal_relations = OwnedSet::new();
        for relation in &self.internal_relations {
            insert_set(&mut internal_relations, *relation, control)?;
        }
        Ok(Self {
            qualifiers,
            internal_relations,
            columns: Arc::clone(&self.columns),
            enclosing: self.enclosing.clone(),
        })
    }
}

pub(super) fn copy_names<'a>(
    names: impl IntoIterator<Item = &'a str>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<String>>, SQLError> {
    let mut output = ProductionVec::new(*control);
    for name in names {
        output.push_produced(control.copy_text(name)?)?;
    }
    Ok(output.finish()?)
}

pub(super) fn copy_scopes(
    scopes: &[QueryScope],
    control: &ProductionControl<'_>,
) -> Result<Buffer<QueryScope>, SQLError> {
    let mut output = Buffer::new(control);
    extend_scopes(&mut output, scopes, control)?;
    Ok(output)
}

pub(super) fn extend_scopes(
    output: &mut Buffer<QueryScope>,
    scopes: &[QueryScope],
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    for scope in scopes {
        output.push(scope.copy_with_control(control)?, control)?;
    }
    Ok(())
}

pub(super) fn copy_ctes(ctes: &Ctes, control: &ProductionControl<'_>) -> Result<Ctes, SQLError> {
    let mut output = OwnedMap::new();
    for (name, columns) in ctes {
        insert(
            &mut output,
            control.copy_text(name)?,
            Arc::clone(columns),
            control,
        )?;
    }
    Ok(output)
}

pub(super) fn insert<K: Ord, V>(
    map: &mut OwnedMap<K, V>,
    key: K,
    value: V,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    match control.budget() {
        Some(budget) => {
            map.insert_budgeted(key, value, budget)?;
        }
        None => {
            map.insert(key, value);
        }
    }
    Ok(())
}

pub(super) fn insert_set<T: Ord>(
    set: &mut OwnedSet<T>,
    value: T,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    match control.budget() {
        Some(budget) => {
            set.insert_budgeted(value, budget)?;
        }
        None => {
            set.insert(value);
        }
    }
    Ok(())
}
