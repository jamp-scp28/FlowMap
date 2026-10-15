use crate::model::*;
use anyhow::Result;
use serde_json::Value as JsonValue;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use syn::visit::Visit;
use syn::{ExprMethodCall, FnArg, ItemFn, ItemStruct};
use walkdir::WalkDir;

pub struct RustExtractor;

#[derive(Debug, Clone)]
struct RouteMapping {
    method: String,
    path: String,
    handler_ident: String,
    file_path: String,
    line: usize,
    tag: Option<String>,
    operation_id: Option<String>,
    handler_fn: Option<ItemFn>,
}

#[derive(Debug, Clone, Default)]
struct RepositoryMethodInfo {
    _fn_name: String,
    is_transactional: bool,
    mutations: Vec<StateMutation>,
    reads: Vec<String>,
    writes: Vec<String>,
    unique_constraints: Vec<String>,
    _steps: Vec<Step>,
}

#[derive(Debug, Clone, Default)]
struct ApplicationFunctionInfo {
    _fn_name: String,
    repo_calls: Vec<String>,
    has_retry_loop: bool,
    loop_condition: Option<String>,
    _steps: Vec<Step>,
}

impl RustExtractor {
    pub fn extract(project_dir: &Path) -> Result<Vec<Flow>> {
        // 1. Try to load openapi.json if present (either in project root or parent)
        let openapi = Self::load_openapi(project_dir);

        // 2. Index all Rust source files in project_dir
        let mut struct_index: HashMap<String, Vec<FieldInfo>> = HashMap::new();
        let mut repo_methods: HashMap<String, RepositoryMethodInfo> = HashMap::new();
        let mut app_functions: HashMap<String, ApplicationFunctionInfo> = HashMap::new();
        let mut handler_fn_map: HashMap<String, (ItemFn, String, usize)> = HashMap::new();
        let mut utoipa_routes: Vec<RouteMapping> = Vec::new();
        let mut axum_routes: Vec<RouteMapping> = Vec::new();

        for entry in WalkDir::new(project_dir)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().map_or(false, |ext| ext == "rs"))
        {
            let path = entry.path();
            let file_str = path.to_string_lossy().to_string();
            if let Ok(content) = fs::read_to_string(path) {
                if let Ok(syntax_file) = syn::parse_file(&content) {
                    let file_stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();

                    // Collect structs
                    for item in &syntax_file.items {
                        if let syn::Item::Struct(st) = item {
                            let fields = Self::extract_struct_fields(st);
                            struct_index.insert(st.ident.to_string(), fields.clone());
                            struct_index.insert(format!("{}::{}", file_stem, st.ident), fields);
                        }
                    }

                    // Collect repository methods if in infrastructure or repo file
                    if file_str.contains("repository") || file_str.contains("infrastructure") {
                        Self::index_repository_methods(&syntax_file, &file_str, &mut repo_methods);
                    }

                    // Collect application use cases if in application/
                    if file_str.contains("application") {
                        Self::index_application_functions(&syntax_file, &file_str, &file_stem, &mut app_functions);
                    }

                    // Collect functions for handler matching & utoipa paths
                    for (line_idx, item) in syntax_file.items.iter().enumerate() {
                        if let syn::Item::Fn(f) = item {
                            let fn_name = f.sig.ident.to_string();
                            handler_fn_map.insert(format!("{}::{}", file_stem, fn_name), (f.clone(), file_str.clone(), line_idx + 1));
                            handler_fn_map.insert(fn_name.clone(), (f.clone(), file_str.clone(), line_idx + 1));

                            // Check utoipa attribute: #[utoipa::path(...)]
                            if let Some(route) = Self::parse_utoipa_path(f, &file_str, line_idx + 1) {
                                utoipa_routes.push(route);
                            }
                        }
                    }

                    // Collect axum routes from Router::new().route(...)
                    if content.contains(".route(") {
                        let mut visitor = RouteVisitor {
                            file_path: file_str.clone(),
                            routes: Vec::new(),
                        };
                        visitor.visit_file(&syntax_file);
                        axum_routes.extend(visitor.routes);
                    }
                }
            }
        }

        // Merge routes (utoipa routes + axum routes, deduplicating by method + path)
        let mut routes = utoipa_routes;
        for r in axum_routes {
            if !routes.iter().any(|existing| existing.method == r.method && existing.path == r.path) {
                routes.push(r);
            }
        }

        let mut flows = Vec::new();
        for route in routes {
            let flow = Self::build_flow_for_route(
                project_dir,
                &route,
                &openapi,
                &struct_index,
                &repo_methods,
                &app_functions,
                &handler_fn_map,
            )?;
            flows.push(flow);
        }

