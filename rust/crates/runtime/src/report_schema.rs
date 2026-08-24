use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub const REPORT_SCHEMA_V1: &str = "claw.report.v1";
pub const DEFAULT_PROJECTION_POLICY_V1: &str = "claw.report.projection.v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimKind {
    ObservedFact,
    Inference,
    Hypothesis,
    Recommendation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportConfidence {
    High,
    Medium,
    Low,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SensitivityClass {
    Public,
    Internal,
    OperatorOnly,
    Secret,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldDeltaState {
    Changed,
    Unchanged,
    Cleared,
    CarriedForward,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NegativeFindingStatus {
    NotObservedInCheckedScope,
    UnknownNotChecked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportClaim {
    pub id: String,
    pub kind: ClaimKind,
    pub text: String,
    pub confidence: ReportConfidence,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<String>,
    pub sensitivity: SensitivityClass,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NegativeEvidence {
    pub id: String,
    pub status: NegativeFindingStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checked_surfaces: Vec<String>,
    pub query: String,
    pub window: String,
    pub sensitivity: SensitivityClass,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldDelta {
    pub field: String,
    pub state: FieldDeltaState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_hash: Option<String>,
    pub attribution: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportIdentity {
    pub report_id: String,
    pub content_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanonicalReportV1 {
    pub schema_version: String,
    pub identity: ReportIdentity,
    pub generated_at: String,
    pub producer: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub claims: Vec<ReportClaim>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub negative_evidence: Vec<NegativeEvidence>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub field_deltas: Vec<FieldDelta>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsumerCapabilities {
    pub consumer: String,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub schema_versions: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub field_families: BTreeSet<String>,
    pub max_sensitivity: SensitivityClass,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedactionProvenance {
    pub field_path: String,
    pub reason: String,
    pub policy_id: String,
    pub original_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionProvenance {
    pub policy_id: String,
    pub source_schema_version: String,
    pub source_report_id: String,
    pub source_content_hash: String,
    pub consumer: String,
    pub downgraded: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub omitted_field_families: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub redactions: Vec<RedactionProvenance>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportProjectionV1 {
    pub schema_version: String,
    pub projection_id: String,
    pub view: String,
    pub provenance: ProjectionProvenance,
    pub payload: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportSchemaField {
    pub id: String,
    pub description: String,
    pub required: bool,
    pub field_family: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportSchemaRegistry {
    pub schema_version: String,
    pub compatibility: String,
    pub fields: Vec<ReportSchemaField>,
}

#[must_use]
pub fn report_schema_v1_registry() -> ReportSchemaRegistry {
    ReportSchemaRegistry {
        schema_version: REPORT_SCHEMA_V1.to_string(),
        compatibility: "additive fields are compatible; missing required fields are breaking"
            .to_string(),
        fields: vec![
            field(
                "identity.report_id",
                "stable canonical report identity",
                true,
                "identity",
            ),
            field(
                "identity.content_hash",
                "hash of canonical payload excluding identity",
                true,
                "identity",
            ),
            field(
                "claims[].kind",
                "fact/inference/hypothesis/recommendation label",
                true,
                "claims",
            ),
            field(
                "claims[].confidence",
                "confidence bucket for the claim",
                true,
                "claims",
            ),
            field(
                "claims[].evidence",
                "evidence ids supporting a claim",
                false,
                "claims",
            ),
            field(
                "negative_evidence[]",
                "searched-and-not-found findings with checked scope",
                false,
                "negative_evidence",
            ),
            field(
                "field_deltas[]",
                "field-level changed/unchanged/cleared/carried-forward attribution",
                false,
                "field_deltas",
            ),
            field(
                "projection.provenance.redactions[]",
                "redaction policy provenance for projected fields",
                false,
                "projection",
            ),
        ],
    }
}

#[must_use]
pub fn canonicalize_report(mut report: CanonicalReportV1) -> CanonicalReportV1 {
    report.schema_version = REPORT_SCHEMA_V1.to_string();
    report.claims.sort_by(|a, b| a.id.cmp(&b.id));
    report.negative_evidence.sort_by(|a, b| a.id.cmp(&b.id));
    report.field_deltas.sort_by(|a, b| a.field.cmp(&b.field));
    let content_hash = report_content_hash(&report);
    if report.identity.report_id.is_empty() {
        report.identity.report_id = format!("report-{content_hash}");
    }
    report.identity.content_hash = content_hash;
    report
}

#[must_use]
pub fn report_content_hash(report: &CanonicalReportV1) -> String {
    let mut hashable = report.clone();
    hashable.identity.report_id.clear();
    hashable.identity.content_hash.clear();
    stable_json_hash(&serde_json::to_value(hashable).expect("report should serialize"))
}

#[must_use]
pub fn project_report(
    report: &CanonicalReportV1,
    capabilities: &ConsumerCapabilities,
    view: impl Into<String>,
) -> ReportProjectionV1 {
    let view = view.into();
    let supports_schema = capabilities.schema_versions.contains(REPORT_SCHEMA_V1);
    let mut omitted_field_families = Vec::new();
    let mut redactions = Vec::new();
    let mut payload = serde_json::Map::new();

    payload.insert(
        "identity".to_string(),
        serde_json::to_value(&report.identity).expect("identity serializes"),
    );
    payload.insert(
        "generated_at".to_string(),
        Value::String(report.generated_at.clone()),
    );
    payload.insert(
        "producer".to_string(),
        Value::String(report.producer.clone()),
    );

    if supports_family(capabilities, "claims") {
        let claims = report
            .claims
            .iter()
            .enumerate()
            .filter_map(|(index, claim)| redact_claim(index, claim, capabilities, &mut redactions))
            .collect::<Vec<_>>();
        payload.insert("claims".to_string(), Value::Array(claims));
    } else {
        omitted_field_families.push("claims".to_string());
    }

    if supports_family(capabilities, "negative_evidence") {
        payload.insert(
            "negative_evidence".to_string(),
            serde_json::to_value(&report.negative_evidence).expect("negative evidence serializes"),
        );
    } else {
        omitted_field_families.push("negative_evidence".to_string());
    }

    if supports_family(capabilities, "field_deltas") {
        payload.insert(
            "field_deltas".to_string(),
            serde_json::to_value(&report.field_deltas).expect("field deltas serialize"),
        );
    } else {
        omitted_field_families.push("field_deltas".to_string());
    }

    let downgraded =
        !supports_schema || !omitted_field_families.is_empty() || !redactions.is_empty();
    let provenance = ProjectionProvenance {
        policy_id: DEFAULT_PROJECTION_POLICY_V1.to_string(),
        source_schema_version: report.schema_version.clone(),
        source_report_id: report.identity.report_id.clone(),
        source_content_hash: report.identity.content_hash.clone(),
        consumer: capabilities.consumer.clone(),
        downgraded,
        omitted_field_families,
        redactions,
    };
    let mut projection = ReportProjectionV1 {
        schema_version: REPORT_SCHEMA_V1.to_string(),
        projection_id: String::new(),
        view,
        provenance,
        payload: Value::Object(payload),
    };
    projection.projection_id = stable_json_hash(&serde_json::json!({
        "view": projection.view,
        "provenance": projection.provenance,
        "payload": projection.payload,
    }));
    projection
}

fn field(id: &str, description: &str, required: bool, field_family: &str) -> ReportSchemaField {
    ReportSchemaField {
        id: id.to_string(),
        description: description.to_string(),
        required,
        field_family: field_family.to_string(),
    }
}

fn supports_family(capabilities: &ConsumerCapabilities, family: &str) -> bool {
    capabilities.field_families.is_empty() || capabilities.field_families.contains(family)
}

fn redact_claim(
    index: usize,
    claim: &ReportClaim,
    capabilities: &ConsumerCapabilities,
    redactions: &mut Vec<RedactionProvenance>,
) -> Option<Value> {
    if claim.sensitivity <= capabilities.max_sensitivity {
        return Some(serde_json::to_value(claim).expect("claim serializes"));
    }
    if claim.sensitivity == SensitivityClass::Secret {
        redactions.push(RedactionProvenance {
            field_path: format!("claims[{index}]"),
            reason: "omitted: sensitivity exceeds consumer policy".to_string(),
            policy_id: DEFAULT_PROJECTION_POLICY_V1.to_string(),
            original_hash: stable_json_hash(
                &serde_json::to_value(claim).expect("claim serializes"),
            ),
        });
        return None;
    }

    let mut redacted = claim.clone();
    let original_hash = stable_json_hash(&serde_json::to_value(claim).expect("claim serializes"));
    redacted.text = "<redacted>".to_string();
    redacted.evidence.clear();
    redactions.push(RedactionProvenance {
        field_path: format!("claims[{index}].text"),
        reason: "transformed: sensitivity exceeds consumer policy".to_string(),
        policy_id: DEFAULT_PROJECTION_POLICY_V1.to_string(),
        original_hash,
    });
    Some(serde_json::to_value(redacted).expect("redacted claim serializes"))
}

fn stable_json_hash(value: &Value) -> String {
    let normalized = normalize_json(value);
    let bytes = serde_json::to_vec(&normalized).expect("normalized json should serialize");
    let digest = Sha256::digest(bytes);
    let mut hash = String::with_capacity(16);
    for byte in &digest[..8] {
        use std::fmt::Write as _;
        write!(&mut hash, "{byte:02x}").expect("writing to String should not fail");
    }
    hash
}

fn normalize_json(value: &Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.iter().map(normalize_json).collect()),
        Value::Object(map) => {
            let sorted = map
                .iter()
                .map(|(key, value)| (key.clone(), normalize_json(value)))
                .collect::<BTreeMap<_, _>>();
            serde_json::to_value(sorted).expect("sorted map should serialize")
        }
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        canonicalize_report, project_report, report_schema_v1_registry, CanonicalReportV1,
        ClaimKind, ConsumerCapabilities, FieldDelta, FieldDeltaState, NegativeEvidence,
        NegativeFindingStatus, ReportClaim, ReportConfidence, ReportIdentity, SensitivityClass,
        REPORT_SCHEMA_V1,
    };

    fn fixture_report() -> CanonicalReportV1 {
        canonicalize_report(CanonicalReportV1 {
            schema_version: String::new(),
            identity: ReportIdentity {
                report_id: String::new(),
                content_hash: String::new(),
            },
            generated_at: "2026-05-14T00:00:00Z".to_string(),
            producer: "worker-1".to_string(),
            claims: vec![
                ReportClaim {
                    id: "claim-secret".to_string(),
                    kind: ClaimKind::ObservedFact,
                    text: "secret token appeared in logs".to_string(),
                    confidence: ReportConfidence::High,
                    evidence: vec!["log:secret".to_string()],
                    sensitivity: SensitivityClass::Secret,
                },
                ReportClaim {
                    id: "claim-hypothesis".to_string(),
                    kind: ClaimKind::Hypothesis,
                    text: "transport restart likely caused the retry".to_string(),
                    confidence: ReportConfidence::Medium,
                    evidence: vec!["event:transport".to_string()],
                    sensitivity: SensitivityClass::Internal,
                },
                ReportClaim {
                    id: "claim-fact".to_string(),
                    kind: ClaimKind::ObservedFact,
                    text: "lane finished once".to_string(),
                    confidence: ReportConfidence::High,
                    evidence: vec!["event:lane.finished".to_string()],
                    sensitivity: SensitivityClass::Public,
                },
            ],
            negative_evidence: vec![NegativeEvidence {
                id: "neg-blocker".to_string(),
                status: NegativeFindingStatus::NotObservedInCheckedScope,
                checked_surfaces: vec!["lane_events".to_string(), "worker_status".to_string()],
                query: "current blocker".to_string(),
                window: "2026-05-14T00:00:00Z/2026-05-14T00:05:00Z".to_string(),
                sensitivity: SensitivityClass::Public,
            }],
            field_deltas: vec![FieldDelta {
                field: "blocker".to_string(),
                state: FieldDeltaState::Cleared,
                previous_hash: Some("prev123".to_string()),
                current_hash: None,
                attribution: "lane.failed reconciled to lane.finished".to_string(),
            }],
        })
    }

    fn capabilities(families: &[&str], max_sensitivity: SensitivityClass) -> ConsumerCapabilities {
        ConsumerCapabilities {
            consumer: "clawhip".to_string(),
            schema_versions: [REPORT_SCHEMA_V1.to_string()].into_iter().collect(),
            field_families: families
                .iter()
                .map(|family| (*family).to_string())
                .collect(),
            max_sensitivity,
        }
    }

    #[test]
    fn report_schema_registry_is_self_describing() {
        let registry = report_schema_v1_registry();
        assert_eq!(registry.schema_version, REPORT_SCHEMA_V1);
        assert!(registry
            .fields
            .iter()
            .any(|field| field.id == "claims[].kind"));
        assert!(registry
            .fields
            .iter()
            .any(|field| field.id == "negative_evidence[]"));
        assert!(registry
            .fields
            .iter()
            .any(|field| field.id == "projection.provenance.redactions[]"));
    }

    #[test]
    fn canonical_report_labels_claims_negative_evidence_and_deltas() {
        let report = fixture_report();
        assert_eq!(report.schema_version, REPORT_SCHEMA_V1);
        assert!(report.identity.report_id.starts_with("report-"));
        assert_eq!(report.identity.content_hash.len(), 16);
        assert_eq!(report.claims[0].id, "claim-fact");
        assert_eq!(report.claims[1].kind, ClaimKind::Hypothesis);
        assert_eq!(report.claims[1].confidence, ReportConfidence::Medium);
        assert_eq!(
            report.negative_evidence[0].status,
            NegativeFindingStatus::NotObservedInCheckedScope
        );
        assert_eq!(report.field_deltas[0].state, FieldDeltaState::Cleared);
    }

    #[test]
    fn projections_are_deterministic_and_record_redaction_provenance() {
        let report = fixture_report();
        let capabilities = capabilities(
            &["claims", "negative_evidence", "field_deltas"],
            SensitivityClass::Public,
        );

        let first = project_report(&report, &capabilities, "delta_brief");
        let second = project_report(&report, &capabilities, "delta_brief");

        assert_eq!(first, second);
        assert_eq!(first.provenance.source_report_id, report.identity.report_id);
        assert_eq!(
            first.provenance.source_content_hash,
            report.identity.content_hash
        );
        assert!(first.provenance.downgraded);
        assert_eq!(first.provenance.redactions.len(), 2);
        assert!(first
            .provenance
            .redactions
            .iter()
            .any(|redaction| redaction.field_path == "claims[1].text"));
        assert!(first
            .provenance
            .redactions
            .iter()
            .any(|redaction| redaction.field_path == "claims[2]"));
    }

    #[test]
    fn capability_negotiation_omits_unsupported_field_families() {
        let report = fixture_report();
        let capabilities = capabilities(&["claims"], SensitivityClass::Internal);
        let projection = project_report(&report, &capabilities, "legacy_clawhip");

        assert!(projection.provenance.downgraded);
        assert_eq!(
            projection.provenance.omitted_field_families,
            vec!["negative_evidence".to_string(), "field_deltas".to_string()]
        );
        assert!(projection.payload.get("claims").is_some());
        assert!(projection.payload.get("negative_evidence").is_none());
        assert!(projection.payload.get("field_deltas").is_none());
    }
}
