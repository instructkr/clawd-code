//! Machine-checkable conformance helpers for G004 event/report contract bundles.
//!
//! The harness intentionally validates JSON-shaped artifacts instead of owning the
//! lane-event, report, or approval-token implementations. This keeps it usable by
//! independent implementation lanes and by golden fixtures produced outside the
//! runtime crate.

use serde_json::Value;

const BUNDLE_SCHEMA_VERSION: &str = "g004.contract.bundle.v1";
const REPORT_SCHEMA_VERSION: &str = "g004.report.v1";

/// A single conformance validation failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct G004ConformanceError {
    /// JSON pointer-ish path to the invalid field.
    pub path: String,
    /// Human-readable reason the field failed validation.
    pub message: String,
}

impl G004ConformanceError {
    fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
        }
    }
}

/// Validate a G004 golden contract bundle.
///
/// The bundle shape is deliberately small and cross-lane:
/// - `laneEvents[]` must expose stable event identity, ordering/provenance, and
///   terminal dedupe fingerprints.
/// - `reports[]` must expose schema identity, content hash, projection/redaction
///   provenance, capability negotiation, fact/hypothesis/negative-evidence
///   labels, confidence, and field-level delta attribution.
/// - `approvalTokens[]` must expose owner/scope, delegation chain, one-time-use,
///   and replay-prevention fields.
#[must_use]
pub fn validate_g004_contract_bundle(bundle: &Value) -> Vec<G004ConformanceError> {
    let mut errors = Vec::new();

    require_string_eq(bundle, "/schemaVersion", BUNDLE_SCHEMA_VERSION, &mut errors);
    validate_lane_events(bundle.get("laneEvents"), "/laneEvents", &mut errors);
    validate_reports(bundle.get("reports"), "/reports", &mut errors);
    validate_approval_tokens(bundle.get("approvalTokens"), "/approvalTokens", &mut errors);

    errors
}

#[must_use]
pub fn is_g004_contract_bundle_valid(bundle: &Value) -> bool {
    validate_g004_contract_bundle(bundle).is_empty()
}

fn validate_lane_events(value: Option<&Value>, path: &str, errors: &mut Vec<G004ConformanceError>) {
    let Some(events) = non_empty_array(value, path, errors) else {
        return;
    };

    let mut previous_seq = None;
    for (index, event) in events.iter().enumerate() {
        let base = format!("{path}/{index}");
        require_non_empty_string_at(event, "/event", &format!("{base}/event"), errors);
        require_non_empty_string_at(event, "/status", &format!("{base}/status"), errors);
        require_non_empty_string_at(event, "/emittedAt", &format!("{base}/emittedAt"), errors);
        require_non_empty_string_at(
            event,
            "/metadata/provenance",
            &format!("{base}/metadata/provenance"),
            errors,
        );
        require_non_empty_string_at(
            event,
            "/metadata/emitterIdentity",
            &format!("{base}/metadata/emitterIdentity"),
            errors,
        );
        require_non_empty_string_at(
            event,
            "/metadata/environmentLabel",
            &format!("{base}/metadata/environmentLabel"),
            errors,
        );

        match get_path(event, "/metadata/seq").and_then(Value::as_u64) {
            Some(seq) => {
                if let Some(previous) = previous_seq {
                    if seq <= previous {
                        errors.push(G004ConformanceError::new(
                            format!("{base}/metadata/seq"),
                            "sequence must be strictly increasing",
                        ));
                    }
                }
                previous_seq = Some(seq);
            }
            None => errors.push(G004ConformanceError::new(
                format!("{base}/metadata/seq"),
                "required u64 field missing",
            )),
        }

        if is_terminal_event_value(event.get("event")) {
            require_non_empty_string_at(
                event,
                "/metadata/eventFingerprint",
                &format!("{base}/metadata/eventFingerprint"),
                errors,
            );
        }
    }
}