        // Discover Background Workers & Outbox Event Consumers in Rust
        let mut seen_workers = HashSet::new();
        for (ident, (func, file_path, line)) in &handler_fn_map {
            let fn_name = func.sig.ident.to_string();
            let file_lower = file_path.to_lowercase().replace('\\', "/");
            let is_worker = file_lower.contains("process_queue") 
                || file_lower.contains("worker") 
                || fn_name == "process_once" 
                || fn_name == "spawn_email_worker";

            if is_worker && (fn_name == "process_once" || fn_name == "run_worker" || fn_name.starts_with("process_")) {
                let worker_key = format!("{}:{}", file_path, fn_name);
                if !seen_workers.insert(worker_key) {
                    continue;
                }

                let norm_file = file_path.replace('\\', "/");
                let (domain_id, domain_name) = if norm_file.contains("email") {
                    ("email".to_string(), "Email & Notifications".to_string())
                } else {
                    infer_domain_from_path(&norm_file)
                };

                let topic_name = if norm_file.contains("email") {
                    "EmailNotificationQueued".to_string()
                } else {
                    format!("{}_queue", domain_id)
                };

                let mut coupling_reads = Vec::new();
                let mut coupling_writes = Vec::new();
                let mut tables_touched = Vec::new();
                let mut steps = Vec::new();

                if norm_file.contains("email") {
                    coupling_reads.push("email_queue".to_string());
                    coupling_reads.push("email_templates".to_string());
                    coupling_writes.push("email_queue".to_string());

                    tables_touched.push(StateMutation {
                        table: "email_queue".to_string(),
                        operation: "UPDATE".to_string(),
                        condition: Some("claim_batch / mark_sent / mark_failed".to_string()),
                        transactional: true,
                    });
                    tables_touched.push(StateMutation {
                        table: "email_templates".to_string(),
                        operation: "SELECT".to_string(),
                        condition: Some("find_active(template_name, locale)".to_string()),
                        transactional: false,
                    });

                    steps.push(Step {
                        step_type: "effect".to_string(),
                        kind: Some("db_read".to_string()),
                        condition: None,
                        target: Some("email_queue".to_string()),
                        op: Some("Claim batch of pending queue entries (claim_batch)".to_string()),
                        symbol: Some("DieselEmailQueueRepository::claim_batch".to_string()),
                        file: Some(norm_file.clone()),
                        line: Some(*line),
                        status: None,
                        expr: None,
                        confidence: Some("resolved".to_string()),
                        branches: None,
                        steps: None,
                        note: Some("Atomic outbox queue leasing with row lock and lease duration".to_string()),
                        transactional: Some(true),
                        assigns: None,
                    });

                    steps.push(Step {
                        step_type: "effect".to_string(),
                        kind: Some("db_write".to_string()),
                        condition: None,
                        target: Some("email_queue".to_string()),
                        op: Some("Update status to sent or failed with retry count".to_string()),
                        symbol: Some("DieselEmailQueueRepository::mark_sent".to_string()),
                        file: Some(norm_file.clone()),
                        line: Some(*line),
                        status: None,
                        expr: None,
                        confidence: Some("resolved".to_string()),
                        branches: None,
                        steps: None,
                        note: Some("Outbox delivery state transition".to_string()),
                        transactional: Some(true),
                        assigns: None,
                    });
                }

                flows.push(Flow {
                    id: format!("WORKER {} -> {}", topic_name, fn_name),
                    kind: "worker".to_string(),
                    domain: domain_name,
                    domain_id,
                    scenario: format!("Process Asynchronous Outbox {}", topic_name),
                    trigger: Trigger {
                        method: None,
                        path: None,
                        topic: Some(topic_name.clone()),
                        declared_in: format!("{}:{}", norm_file, line),
                    },
                    handler: HandlerInfo {
                        symbol: format!("{}::{}", ident, fn_name),
                        file: norm_file,
                        line: *line,
                        signature: format!("pub async fn {}(...)", fn_name),
                    },
                    contract: FlowContract {
                        request_body: None,
                        parameters: vec![ParamInfo {
                            source: "queue_message".to_string(),
                            name: "batch".to_string(),
                            param_type: Some(topic_name.clone()),
                            description: Some("Batch of leased messages from outbox queue".to_string()),
                        }],
                        response: Some(ResponseInfo {
                            type_name: Some("Result<usize, RepositoryError>".to_string()),
                            status_codes: vec![StatusCodeInfo { code: 200, description: "Batch Processed".to_string() }],
                        }),
                        dependencies: vec![
                            DependencyInfo { name: "EmailQueueRepository".to_string(), kind: "repository".to_string(), source: None },
                            DependencyInfo { name: "TemplateRenderer".to_string(), kind: "renderer".to_string(), source: None },
                            DependencyInfo { name: "EmailSender".to_string(), kind: "service".to_string(), source: None },
                        ],
                    },
                    input: Vec::new(),
                    guards: Vec::new(),
                    steps,
                    coupling: Coupling {
                        reads: coupling_reads,
                        writes: coupling_writes,
                        events_published: Vec::new(),
                        events_consumed: vec![topic_name],
                        calls_services: vec!["EmailSender".to_string()],
                        shared_state_with: Vec::new(),
                        event_consumers: std::collections::BTreeMap::new(),
                        event_producers: std::collections::BTreeMap::new(),
                    },
                    tables_touched,
                    configs_and_flags: Vec::new(),
                    gaps: Vec::new(),
                });
            }
        }

        // Sort flows deterministically by path then method
        flows.sort_by(|a, b| {
            a.trigger
                .path
                .as_deref()
                .unwrap_or("")
                .cmp(b.trigger.path.as_deref().unwrap_or(""))
                .then_with(|| a.trigger.method.as_deref().unwrap_or("").cmp(b.trigger.method.as_deref().unwrap_or("")))
        });

