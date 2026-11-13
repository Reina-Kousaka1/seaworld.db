# SeaWorld logical snapshot export

The embedded SeaWorld library exposes Database::begin_snapshot_export() for
V3/V4 databases. It pins one in-process MVCC commit version and provides a
complete sorted inventory of logical collections, bounded key pages, record
and collection SHA-256 hashes, encoded-size totals, and a database inventory
digest. The snapshot ID is derived from the database identity and committed
version, so repeated exports at the same database epoch identify the same
logical view. is_current() detects whether that live handle has advanced.

The scope is all logical collections, including empty collections. Records
deleted before the pinned version are absent; records deleted after it remain
visible through the lease. Storage indexes and WAL tombstone frames are not
exported. V1/V2 databases are rejected because they do not provide the MVCC
snapshot guarantee required by this API. Snapshot leases are process-local and
bounded by SeaWorld's existing MVCC lease/history limits; they are not durable
across restart.

This is a library API, not a network protocol, daemon endpoint, or offline CLI
export command. It does not connect to Molly's configured SeaWorld socket
adapter. No SeaWorld data was accessed by the tests; tests construct disposable
synthetic databases only. Before any operational export, a separately reviewed
transport and a verified writer-stop protocol are still required. A Molly
process-local write gate cannot stop external clients.

The hashes are integrity checks over the documented logical encoding, not
signatures or proof of ownership. Importers must still validate every
collection name, record schema, guild identity, and domain policy. This API
does not authorize migration of Molly's virtual-currency data.
