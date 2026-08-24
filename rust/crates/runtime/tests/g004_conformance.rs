use runtime::g004_conformance::{is_g004_contract_bundle_valid, validate_g004_contract_bundle};
use serde_json::{json, Value};

fn valid_bundle() -> Value {
    serde_json::from_str(include_str!("fixtures/g004_contract_bundle.valid.json"))
        .expect("valid fixture JSON should parse")
}

#[test]
fn valid_g004_contract_bundle_fixture_passes_conformance() {
    let fixture = valid_bundle();

    let errors = validate_g004_contract_bundle(&fixture);

    assert!(
        errors.is_empty(),
        "unexpected conformance errors: {errors:?}"
    );
    assert!(is_g004_contract_bundle_valid(&fixture));
}

#[test]
fn g004_conformance_reports_machine_readable_paths_for_contract_gaps() {
    let invalid = json!({
        "schemaVersion": "g004.contract.bundle.v1",
        "laneEvents": [
            {
                "event": "lane.finished",
                "status": "completed",
                "emittedAt": "2026-05-14T00:00:10Z",
                "metadata": {
                    "seq": 1,
                    "provenance": "live_lane",
                    "emitterIdentity": "worker-1",
                    "environmentLabel": "team-g004"
                }
            }
        ],
        "reports": [
            {
                "schemaVersion": "g004.report.v1",
                "reportId": "report-with-gaps",
                "identity": { "contentHash": "sha256:report-content" },
                "projection": { "provenance": "runtime.event_projection.v1" },
                "redaction": { "provenance": "runtime.redaction_policy.v1" },
                "consumerCapabilities": [],
                "findings": [
                    {
                        "kind": "guess",
                        "confidence": "certain",
                        "statement": "bad labels should be rejected"
                    }
                ],
                "fieldDeltas": []
            }
        ],
        "approvalTokens": [
            {
                "tokenId": "approval-token-fixture",
                "owner": "leader-fixed",
                "scope": "g004.contract.bundle.fixture",
                "issuedAt": "2026-05-14T00:00:01Z",
                "oneTimeUse": false,
                "replayPreventionNonce": "nonce-fixture-001",
                "delegationChain": []
            }
        ]
    });

    let errors = validate_g004_contract_bundle(&invalid);
    let paths: Vec<&str> = errors.iter().map(|error| error.path.as_str()).collect();

    assert!(paths.contains(&"/laneEvents/0/metadata/eventFingerprint"));
    assert!(paths.contains(&"/reports/0/consumerCapabilities"));
    assert!(paths.contains(&"/reports/0/findings/0/kind"));
    assert!(paths.contains(&"/reports/0/findings/0/confidence"));
    assert!(paths.contains(&"/reports/0/fieldDeltas"));
    assert!(paths.contains(&"/approvalTokens/0/oneTimeUse"));
    assert!(paths.contains(&"/approvalTokens/0/delegationChain"));
}