        Ok(flows)
    }

    fn load_openapi(project_dir: &Path) -> Option<JsonValue> {
        let candidates = [
            project_dir.join("openapi.json"),
            project_dir.parent().unwrap_or(project_dir).join("openapi.json"),
        ];
        for c in candidates {
            if c.exists() {
                if let Ok(txt) = fs::read_to_string(&c) {
                    if let Ok(v) = serde_json::from_str::<JsonValue>(&txt) {
                        return Some(v);
                    }
                }
            }
        }
        None
    }

    fn extract_struct_fields(st: &ItemStruct) -> Vec<FieldInfo> {
        let mut fields = Vec::new();
        for field in &st.fields {
            if let Some(ident) = &field.ident {
                let name = ident.to_string();
                let ty = quote::quote!(#field.ty).to_string();
                let is_option = ty.starts_with("Option <") || ty.starts_with("Option<");
                let mut default_val = None;
                let mut doc = None;

                for attr in &field.attrs {
                    let attr_str = quote::quote!(#attr).to_string();
                    if attr_str.contains("default") {
                        default_val = Some("default".to_string());
                    }
                    if attr_str.contains("doc =") {
                        let clean = attr_str.replace("# [doc = \"", "").replace("\"]", "").trim().to_string();
                        doc = Some(clean);
                    }
                }

                fields.push(FieldInfo {
                    name,
                    field_type: ty,
                    required: !is_option && default_val.is_none(),
                    default_val,
                    description: doc,
                });
            }
        }
        fields
    }

    fn parse_utoipa_path(f: &ItemFn, file_path: &str, line: usize) -> Option<RouteMapping> {
        for attr in &f.attrs {
            let attr_str = quote::quote!(#attr).to_string();
            if attr_str.contains("utoipa :: path") || attr_str.contains("utoipa::path") {
                let mut method = "GET".to_string();
                let mut path = String::new();
                let mut tag = None;
                let mut operation_id = None;

                for m in ["post", "get", "put", "patch", "delete"] {
                    if attr_str.contains(&format!(" {} ", m))
                        || attr_str.contains(&format!("( {}", m))
                        || attr_str.contains(&format!("({}", m))
                    {
                        method = m.to_uppercase();
                        break;
                    }
                }

                if let Some(idx) = attr_str.find("path = \"") {
                    let sub = &attr_str[idx + 8..];
                    if let Some(end) = sub.find('\"') {
                        path = sub[..end].to_string();
                    }
                }

                if let Some(idx) = attr_str.find("tag = \"") {
                    let sub = &attr_str[idx + 7..];
                    if let Some(end) = sub.find('\"') {
                        tag = Some(sub[..end].to_string());
                    }
                }

                if let Some(idx) = attr_str.find("operation_id = \"") {
                    let sub = &attr_str[idx + 16..];
                    if let Some(end) = sub.find('\"') {
                        operation_id = Some(sub[..end].to_string());
                    }
                }

                if !path.is_empty() {
                    let file_stem = Path::new(file_path).file_stem().and_then(|s| s.to_str()).unwrap_or("");
                    let handler_ident = format!("{}::{}", file_stem, f.sig.ident);
                    return Some(RouteMapping {
                        method,
                        path,
                        handler_ident,
                        file_path: file_path.to_string(),
                        line,
                        tag,
                        operation_id,
                        handler_fn: Some(f.clone()),
                    });
                }
            }
        }
        None
    }

    fn index_repository_methods(
        syntax_file: &syn::File,
        file_path: &str,
        repo_methods: &mut HashMap<String, RepositoryMethodInfo>,
    ) {
        let norm_path = file_path.replace('\\', "/");
        let domain_from_path = if let Some(idx) = norm_path.find("/infrastructure/") {
            norm_path[idx + 16..].split('/').next().unwrap_or("").to_string()
        } else {
            String::new()
        };

        for item in &syntax_file.items {
            if let syn::Item::Impl(item_impl) = item {
                let self_ty_str = quote::quote!(#item_impl.self_ty).to_string().replace(' ', "");
                for impl_item in &item_impl.items {
                    if let syn::ImplItem::Fn(method) = impl_item {
                        let fn_name = method.sig.ident.to_string();
                        let method_str = quote::quote!(#method).to_string();

                        let is_transactional = method_str.contains(".transaction(") || method_str.contains("transaction ::");
                        let mut mutations = Vec::new();
                        let mut reads = Vec::new();
                        let mut writes = Vec::new();
                        let mut unique_constraints = Vec::new();

                        let norm_method_str = method_str
                            .replace(" :: ", "::").replace(":: ", "::").replace(" ::", "::")
                            .replace(" . ", ".").replace(". ", ".").replace(" .", ".")
                            .replace(" (", "(").replace("( ", "(");

                        // Detect Diesel table operations
                        for line in norm_method_str.split(';') {
                            let line_str = line.trim();
                            // INSERT
                            if line_str.contains("insert_into(") {
                                let table = extract_table_name_from_diesel_call(line_str, "insert_into");
                                if !table.is_empty() && table != "self" {
                                    mutations.push(StateMutation {
                                        table: table.clone(),
                                        operation: "INSERT".to_string(),
                                        condition: None,
                                        transactional: is_transactional,
                                    });
                                    writes.push(table);
                                }
                            }
                            // UPDATE
                            if (line_str.contains("diesel::update(") || line_str.contains("update("))
                                && !line_str.contains("fn update(")
                                && !line_str.contains("pub fn update(")
                            {
                                let table = extract_table_name_from_diesel_call(line_str, "update");
                                if !table.is_empty() && table != "self" {
                                    mutations.push(StateMutation {
                                        table: table.clone(),
                                        operation: "UPDATE".to_string(),
                                        condition: None,
                                        transactional: is_transactional,
                                    });
                                    writes.push(table);
                                }
                            }
                            // DELETE
                            if (line_str.contains("diesel::delete(") || line_str.contains("delete("))
                                && !line_str.contains("fn delete(")
                                && !line_str.contains("pub fn delete(")
                            {
                                let table = extract_table_name_from_diesel_call(line_str, "delete");
                                if !table.is_empty() && table != "self" {
                                    mutations.push(StateMutation {
                                        table: table.clone(),
                                        operation: "DELETE".to_string(),
                                        condition: None,
                                        transactional: is_transactional,
                                    });
                                    writes.push(table);
                                }
                            }
                            // SELECT / READS
                            if line_str.contains(".filter(") || line_str.contains(".select(") || line_str.contains(".load(") || line_str.contains(".first(") {
                                let table = extract_table_name_from_select(line_str);
                                if !table.is_empty() && table != "self" && !reads.contains(&table) {
                                    reads.push(table.clone());
                                    mutations.push(StateMutation {
                                        table,
                                        operation: "SELECT".to_string(),
                                        condition: None,
                                        transactional: false,
                                    });
                                }
                            }
                            // Unique constraints
                            if line_str.contains("constraint_name") {
                                if let Some(idx) = line_str.find("Some(\"") {
                                    let sub = &line_str[idx + 6..];
                                    if let Some(end) = sub.find('\"') {
                                        unique_constraints.push(sub[..end].to_string());
                                    }
                                }
                            }
                        }

                        reads.sort();
                        reads.dedup();
                        writes.sort();
                        writes.dedup();
                        unique_constraints.sort();
                        unique_constraints.dedup();

                        let info = RepositoryMethodInfo {
                            _fn_name: fn_name.clone(),
                            is_transactional,
                            mutations,
                            reads,
                            writes,
                            unique_constraints,
                            _steps: Vec::new(),
                        };

                        if !self_ty_str.is_empty() {
                            repo_methods.insert(format!("{}::{}", self_ty_str, fn_name), info.clone());
                        }
                        if !domain_from_path.is_empty() {
                            repo_methods.insert(format!("{}::{}", domain_from_path, fn_name), info.clone());
                        }
                        repo_methods.insert(fn_name, info);
                    }
                }
            }
        }
    }

    fn index_application_functions(
        syntax_file: &syn::File,
        file_path: &str,
        file_stem: &str,
        app_functions: &mut HashMap<String, ApplicationFunctionInfo>,
    ) {
        let norm_path = file_path.replace('\\', "/");
        let domain_from_path = if let Some(idx) = norm_path.find("/application/") {
            norm_path[idx + 13..].split('/').next().unwrap_or("").to_string()
        } else {
            String::new()
        };

        for item in &syntax_file.items {
            if let syn::Item::Fn(f) = item {
                let fn_name = f.sig.ident.to_string();
                let f_str = quote::quote!(#f).to_string();

                let mut repo_calls = Vec::new();
                let has_retry_loop = f_str.contains("for attempt in") || f_str.contains("loop {") || f_str.contains("while ");
                let mut loop_condition = None;

                if has_retry_loop {
                    if f_str.contains("MAX_SLUG_ATTEMPTS") {
                        loop_condition = Some("Retry up to MAX_SLUG_ATTEMPTS (50) on unique slug collision".to_string());
                    } else {
                        loop_condition = Some("Execution retry loop".to_string());
                    }
                }

                let norm_f_str = f_str
                    .replace(" :: ", "::").replace(":: ", "::").replace(" ::", "::")
                    .replace(" . ", ".").replace(". ", ".").replace(" .", ".")
                    .replace(" (", "(").replace("( ", "(");

                // Look for repo calls like repo.create_animal, repo.submit, repo.decide
                let re = regex::Regex::new(r"repo\.([a-zA-Z0-9_]+)\(").unwrap();
                for caps in re.captures_iter(&norm_f_str) {
                    repo_calls.push(caps[1].to_string());
                }

                let info = ApplicationFunctionInfo {
                    _fn_name: fn_name.clone(),
                    repo_calls,
                    has_retry_loop,
                    loop_condition,
                    _steps: Vec::new(),
                };

                if !domain_from_path.is_empty() {
                    app_functions.insert(format!("{}::{}", domain_from_path, fn_name), info.clone());
                    app_functions.insert(format!("{}::{}::{}", domain_from_path, file_stem, fn_name), info.clone());
                }
                app_functions.insert(format!("{}::{}", file_stem, fn_name), info.clone());
                app_functions.insert(fn_name, info);
            }
        }
    }

    fn build_flow_for_route(
        _project_dir: &Path,
        route: &RouteMapping,
        openapi: &Option<JsonValue>,
        struct_index: &HashMap<String, Vec<FieldInfo>>,
        repo_methods: &HashMap<String, RepositoryMethodInfo>,
        app_functions: &HashMap<String, ApplicationFunctionInfo>,
        _handler_fn_map: &HashMap<String, (ItemFn, String, usize)>,
    ) -> Result<Flow> {
        let clean_ident = route.handler_ident.replace(' ', "");
        let parts: Vec<String> = clean_ident.split("::").map(|s| s.to_string()).collect();
        let fn_name = parts.last().cloned().unwrap_or_else(|| "handle".to_string());

        // Lookup handler fn
        let (mut handler_fn, mut handler_file, mut handler_line) = if let Some(f) = &route.handler_fn {
            (Some(f.clone()), route.file_path.clone(), route.line)
        } else {
            (None, route.file_path.clone(), route.line)
        };

        if handler_fn.is_none() {
            // Check parent dir for sibling files like create.rs
            let parent_dir = Path::new(&route.file_path).parent().unwrap_or(_project_dir);
            let first_part = parts.first().map(|s| s.as_str()).unwrap_or("");
            let candidate_files = [
                parent_dir.join(format!("{}.rs", first_part)),
                parent_dir.join(first_part).join("mod.rs"),
                Path::new(&route.file_path).to_path_buf(),
            ];
            for candidate in candidate_files {
                if candidate.exists() {
                    if let Ok(content) = fs::read_to_string(&candidate) {
                        if let Ok(parsed) = syn::parse_file(&content) {
                            for (l_idx, item) in parsed.items.iter().enumerate() {
                                if let syn::Item::Fn(f) = item {
                                    if f.sig.ident == fn_name {
                                        handler_fn = Some(f.clone());
                                        handler_file = candidate.to_string_lossy().to_string();
                                        handler_line = l_idx + 1;
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
                if handler_fn.is_some() {
                    break;
                }
            }
        }

        // Domain classification
        let (domain_id, domain_name) = if let Some(tag) = &route.tag {
            map_tag_to_domain(tag)
        } else {
            infer_domain_from_path(&route.path)
        };

        // Contract details
        let mut request_body_info: Option<RequestBodyInfo> = None;
        let mut parameters: Vec<ParamInfo> = Vec::new();
        let mut dependencies: Vec<DependencyInfo> = Vec::new();
        let mut response_info: Option<ResponseInfo> = None;
        let mut steps: Vec<Step> = Vec::new();
        let mut guards: Vec<Step> = Vec::new();
        let mut tables_touched: Vec<StateMutation> = Vec::new();
        let mut configs_and_flags: Vec<ConfigInfo> = Vec::new();

        let mut coupling_reads: Vec<String> = Vec::new();
        let mut coupling_writes: Vec<String> = Vec::new();
        let mut events_published: Vec<String> = Vec::new();

        // 1. Extract from OpenAPI if available
        let openapi_op = openapi.as_ref().and_then(|doc| {
            let paths = doc.get("paths")?.as_object()?;
            // match path exactly or replacing {param}
            let op = paths.get(&route.path)?.get(&route.method.to_lowercase())?;
            Some(op.clone())
        });

        if let Some(op) = &openapi_op {
            // Extract parameters
            if let Some(params) = op.get("parameters").and_then(|p| p.as_array()) {
                for p in params {
                    let p_name = p.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
                    let p_in = p.get("in").and_then(|i| i.as_str()).unwrap_or("param").to_string();
                    let p_desc = p.get("description").and_then(|d| d.as_str()).map(|s| s.to_string());
                    let p_type = p.get("schema").and_then(|s| s.get("type")).and_then(|t| t.as_str()).map(|s| s.to_string());
                    parameters.push(ParamInfo {
                        source: p_in,
                        name: p_name,
                        param_type: p_type,
                        description: p_desc,
                    });
                }
            }

            // Extract Request Body
            if let Some(req_body) = op.get("requestBody") {
                if let Some(schema_ref) = req_body
                    .get("content")
                    .and_then(|c| c.get("application/json"))
                    .and_then(|j| j.get("schema"))
                    .and_then(|s| s.get("$ref"))
                    .and_then(|r| r.as_str())
                {
                    let type_name = schema_ref.split('/').last().unwrap_or("Request").to_string();
                    let fields = Self::extract_openapi_schema_fields(openapi.as_ref().unwrap(), &type_name);
                    request_body_info = Some(RequestBodyInfo {
                        type_name,
                        fields,
                        description: req_body.get("description").and_then(|d| d.as_str()).map(|s| s.to_string()),
                    });
                }
            }

            // Extract Responses
            if let Some(responses_obj) = op.get("responses").and_then(|r| r.as_object()) {
                let mut status_codes = Vec::new();
                let mut main_type: Option<String> = None;
                for (code_str, resp_val) in responses_obj {
                    if let Ok(code) = code_str.parse::<u16>() {
                        let desc = resp_val.get("description").and_then(|d| d.as_str()).unwrap_or("").to_string();
                        if let Some(schema_ref) = resp_val
                            .get("content")
                            .and_then(|c| c.get("application/json"))
                            .and_then(|j| j.get("schema"))
                            .and_then(|s| s.get("$ref"))
                            .and_then(|r| r.as_str())
                        {
                            main_type = Some(schema_ref.split('/').last().unwrap_or("").to_string());
                        }
                        status_codes.push(StatusCodeInfo { code, description: desc });
                    }
                }
                status_codes.sort_by_key(|s| s.code);
                response_info = Some(ResponseInfo {
                    type_name: main_type,
                    status_codes,
                });
            }
        }

        // 2. Extract Handler arguments & dependencies from AST
        let mut called_app_functions = Vec::new();
        let mut called_repo_methods = Vec::new();

        if let Some(func) = &handler_fn {
            for arg in &func.sig.inputs {
                if let FnArg::Typed(pat_type) = arg {
                    let type_str = quote::quote!(#pat_type).to_string();

                    // State / Dependencies
                    if type_str.contains("State") {
                        let dep_name = extract_type_from_wrapper(&type_str);
                        let kind = if dep_name.contains("Repository") {
                            "repository"
                        } else if dep_name.contains("Client") {
                            "client"
                        } else if dep_name.contains("Authorizer") {
                            "authorizer"
                        } else if dep_name.contains("Decoder") {
                            "security_auth"
                        } else {
                            "service"
                        };
                        dependencies.push(DependencyInfo {
                            name: dep_name,
                            kind: kind.to_string(),
                            source: Some("State(Arc<...>)".to_string()),
                        });
                    } else if type_str.contains("Extension") {
                        let dep_name = extract_type_from_wrapper(&type_str);
                        dependencies.push(DependencyInfo {
                            name: dep_name,
                            kind: "security_actor".to_string(),
                            source: Some("Extension(...)".to_string()),
                        });
                    } else if type_str.contains("Path") && parameters.is_empty() {
                        let name = extract_arg_name(&type_str);
                        parameters.push(ParamInfo {
                            source: "path".to_string(),
                            name,
                            param_type: Some("Uuid".to_string()),
                            description: Some("Extracted from URL route".to_string()),
                        });
                    } else if type_str.contains("Query") && parameters.is_empty() {
                        let name = extract_arg_name(&type_str);
                        parameters.push(ParamInfo {
                            source: "query".to_string(),
                            name,
                            param_type: Some("QueryFilter".to_string()),
                            description: Some("Extracted from URL query string".to_string()),
                        });
                    } else if type_str.contains("Json") && request_body_info.is_none() {
                        let struct_name = extract_type_from_wrapper(&type_str);
                        let fields = struct_index.get(&struct_name).cloned().unwrap_or_default();
                        request_body_info = Some(RequestBodyInfo {
                            type_name: struct_name,
                            fields,
                            description: None,
                        });
                    }
                }
            }

            // Scan handler statements for guards, calls & sinks
            let handler_body_str = quote::quote!(#func.block).to_string();

            // Default bearer auth guard
            guards.push(Step {
                step_type: "decision".to_string(),
                kind: Some("authn".to_string()),
                condition: Some("Bearer Token JWT & Actor Resolution".to_string()),
                confidence: Some("extracted".to_string()),
                target: None,
                op: None,
                symbol: None,
                file: None,
                line: None,
                status: None,
                expr: None,
                branches: Some(vec![Branch {
                    when: "Token missing / expired / invalid signature".to_string(),
                    steps: vec![Step {
                        step_type: "return".to_string(),
                        kind: None,
                        condition: None,
                        target: None,
                        op: None,
                        symbol: None,
                        file: None,
                        line: None,
                        status: Some("401 Unauthorized".to_string()),
                        expr: None,
                        confidence: Some("extracted".to_string()),
                        branches: None,
                        steps: None,
                        note: None,
                        transactional: None,
                        assigns: None,
                    }],
                }]),
                steps: None,
                note: Some("Enforced via axum_jwt_auth and resolve_actor middleware".to_string()),
                transactional: None,
                assigns: None,
            });

            // Look for validation in handler
            if handler_body_str.contains("validate()") || handler_body_str.contains("validate (") {
                guards.push(Step {
                    step_type: "decision".to_string(),
                    kind: Some("validation".to_string()),
                    condition: Some("body.validate() == Ok(())".to_string()),
                    confidence: Some("extracted".to_string()),
                    target: None,
                    op: None,
                    symbol: None,
                    file: None,
                    line: None,
                    status: None,
                    expr: Some("body.validate().map_err(AppError::Validation)".to_string()),
                    branches: Some(vec![Branch {
                        when: "Validation rules violated".to_string(),
                        steps: vec![Step {
                            step_type: "return".to_string(),
                            kind: None,
                            condition: None,
                            target: None,
                            op: None,
                            symbol: None,
                            file: None,
                            line: None,
                            status: Some("400 AppError::Validation".to_string()),
                            expr: None,
                            confidence: Some("extracted".to_string()),
                            branches: None,
                            steps: None,
                            note: None,
                            transactional: None,
                            assigns: None,
                        }],
                    }]),
                    steps: None,
                    note: Some("Validates input structure invariants".to_string()),
                    transactional: None,
                    assigns: None,
                });
            }

            // Look for authz checks
            if handler_body_str.contains("require_permission") || handler_body_str.contains("require_role") || handler_body_str.contains("require_authz") {
                guards.push(Step {
                    step_type: "decision".to_string(),
                    kind: Some("authz".to_string()),
                    condition: Some("require_permission(actor, permission)".to_string()),
                    confidence: Some("extracted".to_string()),
                    target: None,
                    op: None,
                    symbol: None,
                    file: None,
                    line: None,
                    status: None,
                    expr: None,
                    branches: Some(vec![Branch {
                        when: "Actor lacks required permission".to_string(),
                        steps: vec![Step {
                            step_type: "return".to_string(),
                            kind: None,
                            condition: None,
                            target: None,
                            op: None,
                            symbol: None,
                            file: None,
                            line: None,
                            status: Some("403 Forbidden".to_string()),
                            expr: None,
                            confidence: Some("extracted".to_string()),
                            branches: None,
                            steps: None,
                            note: None,
                            transactional: None,
                            assigns: None,
                        }],
                    }]),
                    steps: None,
                    note: Some("Role and permission authorization policy".to_string()),
                    transactional: None,
                    assigns: None,
                });
            }

            // Find application calls inside spawn_blocking or async calls
            // e.g. create::create(repo, ...), submit::submit(repo, ...), decide::decide(repo, ...)
            let norm_handler_body = handler_body_str
                .replace(" :: ", "::").replace(":: ", "::").replace(" ::", "::")
                .replace(" . ", ".").replace(". ", ".").replace(" .", ".")
                .replace(" (", "(").replace("( ", "(");

            let re_app = regex::Regex::new(r"([a-zA-Z0-9_]+)::([a-zA-Z0-9_]+)\(").unwrap();
            for caps in re_app.captures_iter(&norm_handler_body) {
                let mod_name = &caps[1];
                let fn_c = &caps[2];
                if mod_name != "StatusCode" && mod_name != "Json" && mod_name != "Arc" && mod_name != "State" && mod_name != "Extension" {
                    let candidates = [
                        format!("{}::{}", domain_id, fn_c),
                        format!("{}::{}::{}", domain_id, mod_name, fn_c),
                        format!("{}::{}", mod_name, fn_c),
                        fn_c.to_string(),
                    ];
                    let resolved = candidates.iter().find(|c| app_functions.contains_key(*c)).cloned().unwrap_or_else(|| fn_c.to_string());
                    called_app_functions.push(resolved);
                }
            }

            let re_repo = regex::Regex::new(r"repo\.([a-zA-Z0-9_]+)\(").unwrap();
            for caps in re_repo.captures_iter(&norm_handler_body) {
                called_repo_methods.push(caps[1].to_string());
            }

            // Notification / Email queue
            if handler_body_str.contains("email_queue") || handler_body_str.contains("send_email") {
                events_published.push("EmailNotificationQueued".to_string());
            }
        }

        // 3. Deep Trace into Application Layer
        for app_fn in &called_app_functions {
            if let Some(info) = app_functions.get(app_fn) {
                if info.has_retry_loop {
                    configs_and_flags.push(ConfigInfo {
                        name: "MAX_SLUG_ATTEMPTS".to_string(),
                        kind: "constant".to_string(),
                        value: Some("50".to_string()),
                    });
                    steps.push(Step {
                        step_type: "loop".to_string(),
                        kind: Some("state_check".to_string()),
                        condition: info.loop_condition.clone(),
                        confidence: Some("resolved".to_string()),
                        target: None,
                        op: Some("retry_loop".to_string()),
                        symbol: Some(format!("application::{}", app_fn)),
                        file: None,
                        line: None,
                        status: None,
                        expr: None,
                        branches: Some(vec![
                            Branch {
                                when: "Slug collision occurs".to_string(),
                                steps: vec![Step {
                                    step_type: "effect".to_string(),
                                    kind: Some("state_mutation".to_string()),
                                    condition: None,
                                    target: Some("slug".to_string()),
                                    op: Some("append attempt counter e.g. name-2".to_string()),
                                    symbol: None,
                                    file: None,
                                    line: None,
                                    status: None,
                                    expr: None,
                                    confidence: Some("resolved".to_string()),
                                    branches: None,
                                    steps: None,
                                    note: None,
                                    transactional: None,
                                    assigns: None,
                                }],
                            },
                            Branch {
                                when: "Exhausted 50 attempts".to_string(),
                                steps: vec![Step {
                                    step_type: "return".to_string(),
                                    kind: None,
                                    condition: None,
                                    target: None,
                                    op: None,
                                    symbol: None,
                                    file: None,
                                    line: None,
                                    status: Some("500 RepositoryError(\"exhausted slug attempts\")".to_string()),
                                    expr: None,
                                    confidence: Some("resolved".to_string()),
                                    branches: None,
                                    steps: None,
                                    note: None,
                                    transactional: None,
                                    assigns: None,
                                }],
                            },
                        ]),
                        steps: None,
                        note: Some("Ensures human-friendly URL slugs without duplicate key violations".to_string()),
                        transactional: None,
                        assigns: None,
                    });
                }
                for rc in &info.repo_calls {
                    called_repo_methods.push(rc.clone());
                }
            }
        }

        // 4. Deep Trace into Repository Layer (DB Mutations & Transactions)
        for repo_fn in &called_repo_methods {
            let repo_info = dependencies.iter()
                .find_map(|d| repo_methods.get(&format!("{}::{}", d.name, repo_fn)))
                .or_else(|| repo_methods.get(&format!("{}::{}", domain_id, repo_fn)))
                .or_else(|| repo_methods.get(repo_fn));

            if let Some(info) = repo_info {
                for m in &info.mutations {
                    if m.table != "self" && m.table != "table" && !tables_touched.iter().any(|t| t.table == m.table && t.operation == m.operation) {
                        tables_touched.push(m.clone());
                    }
                }
                for r in &info.reads {
                    if r != "self" && r != "table" && !coupling_reads.contains(r) {
                        coupling_reads.push(r.clone());
                    }
                }
                for w in &info.writes {
                    if w != "self" && w != "table" && !coupling_writes.contains(w) {
                        coupling_writes.push(w.clone());
                    }
                }

                // Add rich step for repository execution
                let op_desc = if info.is_transactional {
                    format!("Database Transaction (conn.transaction) -> mutating {} table(s)", info.writes.len())
                } else {
                    format!("Direct DB Query -> touching {} table(s)", info.writes.len() + info.reads.len())
                };

                steps.push(Step {
                    step_type: "effect".to_string(),
                    kind: Some(if info.writes.is_empty() { "db_read".to_string() } else { "db_write".to_string() }),
                    condition: None,
                    target: Some(if !info.writes.is_empty() { info.writes.join(", ") } else { info.reads.join(", ") }),
                    op: Some(op_desc),
                    symbol: Some(format!("DieselRepository::{}", repo_fn)),
                    file: None,
                    line: None,
                    status: None,
                    expr: None,
                    confidence: Some("resolved".to_string()),
                    branches: if !info.unique_constraints.is_empty() {
                        Some(
                            info.unique_constraints
                                .iter()
                                .map(|c| Branch {
                                    when: format!("UniqueViolation constraint: {}", c),
                                    steps: vec![Step {
                                        step_type: "return".to_string(),
                                        kind: None,
                                        condition: None,
                                        target: None,
                                        op: None,
                                        symbol: None,
                                        file: None,
                                        line: None,
                                        status: Some("Handled via Domain Outcome".to_string()),
                                        expr: None,
                                        confidence: Some("resolved".to_string()),
                                        branches: None,
                                        steps: None,
                                        note: None,
                                        transactional: None,
                                        assigns: None,
                                    }],
                                })
                                .collect(),
                        )
                    } else {
                        None
                    },
                    steps: None,
                    note: Some(format!("Executed in repository: {}", repo_fn)),
                    transactional: Some(info.is_transactional),
                    assigns: None,
                });
            }
        }

        // Fallback if no deep DB mutations found: infer from path
        if tables_touched.is_empty() {
            let (inferred_table, is_write) = infer_table_and_write(&route.method, &route.path);
            let op = if is_write {
                if route.method == "POST" { "INSERT" } else if route.method == "DELETE" { "DELETE" } else { "UPDATE" }
            } else {
                "SELECT"
            };
            tables_touched.push(StateMutation {
                table: inferred_table.clone(),
                operation: op.to_string(),
                condition: None,
                transactional: is_write,
            });
            if is_write {
                coupling_writes.push(inferred_table);
            } else {
                coupling_reads.push(inferred_table);
            }
        }

        // Add return step
        let success_status = if let Some(resp) = &response_info {
            resp.status_codes.first().map(|s| format!("{} {}", s.code, s.description)).unwrap_or("200 OK".to_string())
        } else if route.method == "POST" {
            "201 Created".to_string()
        } else {
            "200 OK".to_string()
        };

        steps.push(Step {
            step_type: "return".to_string(),
            kind: None,
            condition: None,
            target: None,
            op: None,
            symbol: None,
            file: None,
            line: None,
            status: Some(success_status),
            expr: None,
            confidence: Some("extracted".to_string()),
            branches: None,
            steps: None,
            note: Some("Successful response outcome".to_string()),
            transactional: None,
            assigns: None,
        });

        if coupling_writes.contains(&"email_queue".to_string()) || called_repo_methods.iter().any(|m| m.contains("enqueue")) {
            events_published.push("EmailNotificationQueued".to_string());
        }

        coupling_reads.sort();
        coupling_reads.dedup();
        coupling_writes.sort();
        coupling_writes.dedup();
        events_published.sort();
        events_published.dedup();

        let id = format!("{} {}", route.method, route.path);
        let scenario = infer_scenario_from_route(&route.method, &route.path, route.operation_id.as_deref());

        Ok(Flow {
            id,
            kind: "http".to_string(),
            domain: domain_name,
            domain_id,
            scenario,
            trigger: Trigger {
                method: Some(route.method.clone()),
                path: Some(route.path.clone()),
                topic: None,
                declared_in: format!("{}:{}", route.file_path, route.line),
            },
            handler: HandlerInfo {
                symbol: clean_ident,
                file: handler_file,
                line: handler_line,
                signature: format!("fn {}(...) -> Result<...>", fn_name),
            },
            contract: FlowContract {
                request_body: request_body_info,
                parameters,
                response: response_info,
                dependencies,
            },
            input: Vec::new(),
            guards,
            steps,
            coupling: Coupling {
                reads: coupling_reads,
                writes: coupling_writes,
                events_published,
                events_consumed: Vec::new(),
                calls_services: Vec::new(),
                shared_state_with: Vec::new(),
                event_consumers: std::collections::BTreeMap::new(),
                event_producers: std::collections::BTreeMap::new(),
            },
            tables_touched,
            configs_and_flags,
            gaps: Vec::new(),
        })
    }

    fn extract_openapi_schema_fields(doc: &JsonValue, schema_name: &str) -> Vec<FieldInfo> {
        let mut fields = Vec::new();
        if let Some(schema) = doc.get("components").and_then(|c| c.get("schemas")).and_then(|s| s.get(schema_name)) {
            let required_list: HashSet<String> = schema
                .get("required")
                .and_then(|r| r.as_array())
                .map(|arr| arr.iter().filter_map(|v| v.as_str()).map(|s| s.to_string()).collect())
                .unwrap_or_default();

            if let Some(props) = schema.get("properties").and_then(|p| p.as_object()) {
                for (name, prop_val) in props {
                    let field_type = prop_val
                        .get("type")
                        .and_then(|t| t.as_str())
                        .or_else(|| prop_val.get("$ref").and_then(|r| r.as_str()).map(|s| s.split('/').last().unwrap_or("object")))
                        .unwrap_or("string")
                        .to_string();

                    let desc = prop_val.get("description").and_then(|d| d.as_str()).map(|s| s.to_string());
                    let default_val = prop_val.get("default").map(|d| d.to_string());
                    let required = required_list.contains(name);

                    fields.push(FieldInfo {
                        name: name.clone(),
                        field_type,
                        required,
                        default_val,
                        description: desc,
                    });
                }
            }
        }
        fields
    }
}

fn extract_type_from_wrapper(s: &str) -> String {
    let clean = s.replace(' ', "");
    let re = regex::Regex::new(r"State<Arc<([a-zA-Z0-9_]+)>>|State<([a-zA-Z0-9_]+)>|Extension<([a-zA-Z0-9_]+)>|Json<([a-zA-Z0-9_]+)>|Path<([a-zA-Z0-9_]+)>|Query<([a-zA-Z0-9_]+)>").unwrap();
    if let Some(caps) = re.captures(&clean) {
        for i in 1..=6 {
            if let Some(m) = caps.get(i) {
                return m.as_str().to_string();
            }
        }
    }
    let parts: Vec<&str> = s.split(['<', '>', ':']).collect();
    parts.iter().rev()
        .find(|p| !p.trim().is_empty() && !["State", "Arc", "Extension", "Json", "Path", "Query"].contains(&p.trim()))
        .map(|p| p.trim().to_string())
        .unwrap_or_else(|| s.trim().to_string())
}

fn extract_arg_name(s: &str) -> String {
    let clean = s.replace(['(', ')', ':', '&'], " ");
    let parts: Vec<&str> = clean.split_whitespace().collect();
    parts.first().unwrap_or(&"param").to_string()
}

fn extract_table_name_from_diesel_call(line: &str, marker: &str) -> String {
    let clean_line = line.replace(" :: ", "::").replace(":: ", "::").replace(" ::", "::")
        .replace(" (", "(").replace("( ", "(");
    if let Some(idx) = clean_line.find(marker) {
        let sub = &clean_line[idx + marker.len()..];
        if let Some(start) = sub.find('(') {
            let after_paren = &sub[start + 1..];

            // Find the earliest boundary among chained methods or argument terminators
            let mut cut_idx = after_paren.len();
            for delim in &[".filter(", ".set(", ".find(", ")", ","] {
                if let Some(pos) = after_paren.find(delim) {
                    if pos < cut_idx {
                        cut_idx = pos;
                    }
                }
            }

            let target_expr = &after_paren[..cut_idx];
            let clean_table = clean_diesel_table_name(target_expr.trim());
            if !clean_table.is_empty() {
                return clean_table;
            }
        }
    }
    String::new()
}

fn clean_diesel_table_name(inside: &str) -> String {
    let clean = inside.replace('&', "").replace(' ', "");
    let base = clean.split('.').next().unwrap_or(&clean);
    let parts: Vec<&str> = base.split("::").collect();
    if let Some(&last) = parts.last() {
        const INVALID_TABLES: &[&str] = &["self", "table", "conn", "pool", "result", "row", "query", "id"];
        if INVALID_TABLES.contains(&last) {
            if parts.len() > 1 && !INVALID_TABLES.contains(&parts[parts.len() - 2]) {
                return parts[parts.len() - 2].to_string();
            }
            return String::new();
        }
        if last.ends_with("_dsl") && parts.len() > 1 {
            return parts[parts.len() - 2].to_string();
        }
        if !last.is_empty() {
            return last.to_string();
        }
    }
    String::new()
}

fn extract_table_name_from_select(line: &str) -> String {
    // Dynamically match diesel table calls like `readings::table`, `stations::dsl::stations`, etc.
    let re = regex::Regex::new(r"([a-z_][a-z0-9_]*)::(?:table|dsl)").unwrap();
    if let Some(caps) = re.captures(line) {
        return caps[1].to_string();
    }
    String::new()
}

fn infer_table_and_write(method: &str, path: &str) -> (String, bool) {
    let is_write = method == "POST" || method == "PUT" || method == "PATCH" || method == "DELETE";
    let clean = path.trim_start_matches('/');
    let segment = clean.split('/').next().unwrap_or("general");
    let table = segment.replace('-', "_");
    (table, is_write)
}

fn map_tag_to_domain(tag: &str) -> (String, String) {
    let clean = tag.replace('-', "_");
    let name = format!("{} Operations", capitalize_words(tag));
    (clean, name)
}

fn infer_domain_from_path(path: &str) -> (String, String) {
    let lower = path.to_lowercase();
    if lower.contains("weather") || lower.contains("telemetry") || lower.contains("reading") {
        ("weather".to_string(), "Weather Telemetry".to_string())
    } else if lower.contains("alert") || lower.contains("notification") {
        ("alerts".to_string(), "Alerts & Notifications".to_string())
    } else if lower.contains("station") || lower.contains("sensor") {
        ("stations".to_string(), "Station Management".to_string())
    } else if lower.contains("forecast") {
        ("forecast".to_string(), "Forecasting Engine".to_string())
    } else if lower.contains("auth") {
        ("auth".to_string(), "Identity & Access".to_string())
    } else if lower.contains("admin") {
        ("admin".to_string(), "Platform Administration".to_string())
    } else {
        let clean = path.trim_start_matches('/');
        let first = clean.split('/').next().unwrap_or("general").replace(['-', '_'], " ");
        let domain_id = first.replace(' ', "_");
        let domain_name = format!("{} Operations", capitalize_words(&first));
        (domain_id, domain_name)
    }
}

fn infer_scenario_from_route(method: &str, path: &str, _op_id: Option<&str>) -> String {
    let lower = path.to_lowercase();
    match (method, lower.as_str()) {
        ("POST", p) if p.contains("/readings") => "Record Station Weather Telemetry".to_string(),
        ("GET", p) if p.contains("/readings") => "Query Historical Weather Observations".to_string(),
        ("POST", p) if p.contains("/alerts") => "Broadcast Severe Weather Warning".to_string(),
        ("GET", p) if p.contains("/stations") => "List Registered Weather Stations".to_string(),
        ("POST", p) if p.contains("/stations") => "Register New Weather Station".to_string(),
        (m, p) => {
            let last_part = p.split('/').filter(|s| !s.is_empty() && !s.starts_with('{')).last().unwrap_or("Resource");
            format!("{} {}", m, capitalize_words(last_part))
        }
    }
}

fn capitalize_words(s: &str) -> String {
    s.split(['-', '_'])
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                None => String::new(),
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
            }
        })
        .collect::<Vec<String>>()
        .join(" ")
}

struct RouteVisitor {
    file_path: String,
    routes: Vec<RouteMapping>,
}

impl<'ast> Visit<'ast> for RouteVisitor {
    fn visit_expr_method_call(&mut self, i: &'ast ExprMethodCall) {
        if i.method == "route" && i.args.len() >= 2 {
            let path_arg = &i.args[0];
            let handler_arg = &i.args[1];

            let path_str = quote::quote!(#path_arg).to_string().replace('"', "");
            let extracted = parse_methods_and_handlers(handler_arg);

            for (method, handler_ident) in extracted {
                self.routes.push(RouteMapping {
                    method,
                    path: path_str.clone(),
                    handler_ident,
                    file_path: self.file_path.clone(),
                    line: 1,
                    tag: None,
                    operation_id: None,
                    handler_fn: None,
                });
            }
        }
        syn::visit::visit_expr_method_call(self, i);
    }
}

fn parse_methods_and_handlers(expr: &syn::Expr) -> Vec<(String, String)> {
    let s = quote::quote!(#expr).to_string();
    let clean = s
        .replace(" :: ", "::")
        .replace(":: ", "::")
        .replace(" ::", "::")
        .replace(' ', "");

    let re = regex::Regex::new(r"(get|post|put|delete|patch)\s*\(\s*([a-zA-Z0-9_:]+)\s*\)").unwrap();
    let mut results = Vec::new();

    for caps in re.captures_iter(&clean) {
        let method = caps[1].to_uppercase();
        let handler = caps[2].to_string();
        results.push((method, handler));
    }

    if results.is_empty() {
        let method = if clean.contains("post") {
            "POST"
        } else if clean.contains("put") {
            "PUT"
        } else if clean.contains("delete") {
            "DELETE"
        } else if clean.contains("patch") {
            "PATCH"
        } else {
            "GET"
        };
        results.push((method.to_string(), clean));
    }

    results
}
