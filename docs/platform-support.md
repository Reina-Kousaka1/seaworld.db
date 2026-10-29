# Platform and durability qualification

This is the evidence-based platform contract for the embedded Rust engine. It
is not a production certification or a hardware compatibility list.

## CI-qualified platforms

The repository's required CI matrix builds, formats, lints, runs the full test
suite (including generated and process-crash tests), and creates a release build
on:

| Platform | Qualification currently evidenced | Not established by this evidence |
| --- | --- | --- |
| Ubuntu 24.04 x86_64 | Build and engine behavior in the hosted CI runner | Every Linux distribution, filesystem, kernel, mount option, or power-loss behavior |
| Windows latest x86_64 | Build and engine behavior in the hosted CI runner | Every Windows release, filesystem, network share, antivirus interaction, or power-loss behavior |

The CI matrix is SeaWorld's currently supported V1 qualification baseline.
Linux ARM64, Raspberry Pi, macOS, network filesystems, removable storage, and
container volume drivers are not V1-qualified merely because Rust can target
them. They need a separate build, crash, locking, and filesystem assessment.

## File-system contract

SeaWorld relies on its cooperating file locks to exclude a second writer and to
keep a reader from replaying during append or recovery. It uses same-directory
temporary files and no-overwrite hard-link publication for compacted copies,
backup, and restore targets. The destination filesystem must support that hard
link operation. External processes that ignore SeaWorld's locks can still race
or modify a database; this is not supported.

`File::sync_all` is invoked before a commit reports success, and the engine
syncs a parent directory where the Rust standard library supports it. This is a
process-crash recovery boundary, not a universal physical-durability guarantee.
Successful tests establish that incomplete transaction tails are handled as
documented and complete corrupt records fail closed. They do not establish that
the operating system, filesystem, storage controller, virtual machine, or drive
honors every flush through OS crash or power loss.

## Commit acknowledgement and recovery

After a successful commit acknowledgement, SeaWorld has written a complete
transaction marker and received a successful `sync_all` result. A process crash
before acknowledgement can leave either the old state or a complete committed
transaction after reopen; callers must treat a sync error as outcome-ambiguous
and reopen. A cleanly acknowledged commit is recovered after the process-kill
scenarios covered by CI. SeaWorld does **not** claim survival of a power cut,
controller write-cache loss, a lying device, or arbitrary filesystem semantics.

Backup, restore, and compaction use the same qualified publication assumptions.
They never overwrite an existing target. A kill before publication can leave a
temporary sibling file; a directory-sync failure after publication can leave a
complete target whose final durability is uncertain. See [backup and
restore](backup.md) and [transactions](transactions.md) for exact operation
semantics.
