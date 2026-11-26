# 3. Server implementation guide

How to add the sync layer to a backend, in any language, on PostgreSQL,
MySQL/MariaDB or MongoDB. The protocol in
[02-protocol.md](02-protocol.md) says *what* a server must do; this guide
shows one proven way to do it. A server that passes the conformance tests is
correct even if it is built differently.

## 3.1 The four jobs of a server

1. Check the token and work out which scopes the user may sync.
2. Give every change to synced data a number, in commit order, per scope.
3. Apply a batch of pushed changes, each exactly once.
4. Return the records changed after a cursor.

Everything else (retry, batching, ordering, conflict handling on the device)
is the client's job.

## 3.2 Storage

Three small structures are added next to the project's own data. The
project's tables or collections do not change.

| Structure | One row per | Fields | Purpose |
| --- | --- | --- | --- |
| `sync_scopes` | scope | `scope_id` (key), `seq`, `min_seq` | The change counter, and the oldest cursor still answerable |
| `sync_index` | synced record | `scope_id`, `tbl`, `record_id` (key together), `seq`, `deleted`, `changed_at` | Which records changed and when, including tombstones |
| `sync_seen` | pushed change | `change_id` (key), `scope_id`, `status`, `version`, `reason`, `code`, `message`, `created_at` | Duplicate detection |

Indexes needed on `sync_index`:

- unique on (`scope_id`, `seq`): this is what pull reads
- on (`tbl`, `record_id`): to find which scope a record belongs to

`sync_index` holds one row per record, not one per change. When a record
changes again its row moves to the new `seq`. The table therefore stays the
size of the data, and a pull never returns the same record twice in a page.

A tombstone is an index row with `deleted = true`. The project's own row can
be physically deleted; the index remembers it.

## 3.3 The write path

This is the heart of the server. **Every** change to a synced record runs
the following inside one database transaction, whether it comes from a
device, an admin panel, an import or a background job:

```
function sync_write(scope_id, table, record_id, is_delete):
    seq = next_seq(scope_id)           # locks the scope until commit
    write or delete the project's own row
    upsert sync_index (scope_id, table, record_id)
        set seq = seq, deleted = is_delete, changed_at = now
    return seq
```

`next_seq` increments the scope's counter and keeps the counter row locked
until the transaction commits. That lock is the whole trick: a second writer
in the same scope waits, so numbers become visible in exactly the order they
were handed out.

### Why not auto-increment or timestamps

With an ordinary sequence, transaction A can take number 5 and transaction B
number 6, and B can commit first. A device that pulls at that moment sees 6,
stores a cursor of 6, and never asks for 5 again. The change is lost on that
device with no error anywhere. Timestamps have the same problem, plus clock
skew and ties. This bug only appears under concurrent load, so it survives
testing. The conformance suite has a test aimed at it (C-PULL-06).

### `next_seq` per database

PostgreSQL:

```sql
INSERT INTO sync_scopes (scope_id, seq, min_seq) VALUES ($1, 1, 0)
ON CONFLICT (scope_id) DO UPDATE SET seq = sync_scopes.seq + 1
RETURNING seq;
```

MySQL and MariaDB (InnoDB), with the scope row created when the scope is
created:

```sql
SELECT seq FROM sync_scopes WHERE scope_id = ? FOR UPDATE;
UPDATE sync_scopes SET seq = seq + 1 WHERE scope_id = ?;
-- the new number is the selected value plus one
```

MongoDB, inside a multi-document transaction:

```js
const scope = await db.collection('sync_scopes').findOneAndUpdate(
  { _id: scopeId },
  { $inc: { seq: 1 }, $setOnInsert: { min_seq: 0 } },
  { upsert: true, returnDocument: 'after', session }
);
// scope.seq is the new number
```

### Costs and limits of this design

- Writes within one scope are serialised. Keep transactions short, and keep
  scopes reasonably small: a scope is meant to be a user, a team or a
  tenant, not the whole database.
- Numbers may have gaps (a transaction that takes a number and then rolls
  back, or a rejected change). Gaps are harmless.

## 3.4 Push

One transaction per batch, with a savepoint per change where the database
has savepoints. Otherwise one transaction per change.

```
for change in request.changes:                       # in order
    if sync_seen has change.change_id:
        result = stored result                       # duplicate
        if status is conflict or rejected: attach current record
        continue

    seq = next_seq(scope)                            # lock the scope first

    if table unknown           -> rejected unknown_table
    if table is read-only      -> rejected read_only
    if a field is unknown      -> rejected unknown_field
    idx = sync_index row for (table, id), any scope
    if idx exists in another scope -> rejected forbidden
    if project validation fails    -> rejected <code>

    if op is upsert:
        if idx exists and idx.deleted          -> conflict deleted
        if policy(table) is server_wins
           and idx exists
           and change.base_version != idx.seq  -> conflict stale
        else:
            create or update the project's row with change.fields only
            upsert sync_index with seq, deleted = false
            -> applied, version = seq

    if op is delete:
        if idx is missing                      -> applied, version = null
        if idx.deleted                         -> applied, version = idx.seq
        if policy(table) is server_wins
           and change.base_version != idx.seq  -> conflict stale
        else:
            delete the project's row
            update sync_index with seq, deleted = true
            -> applied, version = seq

    store the result in sync_seen                    # same transaction
    for conflict and rejected: attach the current record
```

Notes:

- The number taken by `next_seq` is simply unused when the change is not
  applied.
- "Update with `change.fields` only" means a partial update. Do not read the
  record, merge in memory and write the whole thing back outside the lock.
