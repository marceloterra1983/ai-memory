# Conditional writes on the local 3.4.2 port

`memory_write_page` accepts either `create_only: true` or an `expected_page_id` from `memory_read_page`. An existing unindexed file blocks creation. A changed file blocks replacement, even when its SQL head has not changed. File checks compare the entire body and frontmatter, before presentation limits. Independent Wiki handles share OS locks through stable files outside the wiki tree. SQL preconditions remain the final authority and a losing write restores its file snapshot.

An explicit page miss returns `error.data.code = "page_not_found"`; an existing-scope lookup miss returns `"scope_not_found"`. Authorization, malformed input, parse and IO failures keep their own errors. A read attaches `page_id` only when that exact source matches the indexed version. External edits remain readable without a version token; reindexing establishes their new version. Missing files use one indexed content-and-id snapshot.

`max_chars` retains the upstream default 12000 and range 500–64000. The version check always uses complete content. A truncated response is not proof of full body equality; clients must inspect `truncated`. The immutable reliable-mcp client does not negotiate this bound; compatibility for larger pages needs an explicit decision about unbounded reads.
