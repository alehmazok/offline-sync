# 5. Conformance tests

A server implements the protocol correctly when it passes every required
test below. The suite is a command-line program (`sync-conformance`) that
talks to a running server over HTTP and knows nothing about its language or
database. Requirement numbers (R1 to R16) refer to section 10 of
[02-protocol.md](02-protocol.md).

## 5.1 What the server must provide for testing

In a test or staging environment only, the server exposes three extra
tables through the normal sync endpoints:

| Table | Policy | Fields | Validation |
| --- | --- | --- | --- |
| `conf_items` | `field_merge` | `title` text, `qty` integer, `note` text or null | `title` must not be empty, otherwise `validation_failed` |
| `conf_locked` | `server_wins` | `title` text, `qty` integer | none |
| `conf_readonly` | read-only | `title` text | all client changes get `read_only` |

and two test users:

| User | Access |
| --- | --- |
| A | `read_write` on scope S1 and scope S2 |
| B | `read` on scope S1, nothing on S2 |

The suite is configured with the base URL, a token for each user, the two
scope IDs and the schema version. It creates records with fresh random IDs
on every run, so the scopes do not need to be emptied between runs.

In the tests, "device" means an independent client with its own
`X-Sync-Device` value and its own cursor.

## 5.2 Session

| ID | Test | Expected | Req. |
| --- | --- | --- | --- |
| C-SES-01 | User A requests the session | Protocol 1, versions and limits present, S1 and S2 listed as `read_write` | R1 |
| C-SES-02 | User B requests the session | S1 listed as `read`; S2 not listed | R1 |
| C-SES-03 | Request without a token | 401 `unauthorized` | R2 |

## 5.3 Push

| ID | Test | Expected | Req. |
| --- | --- | --- | --- |
| C-PUSH-01 | Create one record | `applied` with a version; a pull returns it with all three fields | R3 |
| C-PUSH-02 | Create, then update one field | Second version is greater; the other fields are unchanged | R3, R6 |
| C-PUSH-03 | Send the same batch twice | Identical statuses and versions; a pull shows the change applied once | R4 |
| C-PUSH-04 | Resend an `applied` change after another device changed the record | Still `applied` with the original version; the other device's change is intact | R4 |
| C-PUSH-05 | Batch of valid, invalid (empty `title`), valid | `applied`, `rejected` `validation_failed`, `applied` | R5 |
| C-PUSH-06 | Rejected change | The result carries `record`: `null` for a record that never existed, the current record otherwise | R13 |
| C-PUSH-07 | Set a field to `null` | A pull returns the field as `null` | R6 |
| C-PUSH-08 | Change to `conf_readonly` | `rejected` `read_only` | R16 |
| C-PUSH-09 | Change with an unknown table; change with an unknown field | `rejected` `unknown_table`; `rejected` `unknown_field` | R16 |
| C-PUSH-10 | Request body with an extra unknown top-level property | Accepted and processed normally | R16 |
| C-PUSH-11 | User B pushes to S1 | 403 `scope_forbidden` | R2 |
| C-PUSH-12 | User B pushes to S2 | 403 or 404 | R2 |
| C-PUSH-13 | User A in S2 upserts an ID that exists in S1 | `rejected` `forbidden`; the S1 record is unchanged | R2 |
| C-PUSH-14 | Batch larger than `max_push_changes` | 413 `too_large`; nothing applied | 7 |
| C-PUSH-15 | Values of every type in section 2.2, including a large integer as a string and non-ASCII text | A pull returns identical values | 2.2 |

## 5.4 Pull

| ID | Test | Expected | Req. |
| --- | --- | --- | --- |
| C-PULL-01 | Pull with no cursor on a scope with data | Records in ascending version order; a cursor is returned | R7 |
| C-PULL-02 | Pull again with the returned cursor, no changes in between | Empty `records`, a cursor, `has_more` false | R7 |
| C-PULL-03 | Create 25 records, pull with `limit=10` until `has_more` is false | Exactly the same set as a single unlimited pull; no duplicates within a page | R10 |
| C-PULL-04 | Delete a record, pull from a cursor taken before the delete | A tombstone: `deleted` true, no `data` | R9 |
| C-PULL-05 | Change one record three times, pull from a cursor before the first change | The record appears at least once, and its last appearance has the final state | R7 |
| C-PULL-06 | **Concurrency.** 8 devices push 50 changes each to one scope in parallel while a ninth pulls in a tight loop, following its cursor | After the writers finish and one last pull, the reader has received every applied record at its final version. The run is repeated 20 times | R8 |
| C-PULL-07 | A device pulls after its own push | Its own change is returned like any other | R7 |
| C-PULL-08 | User B pulls S2 | 403 or 404 | R2 |
| C-PULL-09 | Pull a record with only some fields ever set | `data` contains every field of the table, unset ones as `null` | 5.2 |
| C-PULL-10 | Scopes are independent: changes in S2 | A pull of S1 from its cursor returns nothing | R7 |

