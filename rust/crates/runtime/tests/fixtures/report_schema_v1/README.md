# Report schema v1 fixture set

Validated by `cargo test -p runtime report_schema -- --nocapture`.

The in-code fixture in `runtime::report_schema::tests::fixture_report` covers:
- fact / hypothesis / confidence labels
- negative evidence with checked surfaces and query window
- field-level delta attribution
- canonical report id plus content hash
- deterministic projection/redaction provenance
- consumer capability negotiation and downgraded projections
