//! This crate's integration tests. Every file here is a module of the lib's
//! test harness (see the `integration` module in `src/lib.rs`), so they cost
//! no extra binary; a new file must be declared below.

mod copilot_embedder;
mod copilot_provider;
mod dream_local;
mod embedding_document_timeout;
mod extra_headers_on_the_wire;
mod fallback_provider;
mod local_embedding_chunks;
mod openai_compat_embedder;
mod openai_compat_strict;
mod query_embedding_cache;
