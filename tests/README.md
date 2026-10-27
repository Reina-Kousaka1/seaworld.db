# Test layout

The first integration suite is `src/test/java/org/seaworlddb/EngineTests.java` and runs through `scripts/test.ps1`. It covers file creation/validation, collections, document mutations, nested values, primary-ID lookups, malformed files, bounded lengths, path-name rejection, nesting limits, and incomplete-tail recovery.

The current suite simulates a truncated append both with a partial prefix and a complete block prefix whose body is cut off. It does not yet terminate a writer process at real write/force boundaries. Add a subprocess crash harness before making stronger crash-recovery claims or adding multi-operation transactions. Planned failure points are before append, during block write, after write but before force, and after force but before the operation returns.
