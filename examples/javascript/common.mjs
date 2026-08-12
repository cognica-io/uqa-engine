//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

export function assertEqual(actual, expected, label) {
  const actualJSON = JSON.stringify(canonical(actual));
  const expectedJSON = JSON.stringify(canonical(expected));
  if (actualJSON !== expectedJSON) {
    throw new Error(`${label} returned ${actualJSON}, expected ${expectedJSON}`);
  }
}

function canonical(value) {
  if (Array.isArray(value)) {
    return value.map(canonical);
  }
  if (value !== null && typeof value === "object") {
    return Object.fromEntries(
      Object.keys(value)
        .sort()
        .map((key) => [key, canonical(value[key])]),
    );
  }
  return value;
}