- Validation is the project's own code. It sees the table, the ID, the
  fields and the user, and returns either OK or a code and message.
- Deleting a parent record does not tell devices about its children. Either
  write a tombstone for every child through the write path, or do not
  cascade on the server.

## 3.5 Pull

```
if cursor given and cursor < scope.min_seq:  -> 410 cursor_expired
after = cursor or 0

rows = sync_index where scope_id = scope and seq > after
       order by seq limit (limit + 1)
has_more = more than `limit` rows came back; drop the extra row

if after == 0: tombstones may be left out of the response

load the project's rows for the non-deleted entries, grouped by table
build one record per entry: version = entry.seq, data = all synced fields

cursor = seq of the last index row read (or `after` if none)
```

Details that matter:

- Read the index and the project's rows in one consistent snapshot where the
  database offers it (a read-only `REPEATABLE READ` transaction in
  PostgreSQL and MySQL, a snapshot session in MongoDB).
- Without a snapshot, a record can be deleted between the two reads. If the
  project's row is missing for a non-deleted entry, skip that entry but
  still advance the cursor past it. Its tombstone has a higher number and
  will arrive in a later pull.
- The cursor is the number as a string. Encoding it (for example base64) is
  optional and keeps clients from depending on its meaning.
- An index on (`scope_id`, `seq`) makes this a single range scan.

## 3.6 Session

Return the protocol number, the schema versions, the limits, and the list
of scopes for the user. Computing that list is the project's access rule,
for example "the user's own scope, plus one scope per team the user belongs
to, plus the read-only catalogue scope".

Check `X-Sync-Schema` on every request, not only here: below
`min_schema_version` answer 426, above `schema_version` answer 409.

## 3.7 Choosing scopes

| Situation | Scope design |
| --- | --- |
| Personal data, several devices per user | One scope per user: `user:<id>` |
| A team working on shared data | One scope per team: `team:<id>`, `read_write` for members |
| Reference data everyone reads (catalogue, price list) | One read-only scope: `catalog`, or one per region |
| A manager who sees several teams | The session lists several team scopes |
| A user who should see only part of a team's data | Split that data into its own scope. Record-level filtering inside a scope is not supported in version 1 |

A record belongs to one scope for its whole life. To move a record, write a
tombstone in the old scope and create the record in the new one, in one
transaction, taking the two scope locks in a fixed order (sorted by scope
ID) so two moves cannot deadlock.

## 3.8 Housekeeping

- **Tombstones.** Remove index rows with `deleted = true` older than the
  retention period (90 days is a reasonable start; the protocol asks for at
  least 30). In the same transaction set the scope's `min_seq` to the
  highest `seq` removed.
- **Seen changes.** Remove `sync_seen` rows older than 30 days, or longer.
- **New scope.** Create its `sync_scopes` row when the user or team is
  created.
- **Existing data.** When sync is added to a project that already has data,
  run a one-off backfill that passes every existing record through the write
  path.

## 3.9 Notes per database

**PostgreSQL**

- Default `READ COMMITTED` is fine for push. The `ON CONFLICT ... DO UPDATE`
  statement takes the row lock.
- Use `SAVEPOINT` per change so one failing change does not abort the batch.
- Reading from a streaming replica is safe for pull, because replicas apply
  commits in order. A device may then briefly not see its own push, which is
  harmless.

**MySQL and MariaDB**

- Tables must be InnoDB.
- Use `SELECT ... FOR UPDATE` on the scope row as shown; do not rely on
  `AUTO_INCREMENT`.
- Savepoints are available.
- For pull, open the transaction with
  `START TRANSACTION WITH CONSISTENT SNAPSHOT, READ ONLY`.

**MongoDB**

- Multi-document transactions are required, so the deployment must be a
  replica set (Atlas clusters are).
- Two transactions that touch the same scope counter do not queue: one of
  them fails with a transient write conflict. Use the driver's
  `withTransaction` helper, which retries automatically.
- Keep each transaction small: one change, or a few. Long transactions hit
  the transaction time limit.
- A partial update is `$set` with only the pushed fields.
- For pull, read inside a session with snapshot read concern, or apply the
  "skip the missing record" rule from section 3.5.

## 3.10 Common mistakes

| Mistake | Symptom | Rule that prevents it |
| --- | --- | --- |
| Numbering changes with auto-increment or timestamps | Devices occasionally miss a change, never reproducible | Section 3.3 |
| A write that skips the write path (bulk update, raw SQL, admin tool, import) | The change never reaches any device | Section 3.3; note that bulk-update helpers in most ORMs skip per-record hooks |
| Assigning the number before the transaction, or committing the index separately from the data | Same as the first row | Section 3.3 |
| Replacing the whole record on upsert | One device erases another device's edit to a different field | Section 3.4 |
| Not storing results for duplicates | A retried batch applies twice, or returns a different status | Section 3.4 |
| Physically deleting with no tombstone | Deleted records stay forever on other devices | Section 3.2 |
| Returning partial records on pull | Devices lose fields | Protocol 5.2 |

## 3.11 Checklist for a new backend

- [ ] The three sync structures and their indexes exist
- [ ] `next_seq` is implemented as in section 3.3 for this database
- [ ] One `sync_write` function exists and every write to synced data calls it
- [ ] Table list, field lists, read-only flags and conflict policies are configured
- [ ] Scope rules for the session endpoint are written
- [ ] Validation hooks are written
- [ ] Housekeeping jobs for tombstones and seen changes are scheduled
- [ ] Existing data is backfilled
- [ ] The conformance suite passes, including the concurrency tests
