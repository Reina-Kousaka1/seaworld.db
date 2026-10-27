# Native query engine

The typed native query API is independent of SQL syntax. Any future SQL parser must lower to these same query nodes and call the same planner and executor.

## Paths and values

`FieldPath` parses dotted paths into non-empty object-key segments. It traverses objects only: arrays are treated as values and are not implicitly traversed. A missing segment differs from explicit `Value::Null`. Invalid/empty segments and excessive depth are query errors.

`Eq`/`Ne` compare only values of the same `Value` variant; mismatched types are unequal, so `Integer(5) != String("5")`. Ordered comparisons are false for missing values and incompatible types; they never coerce integers to floats or strings. Null only equals null. Float values are finite; `-0.0` and `0.0` compare equal.

The AST supports ID equality, field comparisons (`Eq`, `Ne`, `Lt`, `Lte`, `Gt`, `Gte`), `Exists`, `And`, `Or`, and `Not`. Filters are type values, not parsed strings. Depth and node counts are bounded.

## Planner and explain

The planner chooses a primary-ID lookup for an ID equality, a secondary-index lookup for an indexed equality predicate, an ordered range candidate lookup for a comparable indexed range predicate, or a full collection scan. Indexed predicates under `And` can provide candidates; other boolean shapes use a scan. Every candidate from an index is checked against the complete filter. `Explain` exposes the selected plan; it is diagnostic, not a promise of cost-based optimization.

Results are deterministic: absent an explicit sort, rows are in ascending document-ID order. `limit` and `offset` have explicit upper bounds. Sorting supports missing, null, boolean, integer, finite float, and string values, ordered by that type rank and then by value; unsupported Binary/Array/Object sort values return a query error. Equal sort keys always use ascending document ID as the tie-breaker, including descending sorts.

Index and scan paths are compared in differential tests. A query result must not depend on whether a secondary index exists.

## SQL V0 subset

The current starting subset implements collection creation, simple `SELECT`, `INSERT`, `UPDATE`, and `DELETE`, with field paths and one comparison predicate. `EXPLAIN SELECT` returns the shared native plan. The parser output is converted to the internal query model; SQL is never sent to another database. See [SQL V0](sql.md) for grammar, binding, and value semantics. This is not a promise of full SQL compatibility.

Document-to-SQL semantics must define:

- nested fields through dotted paths;
- arrays and whether a predicate targets the array as a whole or its elements;
- missing values versus explicit `NULL`;
- flexible schemas and fields with different types;
- identifier quoting and reserved words;
- comparison/collation and numeric coercion rules.

The implemented SQL rules are deliberately small: missing fields are distinct from null, nested paths traverse objects only, arrays remain values, and comparisons do not coerce across types. Unsupported SQL syntax returns a query error with a source byte offset when available.
