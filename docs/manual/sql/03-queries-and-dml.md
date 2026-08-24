# Queries and DML

UQA Engine executes relational SQL together with retrieval and graph relations. This chapter covers ordinary query blocks and mutations; ranked predicates are detailed in [Retrieval SQL](06-retrieval.md).

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

### FETCH WITH TIES

`FETCH FIRST count ROWS WITH TIES` takes the requested prefix from the available ordered rows and extends it through every following row whose complete `ORDER BY` key is equal to the last requested row. `OFFSET` is applied first, an omitted count defaults to one row, and a zero count returns no rows.

```sql
SELECT player_id, score
FROM leaderboard
ORDER BY score DESC
FETCH FIRST 10 ROWS WITH TIES;
```

`WITH TIES` requires `ORDER BY`; otherwise UQA Engine raises SQLSTATE `42601`. A NULL count raises SQLSTATE `2201W` instead of behaving like an unlimited `LIMIT`.

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

Duplicate or missing `USING` names, ambiguous common input names, and unsupported equality/common-type pairs are rejected with PostgreSQL SQLSTATEs before execution. Differently declared columns use the implemented PostgreSQL 18 common-type coercion matrix; collations, domains, user-defined equality operators, and the complete common-type matrix remain open.

An alias after a parenthesized join names the complete shaped output and hides every input relation and `USING` alias from the enclosing query level. Its optional column aliases apply positionally after `USING` or `NATURAL` has placed merged columns first, may rename only an initial prefix, and are visible to later joins and LATERAL sources.

```sql execute
SELECT joined.joined_id, joined.left_value, joined.right_value
FROM ((VALUES (1, 'left')) AS l(id, left_value)
JOIN (VALUES (1, 'right')) AS r(id, right_value)
USING (id)) AS joined(joined_id);
```

## LATERAL

A lateral subquery or table function can depend on preceding `FROM` items:

```sql
SELECT t.id, series.value
FROM tasks AS t,
LATERAL generate_series(1, t.repeat_count) AS series(value)
ORDER BY t.id, series.value;
```

The right side is evaluated for each left row. A table function followed by `WITH ORDINALITY` appends a one-based `BIGINT` column after the function's ordinary output columns; its default name is `ordinality`, a positional column-alias list may rename it, and a LATERAL invocation restarts it at one for each left row.

```sql execute
SELECT value, sequence
FROM generate_series(2, 4) WITH ORDINALITY AS series(value, sequence)
ORDER BY sequence;
```

