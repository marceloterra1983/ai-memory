# Conditional MCP page write

Frozen interface for a client outbox that must not overwrite a concurrent edit.
Jev does not apply: the decision is UUID equality and whether a live head
exists in the named scope. It is deterministic, with no text classification.

## `memory_read_page`

A path read (and a query that resolves to a path) keeps `path`, `title`,
`body`, `frontmatter`, and, when requested, `related`. It adds `page_id`, the
UUID of the live head whose stored body was returned.

`page_id` is present only when that row's body and frontmatter are the body
and frontmatter in the response. A file that disagrees with the head omits
`page_id`. When the markdown file is missing, `served_from` is `db-fallback`
and `page_id` is the id of the same row that supplied `body`.

## `memory_write_page`

Both arguments are optional. Omitting them keeps the unconditional write.

| Argument | Effect |
|---|---|
| `create_only: true` | Write only when the path has no live (`is_latest = 1`) row and no markdown file in the resolved workspace and project. A tombstone is absent. An existing file with no live row is `already_exists` and `current_page_id` is null. The same path in another project is a different page. |
| `expected_page_id` | UUID string. Write only when that id is the live head and the file is missing or matches that head. |
| both set | Rejected with JSON-RPC `-32602` before any scope or file is created. |
| empty or non-UUID `expected_page_id` | `-32602`, before any mutation. |

`create_only` refuses an existing file before the file is replaced, including
when the index has no live row. That state is admitted: an external edit or
reconcile can leave markdown on disk that the index does not yet own.

`expected_page_id` proceeds only when that id is the live head and the file is
either missing (the read path's db-fallback, which still returns `page_id`) or
parses as that head. The match is the same one `memory_read_page` uses to
attach `page_id`: valid UTF-8, the same body, and the same frontmatter JSON.
A file the read cannot tie to the head is `stale_head` and is not replaced,
even when the SQL id still matches. Bytes that are not UTF-8, that fail to
parse, or whose stored frontmatter is not JSON also refuse. `current_page_id`
is the live head, or null only when no live row exists.

The disk check runs while an exclusive `flock` is held, and it runs before
replace. The writer transaction then rechecks the SQL head. The flock
serializes the snapshot, the install, that transaction, and rollback for the
path across `Store` handles and processes for page-writing operations;
delete/evict operations do not participate in this lock. It is acquired on a blocking
thread, after the mutation lock and the in-process per-path mutex, in the
same sorted order a batch uses. The lock file is
`data_dir/page-locks/<sha256>`, outside the git wiki, because the page file
is replaced by rename and cannot hold the lock. `Store::open` still admits
two handles. A store upsert that bypasses the flock still fails the
transaction check; the guarded call restores the file it replaced. Omitting
both guards keeps the unconditional write; that write takes the same flock
so its rollback cannot restore bytes over another holder.

A lost race does not create a version and does not change markdown, links, or
evidence. The JSON-RPC error is `-32009` with data:

```json
{
  "code": "page_write_conflict",
  "reason": "already_exists",
  "current_page_id": "uuid-or-null"
}
```

`reason` is `already_exists` or `stale_head`. `current_page_id` is the live
head UUID, or `null` when the path has no live row. That null covers
`create_only` against an unindexed file and `stale_head` for an unknown id.
A missing path with a well-formed unknown id is `stale_head`, not `-32602`.
A missing path whose id is still the live head is not a conflict: the write
recreates the file from db-fallback.
