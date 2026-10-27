//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Array constructors preserve the boundary between dimensions and array-valued domain elements.

use crate::{catalog::type_metadata::pg_type_oid, ColumnType, SQLError};
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    ArrayValue, Value,
};

/// Form ARRAY expression output from already selected scalar element metadata. Domain input has already run in each evaluated expression; retaining an array-valued scalar must not run it again.
pub fn construct_array_with_control(
    count: usize,
    input_type: Option<&ColumnType>,
    control: &ProductionControl<'_>,
    mut evaluate: impl FnMut(usize) -> Result<Produced<Value>, SQLError>,
) -> Result<Produced<Value>, SQLError> {
    let element_type = input_type.map(|ty| ty.array_element_type().unwrap_or(ty));
    let mut values = ProductionVec::new(*control);
    values.reserve(count)?;
    for index in 0..count {
        values.push_produced(evaluate(index)?)?;
    }
    let values = values.finish()?;
    let array = if count == 0 || input_type.is_none_or(|ty| matches!(ty, ColumnType::Array(_))) {
        ArrayValue::try_new_with_control(values, control)?
    } else {
        let mut bounds = ProductionVec::new(*control);
        bounds.push_copy(1)?;
        ArrayValue::with_lower_bounds_with_control(values, bounds.finish()?, control)?
    }
    .ok_or_else(|| {
        SQLError::TypeMismatch("multidimensional arrays must have matching dimensions".into())
    })?;

    let (array, memory) = array.into_parts();
    Ok(control.finish(
        Value::Array(array.with_element_type_oid(element_type.map(|ty| pg_type_oid(ty) as u32))),
        memory,
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use uqa_core::{memory::MemoryBudget, CancellationToken};

    #[test]
    fn declared_scalar_arrays_survive_nested_construction_and_native_output() {
        let domain = ColumnType::Domain {
            collation_oid: None,
            schema: "public".into(),
            name: "array_leaf".into(),
            oid: 20_000,
            array_oid: Some(20_001),
            base: Box::new(ColumnType::Array(Box::new(ColumnType::Integer))),
        };
        let budget = MemoryBudget::new(16_384);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let children = [
            Value::Array(ArrayValue::try_new(vec![Value::Int(1), Value::Int(2)]).unwrap()),
            Value::Array(ArrayValue::try_new(vec![]).unwrap()),
        ];
        let first = construct_array_with_control(2, Some(&domain), &control, |index| {
            Ok(control.copy_value(&children[index])?)
        })
        .unwrap();
        let second = construct_array_with_control(2, Some(&domain), &control, |_| {
            Ok(control.finish(Value::Null, control.empty_reservation())?)
        })
        .unwrap();
        let ty = ColumnType::Array(Box::new(domain));
        let mut rows = [first, second].into_iter();
        let value =
            construct_array_with_control(2, Some(&ty), &control, |_| Ok(rows.next().unwrap()))
                .unwrap();
        let Value::Array(array) = &*value else {
            panic!("array")
        };
        assert_eq!(array.dimensions(), [2, 2]);
        let text =
            crate::result::format_postgres_text_with_control(&value, &ty, None, &control).unwrap();
        assert_eq!(&*text, "{{\"{1,2}\",\"{}\"},{NULL,NULL}}");
        assert_eq!(
            budget.used(),
            value.reserved_bytes() + text.reserved_bytes()
        );
        drop((value, text));
        assert_eq!(budget.used(), 0);
    }
}