fn validate_reports(value: Option<&Value>, path: &str, errors: &mut Vec<G004ConformanceError>) {
    let Some(reports) = non_empty_array(value, path, errors) else {
        return;
    };

    for (index, report) in reports.iter().enumerate() {
        let base = format!("{path}/{index}");
        require_string_eq_at(
            report,
            "/schemaVersion",
            &format!("{base}/schemaVersion"),
            REPORT_SCHEMA_VERSION,
            errors,
        );
        require_non_empty_string_at(report, "/reportId", &format!("{base}/reportId"), errors);
        require_non_empty_string_at(
            report,
            "/identity/contentHash",
            &format!("{base}/identity/contentHash"),
            errors,
        );
        require_non_empty_string_at(
            report,
            "/projection/provenance",
            &format!("{base}/projection/provenance"),
            errors,
        );
        require_non_empty_string_at(
            report,
            "/redaction/provenance",
            &format!("{base}/redaction/provenance"),
            errors,
        );
        non_empty_array(
            get_path(report, "/consumerCapabilities"),
            &format!("{base}/consumerCapabilities"),
            errors,
        );
        validate_findings(
            get_path(report, "/findings"),
            &format!("{base}/findings"),
            errors,
        );
        validate_field_deltas(
            get_path(report, "/fieldDeltas"),
            &format!("{base}/fieldDeltas"),
            errors,
        );
    }
}

fn validate_findings(value: Option<&Value>, path: &str, errors: &mut Vec<G004ConformanceError>) {
    let Some(findings) = non_empty_array(value, path, errors) else {
        return;
    };

    for (index, finding) in findings.iter().enumerate() {
        let base = format!("{path}/{index}");
        require_one_of_at(
            finding,
            "/kind",
            &format!("{base}/kind"),
            &["fact", "hypothesis", "negative_evidence"],
            errors,
        );
        require_one_of_at(
            finding,
            "/confidence",
            &format!("{base}/confidence"),
            &["low", "medium", "high"],
            errors,
        );
        require_non_empty_string_at(finding, "/statement", &format!("{base}/statement"), errors);
    }
}

fn validate_field_deltas(
    value: Option<&Value>,
    path: &str,
    errors: &mut Vec<G004ConformanceError>,
) {
    let Some(deltas) = non_empty_array(value, path, errors) else {
        return;
    };

    for (index, delta) in deltas.iter().enumerate() {
        let base = format!("{path}/{index}");
        require_non_empty_string_at(delta, "/field", &format!("{base}/field"), errors);
        require_non_empty_string_at(
            delta,
            "/previousHash",
            &format!("{base}/previousHash"),
            errors,
        );
        require_non_empty_string_at(
            delta,
            "/currentHash",
            &format!("{base}/currentHash"),
            errors,
        );
        require_non_empty_string_at(
            delta,
            "/attribution",
            &format!("{base}/attribution"),
            errors,
        );
    }
}

fn validate_approval_tokens(
    value: Option<&Value>,
    path: &str,
    errors: &mut Vec<G004ConformanceError>,
) {
    let Some(tokens) = non_empty_array(value, path, errors) else {
        return;
    };

    for (index, token) in tokens.iter().enumerate() {
        let base = format!("{path}/{index}");
        require_non_empty_string_at(token, "/tokenId", &format!("{base}/tokenId"), errors);
        require_non_empty_string_at(token, "/owner", &format!("{base}/owner"), errors);
        require_non_empty_string_at(token, "/scope", &format!("{base}/scope"), errors);
        require_non_empty_string_at(token, "/issuedAt", &format!("{base}/issuedAt"), errors);
        require_bool_true_at(token, "/oneTimeUse", &format!("{base}/oneTimeUse"), errors);
        require_non_empty_string_at(
            token,
            "/replayPreventionNonce",
            &format!("{base}/replayPreventionNonce"),
            errors,
        );
        validate_delegation_chain(
            get_path(token, "/delegationChain"),
            &format!("{base}/delegationChain"),
            errors,
        );
    }
}

