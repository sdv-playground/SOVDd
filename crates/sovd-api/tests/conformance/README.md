# ISO 17978-3 conformance suite

Clause-cited checks of the entity API, each run against both trees (`/v1` and `/vehicle/v1`) over the shared mocks in `tests/common`.
Every test's doc comment cites the ISO 17978-3:2026 clause (or table) it checks; assertions cover status, headers and body shape only.

A known gap is a test marked `#[ignore = "Fxx: <clause> — <one line>"]` that fails today. The ignored tests are the conformance backlog:

    cargo test -p sovd-api --test conformance -- --ignored

Un-ignoring a test is the definition of done for its fix.
Rule: never delete an ignored test; fix the server and un-ignore it.