C-PULL-06 is the test that catches change numbers becoming visible out of
order. A server that numbers changes with auto-increment or timestamps
passes every other test and fails this one, though not on every run, which
is why it is repeated.

## 5.5 Conflicts

| ID | Test | Expected | Req. |
| --- | --- | --- | --- |
| C-CONF-01 | `conf_items`: devices 1 and 2 hold version N. Device 1 sets `title`, then device 2 sets `qty` with `base_version` N | Both `applied`. The record has device 1's `title` and device 2's `qty` | R11 |
| C-CONF-02 | `conf_items`: both set `title` from the same base | Both `applied`. The later request's value is stored | R11 |
| C-CONF-03 | `conf_locked`: both set a field from the same base | First `applied`; second `conflict` `stale` carrying the current record; nothing from the second is stored | R11, R13 |
| C-CONF-04 | `conf_locked`: update with `base_version` equal to the current version | `applied` | R11 |
| C-CONF-05 | Upsert a record that was deleted | `conflict` `deleted` with a tombstone in `record`; the record stays deleted | R12 |
| C-CONF-06 | Delete a record twice | Both `applied`; the second returns the tombstone's version | R12 |
| C-CONF-07 | Delete an ID the server never saw | `applied` with `version` null | R12 |
| C-CONF-08 | `conf_items`: delete with a stale `base_version` | `applied`; the record is deleted | R11 |
| C-CONF-09 | `conf_locked`: delete with a stale `base_version` | `conflict` `stale`; the record still exists | R11 |
| C-CONF-10 | Upsert a new ID with a non-null `base_version` | `applied`; the record is created | R11 |

## 5.6 Versions and errors

| ID | Test | Expected | Req. |
| --- | --- | --- | --- |
| C-ERR-01 | `X-Sync-Schema` below `min_schema_version` | 426 `upgrade_required` on all three endpoints | R14 |
| C-ERR-02 | `X-Sync-Schema` above `schema_version` | 409 `schema_ahead` | R14 |
| C-ERR-03 | Malformed JSON body | 400 `bad_request` with the standard error body | 7 |
| C-ERR-04 | Pull with a garbage cursor | 400 `bad_request` or 410 `cursor_expired` | 7 |

## 5.7 Optional tests

These need a server-side test switch and are skipped when it is absent.

| ID | Test | Needs | Expected | Req. |
| --- | --- | --- | --- | --- |
| C-OPT-01 | Pull with a cursor older than the pruned tombstones; then download from the beginning in small pages | A way to trigger tombstone pruning | 410 `cursor_expired`; the download finishes, returns every live record, and its final cursor is accepted | R15 |
| C-OPT-02 | Access to a scope is revoked between two requests | A way to change user B's access | The session no longer lists the scope; requests to it get 403 | R1, R2 |

## 5.8 Running it

```
sync-conformance \
  --base-url https://staging.example.com/sync/v1 \
  --token-a ... --token-b ... \
  --scope-1 team:conf1 --scope-2 team:conf2 \
  --schema 1
```

The program prints one line per test and exits non-zero on any failure, so
it can run in each backend's CI. The reference server in this repository
must always pass it; that is how the suite itself is tested
(`cargo test -p sync-conformance`).

## 5.9 Implementation

`crates/sync-conformance` implements all 44 tests above, in the order
listed, with the optional tests last because pruning invalidates old
cursors. Build it with `cargo build --release -p sync-conformance`; the
binary is `target/release/sync-conformance`.

The reference server's test hooks, enabled only by `test_hooks: true` in
its configuration, are:

| Hook | Body | Effect |
| --- | --- | --- |
| `POST /_test/prune` | `{}` | Remove every tombstone and advance each scope's `min_seq` |
| `POST /_test/access` | `{"token", "scope", "access"}` | Grant, change or (with `null`) revoke access |
| `POST /_test/write` | `{"scope", "table", "id", "fields"}` | A server-side write; `fields: null` deletes |
| `GET /_test/dump?scope=` | | Every live record of a scope |

A backend in another stack needs only the first two to run the optional
tests.
