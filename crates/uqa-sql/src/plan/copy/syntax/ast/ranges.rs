//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Copying, Result, SyntaxCopy};

syntax_enum!(crate::ast::RangeAlignment { Integer, Double });

syntax_enum!(crate::ast::RangeSubtype {
    Integer,
    BigInteger,
    Numeric,
    Date,
    Timestamp,
    TimestampTz
});

syntax_struct!(crate::ast::CreateRange { name, options });
syntax_struct!(crate::ast::RangeOption {
    name,
    value,
    position
});
syntax_enum!(crate::ast::RangeOptionValue { Type(value0), Name(value0), Invalid });
