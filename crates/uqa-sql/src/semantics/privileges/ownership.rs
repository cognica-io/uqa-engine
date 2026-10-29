//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Column lineage owns each name, ordered node and output buffer independently.

use crate::SQLError;
use uqa_core::memory::{
    MemoryReservation, OwnedMap, OwnedSet, Produced, ProductionControl, ProductionVec,
};

pub(super) type Names = OwnedSet<Produced<String>>;
pub(super) type Required = OwnedSet<BaseColumn>;
pub(super) type Named<T> = OwnedMap<Produced<String>, T>;

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct BaseColumn {
    pub table: Produced<String>,
    pub column: Produced<String>,
}

pub(super) fn require(
    required: &mut Required,
    table: &str,
    column: &str,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    if !required.contains_by(|key| (table, column).cmp(&(key.table.as_str(), key.column.as_str())))
    {
        insert_set(
            required,
            BaseColumn {
                table: control.copy_text(table)?,
                column: control.copy_text(column)?,
            },
            control,
        )?;
    }
    Ok(())
}

pub(super) fn insert_set<T: Ord>(
    output: &mut OwnedSet<T>,
    value: T,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    match control.budget() {
        Some(budget) => {
            output.insert_budgeted(value, budget)?;
        }
        None => {
            output.insert(value);
        }
    }
    Ok(())
}

pub(super) fn insert<T>(
    output: &mut Named<T>,
    name: &str,
    value: T,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    if let Some(previous) = output.get_mut(name) {
        *previous = value;
    } else {
        let name = control.copy_text(name)?;
        match control.budget() {
            Some(budget) => {
                output.insert_budgeted(name, value, budget)?;
            }
            None => {
                output.insert(name, value);
            }
        }
    }
    Ok(())
}

pub(super) fn copy_names<'a>(
    names: impl IntoIterator<Item = &'a str>,
    control: &ProductionControl<'_>,
) -> Result<Names, SQLError> {
    let mut output = Names::new();
    extend_names(&mut output, names, control)?;
    Ok(output)
}

fn extend_names<'a>(
    output: &mut Names,
    names: impl IntoIterator<Item = &'a str>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    for name in names {
        control.check()?;
        if !output.contains(name) {
            insert_set(output, control.copy_text(name)?, control)?;
        }
    }
    Ok(())
}

pub(super) fn extend_required(
    output: &mut Required,
    input: &Required,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    for column in input {
        require(output, &column.table, &column.column, control)?;
    }
    Ok(())
}

pub(super) struct OutputColumn {
    pub name: Produced<String>,
    pub sources: Required,
}

impl OutputColumn {
    pub fn copy(&self, control: &ProductionControl<'_>) -> Result<Self, SQLError> {
        let name = control.copy_text(&self.name)?;
        let mut sources = Required::new();
        extend_required(&mut sources, &self.sources, control)?;
        Ok(Self { name, sources })
    }
}

#[derive(Default)]
pub(super) struct Columns {
    values: Vec<OutputColumn>,
    _memory: Option<MemoryReservation>,
}

impl Columns {
    pub fn collect(
        columns: impl IntoIterator<Item = Result<OutputColumn, SQLError>>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let mut output = ProductionVec::new(*control);
        for column in columns {
            output.push_produced(control.finish(column?, control.empty_reservation())?)?;
        }
        Ok(Self::from_produced(output.finish()?))
    }

    pub fn from_produced(columns: Produced<Vec<OutputColumn>>) -> Self {
        let (values, memory) = columns.into_parts();
        Self {
            values,
            _memory: memory,
        }
    }

    pub fn copy(&self, control: &ProductionControl<'_>) -> Result<Self, SQLError> {
        Self::collect(self.iter().map(|column| column.copy(control)), control)
    }

    pub fn rename(
        &mut self,
        aliases: &[String],
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        for (column, alias) in self.values.iter_mut().zip(aliases) {
            column.name = control.copy_text(alias)?;
        }
        Ok(())
    }
}

