# SeaWorld SQL V0

SeaWorld SQL is a bounded access language over the document-native engine. `Database::execute_sql` lexes and parses a statement into typed operations, then uses the existing `Query`, planner, and transaction paths. SQL is not the storage model. No SQL text is forwarded to another database.

## Supported statements

```sql
CREATE COLLECTION users;
INSERT INTO users VALUES {"username": "Charlotte", "credits": 1250,
  "profile": {"level": 17, "career": "ballet"},
  "inventory": ["pink_leotard", "ballet_shoes"]};
SELECT * FROM users;
SELECT username, profile.level FROM users WHERE profile.level >= 10;
UPDATE users SET credits = 1500, profile.level = 18 WHERE username = 'Charlotte';
DELETE FROM users WHERE credits < 10;
EXPLAIN SELECT username FROM users WHERE credits >= ?;
```

Only one comparison predicate is supported per `WHERE`. The operators are `=`, `!=`, `<>`, `<`, `<=`, `>`, and `>=`. `UPDATE` and `DELETE` require a predicate, which avoids accidental unqualified mutations in this initial subset. `INSERT` accepts an object literal; values may be null, boolean, signed 64-bit integer, finite float, string, nested object, array, or `?` parameter. SQL does not yet support DDL for indexes, joins, grouping, ordering, limits, aliases, subqueries, functions, comments, or multiple statements.

## Document paths and value semantics

Dot paths such as `profile.level` traverse nested objects only. They do not traverse array elements. A projected path is returned as a single result-document field named by its dotted path. Missing projected fields are omitted. A comparison with `NULL` matches only explicit null values; a missing field does not equal null, and `field != value` matches a missing field under the current native filter semantics. No cross-type coercion is performed. Ordered comparisons only match comparable values of the same supported type.

Arrays are preserved as document values but are not expanded into rows or treated as multi-valued indexes. Invalid traversal through a scalar during an UPDATE returns a query error.

## Binding and bounds

`?` placeholders are bound in left-to-right order through the `parameters` slice passed to `execute_sql`. A missing or extra parameter is an error. Applications should bind values rather than concatenate untrusted input into SQL text. The parser caps input at 64 KiB, 4,096 tokens, 16 KiB per string token, 64 nested value levels, and bounded collection/projection/assignment counts. Diagnostics include the byte offset for lexical and grammar errors. The parser accepts one statement per call.

Prepared statement objects and named parameters are not implemented yet. The current positional API provides safe value binding for the supported literal positions.

## Planner and current limits

`EXPLAIN SELECT` returns the same native `QueryPlan` used by `Database::explain`; indexed candidates are rechecked by the shared executor. This SQL subset does not define full relational or standards compatibility and carries no compatibility promise. See the [roadmap](roadmap.md) for omitted SQL and shell work.
