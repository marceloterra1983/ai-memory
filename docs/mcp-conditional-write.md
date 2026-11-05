# Conditional writes on the local 3.4.2 port

`memory_write_page` accepts either `create_only: true` or an `expected_page_id` from `memory_read_page`. An existing unindexed file blocks creation. A changed file blocks replacement, even when its SQL head has not changed. File checks compare the entire body and frontmatter, before presentation limits. Independent Wiki handles share OS locks through stable files outside the wiki tree. SQL preconditions remain the final authority and a losing write restores its file snapshot.

An explicit page miss returns `error.data.code = "page_not_found"`; an existing-scope lookup miss returns `"scope_not_found"`. Authorization, malformed input, parse and IO failures keep their own errors. A read attaches `page_id` only when that exact source matches the indexed version. External edits remain readable without a version token; reindexing establishes their new version. Missing files use one indexed content-and-id snapshot.

Omitting `max_chars` returns the complete body, with `truncated: false` and no
marker, preserving byte-for-byte readback for the immutable reliable-mcp client.
An explicit `max_chars` retains the upstream range 500–64000 and visible
truncation marker. The version check always uses complete content.
`total_chars` reports the full character count in either case.