impl std::ops::Deref for Columns {
    type Target = [OutputColumn];
    fn deref(&self) -> &Self::Target {
        &self.values
    }
}

#[derive(Default)]
pub(super) struct SourceLineage {
    pub output: Columns,
    pub qualifiers: Named<Columns>,
    pub system_qualifiers: Named<Names>,
    pub tables: Named<Names>,
}

impl SourceLineage {
    pub fn has_unqualified(&self, column: &str) -> bool {
        self.output
            .iter()
            .any(|output| output.name.as_str() == column)
    }
    pub fn has_qualifier(&self, qualifier: &str) -> bool {
        self.qualifiers.contains_key(qualifier) || self.system_qualifiers.contains_key(qualifier)
    }
    pub fn require_unqualified(
        &self,
        column: &str,
        required: &mut Required,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        for output in self
            .output
            .iter()
            .filter(|output| output.name.as_str() == column)
        {
            extend_required(required, &output.sources, control)?;
        }
        Ok(())
    }
    pub fn require_qualified(
        &self,
        qualifier: &str,
        column: &str,
        required: &mut Required,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        if let Some(columns) = self.qualifiers.get(qualifier) {
            for output in columns
                .iter()
                .filter(|output| output.name.as_str() == column)
            {
                extend_required(required, &output.sources, control)?;
            }
        }
        Ok(())
    }
    pub fn require_all(
        &self,
        required: &mut Required,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        for output in self.output.iter() {
            extend_required(required, &output.sources, control)?;
        }
        Ok(())
    }
    pub fn require_qualified_all(
        &self,
        qualifier: &str,
        required: &mut Required,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        if let Some(columns) = self.qualifiers.get(qualifier) {
            for output in columns.iter() {
                extend_required(required, &output.sources, control)?;
            }
        }
        Ok(())
    }
    pub fn require_unqualified_system(
        &self,
        column: &str,
        required: &mut Required,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        for table in self.system_qualifiers.values().flatten() {
            require(required, table, column, control)?;
        }
        Ok(())
    }
    pub fn require_qualified_system(
        &self,
        qualifier: &str,
        column: &str,
        required: &mut Required,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        if let Some(tables) = self.system_qualifiers.get(qualifier) {
            for table in tables {
                require(required, table, column, control)?;
            }
        }
        Ok(())
    }
    pub fn include_tables(
        &mut self,
        other: &Self,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        for (table, columns) in &other.tables {
            let names = columns.iter().map(|name| name.as_str());
            if let Some(output) = self.tables.get_mut(table.as_str()) {
                extend_names(output, names, control)?;
            } else {
                insert(
                    &mut self.tables,
                    table,
                    copy_names(names, control)?,
                    control,
                )?;
            }
        }
        Ok(())
    }
    pub fn copy(&self, control: &ProductionControl<'_>) -> Result<Self, SQLError> {
        let mut output = Self {
            output: self.output.copy(control)?,
            ..Self::default()
        };
        for (name, columns) in &self.qualifiers {
            insert(
                &mut output.qualifiers,
                name,
                columns.copy(control)?,
                control,
            )?;
        }
        for (name, tables) in &self.system_qualifiers {
            insert(
                &mut output.system_qualifiers,
                name,
                copy_names(tables.iter().map(|name| name.as_str()), control)?,
                control,
            )?;
        }
        output.include_tables(self, control)?;
        Ok(output)
    }
}

pub(super) fn scopes<'a, 'b>(
    first: &'a SourceLineage,
    outer: &[&'a SourceLineage],
    control: ProductionControl<'b>,
) -> Result<ProductionVec<'b, &'a SourceLineage>, SQLError> {
    let mut scopes = ProductionVec::new(control);
    scopes.push_copy(first)?;
    scopes.extend_from_slice(outer)?;
    Ok(scopes)
}