`ROWS FROM` groups one or more table functions into one implicitly lateral source, concatenates their columns in declaration order, and zips their rows to the longest member with NULL padding; group-wide aliases and `WITH ORDINALITY` apply after that combined output is formed. See [General table functions](04-expressions-and-functions.md#general-table-functions) for member resolution, multi-array `unnest`, errors, and a runnable example.

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

UQA Engine expands `GROUPING SETS`, `ROLLUP`, and `CUBE`:

```sql
SELECT region, product, sum(amount) AS total
FROM sales
GROUP BY CUBE(region, product)
ORDER BY region, product;
```

`GROUP BY DISTINCT` removes duplicate grouping sets after expanding `GROUPING SETS`, `ROLLUP`, and `CUBE`; `GROUP BY ALL` retains their multiplicity. Grouping-set identity is computed after resolving aliases, column references, and no-op casts, ignores key order and repeated keys, and keeps expressions with different analyzed types or operators distinct.

```sql execute
SELECT region, sum(amount) AS total
FROM (VALUES ('us', 10), ('eu', 20)) AS sales(region, amount)
GROUP BY DISTINCT GROUPING SETS ((region), (region), ());
```

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

Implemented ranking and offset windows are `row_number`, `rank`, `dense_rank`, `lag`, `lead`, and `ntile`. Aggregate windows include `sum`, `count`, `avg`, `min`, and `max`. Frame syntax supports implemented `ROWS`, `RANGE`, and `GROUPS` boundaries. A named `WINDOW` clause can share a complete definition or extend an earlier frameless definition with a missing `ORDER BY` or frame; definitions are processed left to right, `PARTITION BY` is inherited but cannot be added or overridden by a referencing definition, an existing ordering cannot be overridden, and copying a definition that already has a frame is rejected as in PostgreSQL 18. A direct `OVER window_name` reference uses the named frame without copying it.

```sql execute
SELECT department, salary,
       row_number() OVER ranked AS position,
       sum(salary) OVER running AS running_payroll
FROM (VALUES ('engineering', 120), ('engineering', 100), ('sales', 90)) AS employees(department, salary)
WINDOW by_department AS (PARTITION BY department),
       ranked AS (by_department ORDER BY salary DESC),
       running AS (by_department ORDER BY salary ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW)
ORDER BY department, salary DESC;
```

## Row locking

`SELECT` accepts PostgreSQL 18 row-locking clauses.

1. Syntax: `FOR { UPDATE | NO KEY UPDATE | SHARE | KEY SHARE } [ OF relation [, ...] ] [ NOWAIT | SKIP LOCKED ]`. Multiple clauses may overlap: each relation uses the strongest requested lock, and its wait policy is `NOWAIT` if any applicable clause says `NOWAIT`, otherwise `SKIP LOCKED` if any says `SKIP LOCKED`, otherwise blocking.
2. Arguments: `OF` names are unqualified SQL relation or alias identifiers from the current `FROM` clause. An aliased item can be named only by its alias; an unaliased item accepts only its unqualified table name. Repeating a name is harmless, and one unqualified name can select multiple unaliased same-name relations from different schemas. Identifiers are not values and cannot be parameters. Omitting `OF` selects every lockable relation in the query block.
3. Result: the same rows the query would return without the clause. `_score` and other output columns are unchanged. Lock identity columns stay internal.
4. Effects: each returned tuple from a selected base table or lockable view/subquery is locked until the current transaction ends; rows skipped by `OFFSET` are locked as well. An outer clause cannot lock through a CTE, aggregate, window, distinct, or set-operation row-identity barrier, so place the clause inside the underlying query when that behavior is required. If a target tuple changes between the command-snapshot scan and lock acquisition, the current committed tuple is substituted and the query qualifications are rechecked while unchanged inputs remain on the original command snapshot; a pure lock wait does not admit later inserts. Autocommit statements take a statement transaction and release locks when that statement finishes. `UPDATE`, `DELETE`, and `MERGE`, including rows changed by referential actions, take `FOR NO KEY UPDATE` or `FOR UPDATE` locks on the rows they mutate. Locking queries also retain a relation lock, so operations such as `TRUNCATE` wait until the locking transaction ends. `ROLLBACK TO SAVEPOINT` removes row and relation acquisitions and strength upgrades made after that savepoint. Independent engines over the same durable database coordinate row and relation locks within one OS process and across OS processes; cross-process coordination uses a `<database>.uqa-locks` sidecar file created next to the database.
5. Errors: `DISTINCT`, `GROUP BY`, `HAVING`, aggregate functions, window functions, and directly locked set-operation queries reject the clause (`0A000`). An explicit `OF` target rejects a CTE reference or table function; an unqualified clause silently skips CTEs, table functions, and `VALUES` sources because they have no lockable rows. A top-level `VALUES` statement, a foreign table, a virtual catalog relation, and a lockable relation on the nullable side of an outer join reject the clause (`0A000`), while a `WHERE` qualification that cannot be true for the join's null-extended side reduces the outer join first and makes that side lockable. An `OF` name that is not visible in `FROM`, including a base name hidden by an alias, is `42P01`. `NOWAIT` that cannot lock immediately is `55P03`. A wait-for cycle is `40P01`. Cancellation during a wait is `57014`.
6. Example:

```sql execute
CREATE TABLE accounts (
    id INTEGER PRIMARY KEY,
    owner TEXT NOT NULL,
    balance INTEGER NOT NULL
);
INSERT INTO accounts (id, owner, balance) VALUES (1, 'ann', 100);
BEGIN;
SELECT id, balance
FROM accounts
WHERE id = 1
FOR UPDATE;
UPDATE accounts SET balance = balance - 10 WHERE id = 1;
COMMIT;
```

`SKIP LOCKED` skips rows that another session already holds incompatibly and continues so `ORDER BY ... LIMIT` can take the next unlocked row. `NOWAIT` fails instead of waiting. Same-session lock upgrades keep the stronger strength. `FOR KEY SHARE` does not conflict with a non-key `UPDATE`; it does conflict with `FOR UPDATE` and `DELETE`.

```sql
SELECT a.id, o.active
FROM accounts AS a
JOIN owners AS o ON o.name = a.owner
WHERE a.id = 1
FOR UPDATE OF a;
```

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
WHEN MATCHED AND target.quantity <> source.quantity THEN
    UPDATE SET quantity = source.quantity
WHEN NOT MATCHED BY TARGET THEN
    INSERT (item_id, quantity) VALUES (source.item_id, source.quantity)
WHEN NOT MATCHED BY SOURCE THEN
    DELETE
RETURNING WITH (OLD AS before, NEW AS after)
    merge_action() AS action,
    source.item_id AS source_item_id,
    before.quantity AS old_quantity,
    after.quantity AS new_quantity;
```

`MERGE` performs a full candidate join whenever both target-missing and source-missing clauses are present. Each candidate is classified exactly once as `MATCHED`, `NOT MATCHED BY SOURCE`, or `NOT MATCHED [BY TARGET]`, and clauses are tested in written order until the first condition succeeds. MATCHED conditions and UPDATE expressions can read source and target columns, `NOT MATCHED BY SOURCE` conditions and UPDATE expressions can read only target columns, and `NOT MATCHED [BY TARGET]` conditions and INSERT values can read only source columns.

MATCHED and `NOT MATCHED BY SOURCE` support `UPDATE`, `DELETE`, and `DO NOTHING`; `NOT MATCHED [BY TARGET]` supports `INSERT` and `DO NOTHING`. One source row may change multiple distinct target rows, but two selected mutation actions for one target row fail atomically with cardinality violation (`21000`). An unconditional clause is the last reachable clause of its candidate kind, so a later clause of that kind fails with syntax error (`42601`); a relation hidden by the candidate kind fails with undefined table (`42P01`); join and action conditions must be boolean (`42804`); and a MERGE requiring both source-missing and target-missing candidates requires at least one hash-joinable or merge-joinable equality in the join condition (`0A000`).

The command count and `RETURNING` rows include only inserted, updated, and deleted rows. `DO NOTHING` neither increments the count nor emits a row. `merge_action()` returns `INSERT`, `UPDATE`, or `DELETE`; source columns are NULL for source-missing candidates; the target qualifier denotes the new image for INSERT and UPDATE and the old image for DELETE; `OLD` and `NEW` or aliases declared by `WITH` expose both images explicitly. Unqualified `RETURNING *` emits source columns first and target columns second, while qualified stars select their named source, target, old-image, or new-image relation.

All selected actions are validated and staged before publication, including target-column names, candidate-kind visibility, unique and foreign-key constraints, generated columns, repeated-target cardinality, and referential actions, so a failed MERGE leaves no partial target mutation.

## Determinism and bags

SQL results are bags until an operation explicitly removes duplicates, and row order is unspecified without `ORDER BY`. A `LIMIT` without a complete ordering can select different tied rows after data, plan, or index changes. Use a stable unique key as the final ordering term for repeatable APIs and fixtures; omit that unique key intentionally when `FETCH ... WITH TIES` should return every peer at the boundary.
