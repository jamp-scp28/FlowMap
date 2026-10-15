use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Flow {
    pub id: String,
    pub kind: String, // "http", "event_handler", "background_job", "cron"
    #[serde(default)]
    pub domain: String, // e.g. "Adoption Operations", "Animal Catalog & Care", "Quoting Engine"
    #[serde(default)]
    pub domain_id: String, // e.g. "adoption_application", "animal", "quoting"
    #[serde(default)]
    pub scenario: String, // e.g. "Submit Adoption Application", "Decide Adoption Application (Approve/Reject)"
    pub trigger: Trigger,
    pub handler: HandlerInfo,
    #[serde(default)]
    pub contract: FlowContract,
    #[serde(default)]
    pub input: Vec<ParamInfo>,
    #[serde(default)]
    pub guards: Vec<Step>,
    #[serde(default)]
    pub steps: Vec<Step>,
    #[serde(default)]
    pub coupling: Coupling,
    #[serde(default)]
    pub tables_touched: Vec<StateMutation>,
    #[serde(default)]
    pub configs_and_flags: Vec<ConfigInfo>,
    #[serde(default)]
    pub gaps: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FlowContract {
    #[serde(default)]
    pub request_body: Option<RequestBodyInfo>,
    #[serde(default)]
    pub parameters: Vec<ParamInfo>,
    #[serde(default)]
    pub response: Option<ResponseInfo>,
    #[serde(default)]
    pub dependencies: Vec<DependencyInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestBodyInfo {
    pub type_name: String,
    #[serde(default)]
    pub fields: Vec<FieldInfo>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldInfo {
    pub name: String,
    pub field_type: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub default_val: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ResponseInfo {
    #[serde(default)]
    pub type_name: Option<String>,
    #[serde(default)]
    pub status_codes: Vec<StatusCodeInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusCodeInfo {
    pub code: u16,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DependencyInfo {
    pub name: String,
    pub kind: String, // "repository", "dbcontext", "authorizer", "security_actor", "client", "service"
    #[serde(default)]
    pub source: Option<String>, // "State(Arc<...>)","Extension(...)","Constructor"
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateMutation {
    pub table: String,
    pub operation: String, // "INSERT", "UPDATE", "DELETE", "SELECT"
    #[serde(default)]
    pub condition: Option<String>,
    #[serde(default)]
    pub transactional: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigInfo {
    pub name: String,
    pub kind: String, // "constant", "feature_flag", "config", "env_var"
    #[serde(default)]
    pub value: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trigger {
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub topic: Option<String>,
    #[serde(default)]
    pub declared_in: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandlerInfo {
    pub symbol: String,
    pub file: String,
    pub line: usize,
    #[serde(default)]
    pub signature: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParamInfo {
    pub source: String,
    pub name: String,
    #[serde(rename = "type", default)]
    pub param_type: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    #[serde(rename = "type")]
    pub step_type: String, // "decision", "effect", "call", "return", "loop", "state"
    #[serde(default)]
    pub kind: Option<String>, // "authn", "authz", "feature_flag", "config", "state_check", "validation", "db_read", "db_write", "event_publish", "http_call", "log"
    #[serde(default)]
    pub condition: Option<String>,
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub op: Option<String>,
    #[serde(default)]
    pub symbol: Option<String>,
    #[serde(default)]
    pub file: Option<String>,
    #[serde(default)]
    pub line: Option<usize>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub expr: Option<String>,
    #[serde(default)]
    pub confidence: Option<String>, // "extracted", "resolved", "inferred"
    #[serde(default)]
    pub branches: Option<Vec<Branch>>,
    #[serde(default)]
    pub steps: Option<Vec<Step>>,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub transactional: Option<bool>,
    #[serde(default)]
    pub assigns: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Branch {
    pub when: String,
    pub steps: Vec<Step>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventFlowLink {
    pub flow_id: String,
    pub domain: String,
    pub domain_id: String,
    pub scenario: String,
    pub handler_symbol: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Coupling {
    #[serde(default)]
    pub reads: Vec<String>,
    #[serde(default)]
    pub writes: Vec<String>,
    #[serde(default)]
    pub events_published: Vec<String>,
    #[serde(default)]
    pub events_consumed: Vec<String>,
    #[serde(default)]
    pub calls_services: Vec<String>,
    #[serde(default)]
    pub shared_state_with: Vec<String>,
    #[serde(default)]
    pub event_consumers: BTreeMap<String, Vec<EventFlowLink>>,
    #[serde(default)]
    pub event_producers: BTreeMap<String, Vec<EventFlowLink>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FlowAnnotation {
    pub flow_id: String,
    pub business_summary: String,
    pub intent: String,
    pub risk_level: String, // "low", "medium", "high", "critical"
    pub verified_by_test: Option<String>,
    pub author: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableCoupling {
    pub table: String,
    pub readers: Vec<String>, // Flow IDs
    pub writers: Vec<String>, // Flow IDs
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventCoupling {
    pub event: String,
    pub publishers: Vec<String>, // Flow IDs
    pub consumers: Vec<String>, // Flow IDs
    #[serde(default)]
    pub is_consumed_locally: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FlowMapStats {
    pub total_flows: usize,
    pub total_decisions: usize,
    pub total_effects: usize,
    pub total_tables_tracked: usize,
    pub total_events_tracked: usize,
    pub total_flags_tracked: usize,
    pub unconsumed_events: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DomainContext {
    pub id: String,
    pub name: String,
    pub description: String,
    pub icon: String,
    pub flows: Vec<String>, // Flow IDs
    pub tables_written: Vec<String>,
    pub tables_read: Vec<String>,
    pub dependencies: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityTopologyNode {
    pub table: String,
    pub owners: Vec<String>, // Domain IDs
    pub readers: Vec<String>, // Flow IDs
    pub writers: Vec<String>, // Flow IDs
    pub mutations_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectedFlowMap {
    pub version: String,
    pub generated_at: String,
    pub flows: Vec<Flow>,
    #[serde(default)]
    pub domains: BTreeMap<String, DomainContext>,
    #[serde(default)]
    pub entity_topology: Vec<EntityTopologyNode>,
    pub table_index: BTreeMap<String, TableCoupling>,
    pub event_index: BTreeMap<String, EventCoupling>,
    pub flag_index: BTreeMap<String, Vec<String>>,
    pub service_index: BTreeMap<String, Vec<String>>,
    pub stats: FlowMapStats,
    pub annotations: HashMap<String, FlowAnnotation>,
}