fn validate_delegation_chain(
    value: Option<&Value>,
    path: &str,
    errors: &mut Vec<G004ConformanceError>,
) {
    let Some(chain) = non_empty_array(value, path, errors) else {
        return;
    };

    for (index, hop) in chain.iter().enumerate() {
        let base = format!("{path}/{index}");
        require_non_empty_string_at(hop, "/from", &format!("{base}/from"), errors);
        require_non_empty_string_at(hop, "/to", &format!("{base}/to"), errors);
        require_non_empty_string_at(hop, "/action", &format!("{base}/action"), errors);
        require_non_empty_string_at(hop, "/at", &format!("{base}/at"), errors);
    }
}

fn non_empty_array<'a>(
    value: Option<&'a Value>,
    path: &str,
    errors: &mut Vec<G004ConformanceError>,
) -> Option<&'a Vec<Value>> {
    match value.and_then(Value::as_array) {
        Some(array) if !array.is_empty() => Some(array),
        Some(_) => {
            errors.push(G004ConformanceError::new(path, "array must not be empty"));
            None
        }
        None => {
            errors.push(G004ConformanceError::new(
                path,
                "required array field missing",
            ));
            None
        }
    }
}

fn require_string_eq(
    root: &Value,
    path: &str,
    expected: &str,
    errors: &mut Vec<G004ConformanceError>,
) {
    require_string_eq_at(root, path, path, expected, errors);
}

fn require_string_eq_at(
    root: &Value,
    pointer: &str,
    error_path: &str,
    expected: &str,
    errors: &mut Vec<G004ConformanceError>,
) {
    match get_path(root, pointer).and_then(Value::as_str) {
        Some(actual) if actual == expected => {}
        Some(actual) => errors.push(G004ConformanceError::new(
            error_path,
            format!("expected '{expected}', got '{actual}'"),
        )),
        None => errors.push(G004ConformanceError::new(
            error_path,
            "required string field missing",
        )),
    }
}

fn require_non_empty_string_at(
    root: &Value,
    pointer: &str,
    error_path: &str,
    errors: &mut Vec<G004ConformanceError>,
) {
    match get_path(root, pointer).and_then(Value::as_str) {
        Some(value) if !value.trim().is_empty() => {}
        Some(_) => errors.push(G004ConformanceError::new(
            error_path,
            "string must not be empty",
        )),
        None => errors.push(G004ConformanceError::new(
            error_path,
            "required string field missing",
        )),
    }
}

fn require_one_of_at(
    root: &Value,
    pointer: &str,
    error_path: &str,
    allowed: &[&str],
    errors: &mut Vec<G004ConformanceError>,
) {
    match get_path(root, pointer).and_then(Value::as_str) {
        Some(value) if allowed.contains(&value) => {}
        Some(value) => errors.push(G004ConformanceError::new(
            error_path,
            format!("'{value}' is not one of {}", allowed.join(", ")),
        )),
        None => errors.push(G004ConformanceError::new(
            error_path,
            "required string field missing",
        )),
    }
}

fn require_bool_true_at(
    root: &Value,
    pointer: &str,
    error_path: &str,
    errors: &mut Vec<G004ConformanceError>,
) {
    match get_path(root, pointer).and_then(Value::as_bool) {
        Some(true) => {}
        Some(false) => errors.push(G004ConformanceError::new(error_path, "must be true")),
        None => errors.push(G004ConformanceError::new(
            error_path,
            "required boolean field missing",
        )),
    }
}

fn is_terminal_event_value(value: Option<&Value>) -> bool {
    matches!(
        value.and_then(Value::as_str),
        Some("lane.finished" | "lane.failed" | "lane.merged" | "lane.superseded" | "lane.closed")
    )
}

fn get_path<'a>(root: &'a Value, path: &str) -> Option<&'a Value> {
    if let Some(value) = root.pointer(path) {
        return Some(value);
    }

    let segments = path.trim_start_matches('/').split('/').collect::<Vec<_>>();
    for index in 1..segments.len() {
        let relative = format!("/{}", segments[index..].join("/"));
        if let Some(value) = root.pointer(&relative) {
            return Some(value);
        }
    }
    None
}
