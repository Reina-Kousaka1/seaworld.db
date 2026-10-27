# Query engine and SQL compatibility

The query engine is independent of SQL syntax. Native API calls and SQL statements will lower into one SeaWorld query representation and use the same executor and indexes.

An initial filter model can express paths, literal values, comparison operators, and boolean composition:

```text
Filter(
    field = "profile.career",
    operator = Equals,
    value = "ballet"
)
```

The first implementation only has primary-ID lookup and direct document access. It does not yet parse or execute general filters.

## Planned query representation

- `Path`: validated list of object field segments, not an unchecked SQL string
- `Literal`: one document `Value`
- `Compare`: equality and ordered comparisons with explicit type rules
- `And`, `Or`, `Not`: boolean composition
- `Projection`: selected paths or all fields
- `Insert`, `Update`, `Delete`: collection operations over the same document representation

The planner may use a primary-ID map, a single-field index, or a collection scan. Correctness must not depend on which access path the planner selects. Index results will be checked against scan results in tests.

## SQL subset (future)

The intended starting subset is collection creation, simple `SELECT`, `INSERT`, `UPDATE`, and `DELETE`, with field paths and basic predicates. This is not a promise of full SQL compatibility. Parser output is converted to the internal query model; SQL is never sent to another database.

Document-to-SQL semantics must define:

- nested fields through dotted paths;
- arrays and whether a predicate targets the array as a whole or its elements;
- missing values versus explicit `NULL`;
- flexible schemas and fields with different types;
- identifier quoting and reserved words;
- comparison/collation and numeric coercion rules.

Until those rules and an executable parser exist, SQL support is not implemented.
