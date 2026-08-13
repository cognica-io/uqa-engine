# Queries and DML

UQA-RS executes relational SQL together with retrieval and graph relations. This chapter covers ordinary query blocks and mutations; ranked predicates are detailed in [Retrieval SQL](06-retrieval.md).

## SELECT pipeline

A query block can contain projection, `FROM`, `WHERE`, grouping, `HAVING`, window expressions, distinct processing, ordering, offset, and limit.

```sql
SELECT department, count(*) AS employees, avg(salary) AS average_salary
FROM employees
WHERE active
GROUP BY department
HAVING count(*) >= 2
ORDER BY average_salary DESC NULLS LAST, department ASC
LIMIT 20 OFFSET 0;
```

Ascending order defaults to NULLS LAST and descending order defaults to NULLS FIRST. Use explicit `NULLS FIRST` or `NULLS LAST` when portability matters.

## DISTINCT and DISTINCT ON

```sql
SELECT DISTINCT department
FROM employees
ORDER BY department;

SELECT DISTINCT ON (department)
    department, employee_id, salary
FROM employees
ORDER BY department, salary DESC, employee_id ASC;
```

`DISTINCT ON` keeps the first row under the supplied ordering for each key. Make the order complete and deterministic.

## Joins

Implemented join forms are inner, left outer, right outer, full outer, and cross joins.

```sql
SELECT e.employee_id, e.name, d.name AS department
FROM employees AS e
LEFT JOIN departments AS d ON d.department_id = e.department_id
ORDER BY e.employee_id;
```

Qualified joins accept `ON`, `USING (column, ...) [AS alias]`, or `NATURAL`, with exactly one qualification. `USING` emits the merged columns in list order, then the remaining left columns, then the remaining right columns; left, right, and full outer joins choose the merged value with PostgreSQL 18 semantics. The optional `USING` alias names only the merged columns and does not hide either input alias. `NATURAL` derives the `USING` list from common visible names in left-input order and becomes a cross join when there are no common names.

```sql
SELECT merged.id, l.left_value, r.right_value
FROM (VALUES (1, 'left')) AS l(id, left_value)
FULL JOIN (VALUES (1, 'right')) AS r(id, right_value)
USING (id) AS merged;
```

Duplicate or missing `USING` names and ambiguous common input names are rejected with PostgreSQL SQLSTATEs. Static equality-operator resolution and common-type coercion for `USING` columns with different declared types remain an open PostgreSQL 18 compatibility bug. Parenthesized join aliases are also outside the supported surface.

## LATERAL

A lateral subquery or table function can depend on preceding `FROM` items:

```sql
SELECT t.id, series.value
FROM tasks AS t,
LATERAL generate_series(1, t.repeat_count) AS series(value)
ORDER BY t.id, series.value;
```

The right side is evaluated for each left row. Multiple functions inside one `ROWS FROM` item and `WITH ORDINALITY` are not implemented.

## Subqueries

Supported expression and relation subqueries include scalar subqueries, correlated subqueries, `EXISTS`, `IN`, and derived tables.

```sql
SELECT e.employee_id, e.name
FROM employees AS e
WHERE e.salary > (
    SELECT avg(peer.salary)
    FROM employees AS peer
    WHERE peer.department_id = e.department_id
)
ORDER BY e.employee_id;
```

A scalar subquery must return at most one row and one value. Use `EXISTS` when only row existence matters.

## Common table expressions

```sql
WITH active AS (
    SELECT employee_id, department_id, salary
    FROM employees
    WHERE active
)
SELECT department_id, sum(salary) AS payroll
FROM active
GROUP BY department_id;
```

Recursive CTEs are implemented:

```sql
WITH RECURSIVE numbers(n) AS (
    VALUES (1)
    UNION ALL
    SELECT n + 1 FROM numbers WHERE n < 5
)
SELECT n FROM numbers ORDER BY n;
```

Recursive `SEARCH` and `CYCLE` clauses and `NOT MATERIALIZED` are not implemented.

## Set operations

`UNION`, `INTERSECT`, and `EXCEPT` support distinct and `ALL` forms where specified:

```sql
SELECT id FROM current_items
UNION ALL
SELECT id FROM archived_items;
```

Operands must have compatible column counts and coercible types. Apply final ordering and limiting to the combined query when result order matters.

## Grouping sets

UQA-RS expands `GROUPING SETS`, `ROLLUP`, and `CUBE`:

```sql
SELECT region, product, sum(amount) AS total
FROM sales
GROUP BY CUBE(region, product)
ORDER BY region, product;
```

`GROUP BY DISTINCT` is not implemented.

## Window functions

```sql
SELECT employee_id, department_id, salary,
       row_number() OVER (
           PARTITION BY department_id
           ORDER BY salary DESC, employee_id
       ) AS position,
       sum(salary) OVER (
           PARTITION BY department_id
           ORDER BY employee_id
           ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW
       ) AS running_payroll
FROM employees;
```

Implemented ranking and offset windows are `row_number`, `rank`, `dense_rank`, `lag`, `lead`, and `ntile`. Aggregate windows include `sum`, `count`, `avg`, `min`, and `max`. Frame syntax supports implemented `ROWS`, `RANGE`, and `GROUPS` boundaries. Named `WINDOW` clauses are not implemented; place the definition directly in each `OVER` expression.

## VALUES

`VALUES` can be a top-level relation, a CTE body, or an insert source:

```sql
SELECT *
FROM (VALUES (1, 'open'), (2, 'closed')) AS states(id, label)
ORDER BY id;
```

## INSERT

```sql
INSERT INTO tasks (task_id, title, state)
VALUES (1, 'write manual', 'open'),
       (2, 'verify links', 'open')
RETURNING task_id, state;
```

Insert can use literal values or a query source. Defaults, generated serial values, constraints, indexes, and referential actions are updated in the same mutation boundary.

## UPDATE

```sql
UPDATE tasks
SET state = 'closed'
WHERE task_id = 1
RETURNING task_id, state;
```

`UPDATE ... FROM` is implemented:

```sql
UPDATE accounts AS a
SET balance = a.balance + b.amount
FROM bonuses AS b
WHERE b.account_id = a.account_id
RETURNING a.account_id, a.balance;
```

Assignments can use expressions and scalar subqueries. Existing rows are validated against the resulting schema and constraints before publication.

## DELETE

```sql
DELETE FROM tasks
WHERE state = 'closed'
RETURNING task_id;
```

`DELETE ... USING` is implemented:

```sql
DELETE FROM sessions AS s
USING expired_users AS e
WHERE e.user_id = s.user_id;
```

Foreign-key actions can update or delete related rows as part of the same transaction.

## MERGE

```sql
MERGE INTO inventory AS target
USING incoming AS source
ON target.item_id = source.item_id
WHEN MATCHED THEN
    UPDATE SET quantity = source.quantity
WHEN NOT MATCHED THEN
    INSERT (item_id, quantity) VALUES (source.item_id, source.quantity);
```

Implemented matched actions are `UPDATE`, `DELETE`, and `DO NOTHING`. Implemented not-matched actions are `INSERT` and `DO NOTHING`. `WHEN NOT MATCHED BY SOURCE` is not implemented.

## Determinism and bags

SQL results are bags until an operation explicitly removes duplicates, and row order is unspecified without `ORDER BY`. A `LIMIT` without a complete ordering can select different tied rows after data, plan, or index changes. Use a stable unique key as the final ordering term for repeatable APIs and fixtures.
