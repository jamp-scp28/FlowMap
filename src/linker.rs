use crate::model::*;
use chrono::Utc;
use std::collections::{BTreeMap, HashMap, HashSet};

pub struct Linker;

impl Linker {
    pub fn build(flows: Vec<Flow>, annotations: HashMap<String, FlowAnnotation>) -> ConnectedFlowMap {
        let mut table_map: BTreeMap<String, (HashSet<String>, HashSet<String>)> = BTreeMap::new();
        let mut event_map: BTreeMap<String, (HashSet<String>, HashSet<String>)> = BTreeMap::new();
        let mut flag_map: BTreeMap<String, HashSet<String>> = BTreeMap::new();
        let mut service_map: BTreeMap<String, HashSet<String>> = BTreeMap::new();

        let mut total_decisions = 0;
        let mut total_effects = 0;

        let mut enriched_flows = flows;

        // 1. Enrich domain and scenario on each flow if not set
        for flow in &mut enriched_flows {
            if flow.domain.is_empty() || flow.domain_id.is_empty() {
                let (dom_id, dom_name) = infer_domain(flow.trigger.path.as_deref().unwrap_or(&flow.id));
                flow.domain_id = dom_id;
                flow.domain = dom_name;
            }

            if flow.scenario.is_empty() {
                flow.scenario = infer_scenario(
                    flow.trigger.method.as_deref().unwrap_or("POST"),
                    flow.trigger.path.as_deref().unwrap_or(&flow.id),
                );
            }

            // Handle trigger topic as consumed event
            if let Some(topic) = &flow.trigger.topic {
                if !flow.coupling.events_consumed.contains(topic) {
                    flow.coupling.events_consumed.push(topic.clone());
                }
                event_map.entry(topic.clone()).or_default().1.insert(flow.id.clone());
            }

            // Sync tables_touched into coupling reads & writes
            for mutation in &flow.tables_touched {
                if mutation.operation == "INSERT"
                    || mutation.operation == "UPDATE"
                    || mutation.operation == "DELETE"
                {
                    if !flow.coupling.writes.contains(&mutation.table) {
                        flow.coupling.writes.push(mutation.table.clone());
                    }
                } else if mutation.operation == "SELECT" {
                    if !flow.coupling.reads.contains(&mutation.table) {
                        flow.coupling.reads.push(mutation.table.clone());
                    }
                }
            }

            // Index guards
            for guard in &flow.guards {
                if guard.kind.as_deref() == Some("feature_flag") {
                    if let Some(cond) = &guard.condition {
                        let clean_flag = clean_feature_flag(cond);
                        flag_map.entry(clean_flag).or_default().insert(flow.id.clone());
                    }
                }
            }

            // Index steps recursively
            Self::scan_steps(
                &flow.steps,
                &flow.id,
                &mut table_map,
                &mut event_map,
                &mut flag_map,
                &mut service_map,
                &mut total_decisions,
                &mut total_effects,
            );

            // Also check coupling declared on the flow
            for r in &flow.coupling.reads {
                table_map.entry(r.clone()).or_default().0.insert(flow.id.clone());
            }
            for w in &flow.coupling.writes {
                table_map.entry(w.clone()).or_default().1.insert(flow.id.clone());
            }
            for p in &flow.coupling.events_published {
                event_map.entry(p.clone()).or_default().0.insert(flow.id.clone());
            }
            for c in &flow.coupling.events_consumed {
                event_map.entry(c.clone()).or_default().1.insert(flow.id.clone());
            }
            for s in &flow.coupling.calls_services {
                let svc = clean_service_name(s);
                service_map.entry(svc).or_default().insert(flow.id.clone());
            }
            for dep in &flow.contract.dependencies {
                if dep.kind == "client" || dep.kind == "service" {
                    service_map.entry(dep.name.clone()).or_default().insert(flow.id.clone());
                }
            }
        }

        // 2. Build shared_state_with, event_consumers, and event_producers on each flow
        let flow_lookup: HashMap<String, (String, String, String, String)> = enriched_flows
            .iter()
            .map(|f| (f.id.clone(), (f.domain.clone(), f.domain_id.clone(), f.scenario.clone(), f.handler.symbol.clone())))
            .collect();

        for flow in &mut enriched_flows {
            let mut coupled_flows = HashSet::new();
            for w in &flow.coupling.writes {
                if let Some((readers, writers)) = table_map.get(w) {
                    for r in readers {
                        if r != &flow.id {
                            coupled_flows.insert(r.clone());
                        }
                    }
                    for wr in writers {
                        if wr != &flow.id {
                            coupled_flows.insert(wr.clone());
                        }
                    }
                }
            }
            for r in &flow.coupling.reads {
                if let Some((_, writers)) = table_map.get(r) {
                    for wr in writers {
                        if wr != &flow.id {
                            coupled_flows.insert(wr.clone());
                        }
                    }
                }
            }

            let mut sorted: Vec<String> = coupled_flows.into_iter().collect();
            sorted.sort();
            flow.coupling.shared_state_with = sorted;

            // Link event consumers for events published by this flow
            for evt in &flow.coupling.events_published {
                if let Some((_, consumers)) = event_map.get(evt) {
                    let mut links = Vec::new();
                    for cid in consumers {
                        if let Some((dom, dom_id, scen, sym)) = flow_lookup.get(cid) {
                            links.push(EventFlowLink {
                                flow_id: cid.clone(),
                                domain: dom.clone(),
                                domain_id: dom_id.clone(),
                                scenario: scen.clone(),
                                handler_symbol: sym.clone(),
                            });
                        }
                    }
                    links.sort_by(|a, b| a.flow_id.cmp(&b.flow_id));
                    flow.coupling.event_consumers.insert(evt.clone(), links);
                }
            }

            // Link event producers for events consumed by this flow
            for evt in &flow.coupling.events_consumed {
                if let Some((producers, _)) = event_map.get(evt) {
                    let mut links = Vec::new();
                    for pid in producers {
                        if let Some((dom, dom_id, scen, sym)) = flow_lookup.get(pid) {
                            links.push(EventFlowLink {
                                flow_id: pid.clone(),
                                domain: dom.clone(),
                                domain_id: dom_id.clone(),
                                scenario: scen.clone(),
                                handler_symbol: sym.clone(),
                            });
                        }
                    }
                    links.sort_by(|a, b| a.flow_id.cmp(&b.flow_id));
                    flow.coupling.event_producers.insert(evt.clone(), links);
                }
            }
        }

        // 3. Convert table & event indexes to serializable structs
        let mut table_index = BTreeMap::new();
        for (tbl, (readers, writers)) in &table_map {
            let mut r_vec: Vec<String> = readers.iter().cloned().collect();
            let mut w_vec: Vec<String> = writers.iter().cloned().collect();
            r_vec.sort();
            w_vec.sort();
            table_index.insert(
                tbl.clone(),
                TableCoupling {
                    table: tbl.clone(),
                    readers: r_vec,
                    writers: w_vec,
                },
            );
        }

        let mut event_index = BTreeMap::new();
        let mut unconsumed_events = Vec::new();
        for (evt, (pubs, cons)) in &event_map {
            let mut p_vec: Vec<String> = pubs.iter().cloned().collect();
            let mut c_vec: Vec<String> = cons.iter().cloned().collect();
            p_vec.sort();
            c_vec.sort();
            let is_consumed_locally = !c_vec.is_empty();
            if !is_consumed_locally && !p_vec.is_empty() {
                unconsumed_events.push(evt.clone());
            }
            event_index.insert(
                evt.clone(),
                EventCoupling {
                    event: evt.clone(),
                    publishers: p_vec,
                    consumers: c_vec,
                    is_consumed_locally,
                },
            );
        }
        unconsumed_events.sort();

        let mut flag_index = BTreeMap::new();
        for (flag, f_ids) in flag_map {
            let mut list: Vec<String> = f_ids.into_iter().collect();
            list.sort();
            flag_index.insert(flag, list);
        }

        let mut service_index = BTreeMap::new();
        for (svc, f_ids) in service_map {
            let mut list: Vec<String> = f_ids.into_iter().collect();
            list.sort();
            service_index.insert(svc, list);
        }

        // 4. Build Domains & Contexts
        let mut domains: BTreeMap<String, DomainContext> = BTreeMap::new();
        for flow in &enriched_flows {
            let entry = domains.entry(flow.domain_id.clone()).or_insert_with(|| {
                let icon = domain_icon(&flow.domain_id);
                let description = domain_description(&flow.domain_id, &flow.domain);
                DomainContext {
                    id: flow.domain_id.clone(),
                    name: flow.domain.clone(),
                    description,
                    icon,
                    flows: Vec::new(),
                    tables_written: Vec::new(),
                    tables_read: Vec::new(),
                    dependencies: Vec::new(),
                }
            });

            if !entry.flows.contains(&flow.id) {
                entry.flows.push(flow.id.clone());
            }

            for w in &flow.coupling.writes {
                if !entry.tables_written.contains(w) {
                    entry.tables_written.push(w.clone());
                }
            }
            for r in &flow.coupling.reads {
                if !entry.tables_read.contains(r) {
                    entry.tables_read.push(r.clone());
                }
            }
            for dep in &flow.contract.dependencies {
                if !entry.dependencies.contains(&dep.name) {
                    entry.dependencies.push(dep.name.clone());
                }
            }
        }

        // Sort domain fields for deterministic output
        for dom in domains.values_mut() {
            dom.flows.sort();
            dom.tables_written.sort();
            dom.tables_read.sort();
            dom.dependencies.sort();
        }

        // 5. Build Entity Topology Nodes
        let mut entity_topology = Vec::new();
        for (tbl, coupling) in &table_index {
            let mut owners = HashSet::new();
            for w_flow_id in &coupling.writers {
                if let Some(f) = enriched_flows.iter().find(|x| &x.id == w_flow_id) {
                    owners.insert(f.domain_id.clone());
                }
            }
            let mut owner_vec: Vec<String> = owners.into_iter().collect();
            owner_vec.sort();

            entity_topology.push(EntityTopologyNode {
                table: tbl.clone(),
                owners: owner_vec,
                readers: coupling.readers.clone(),
                writers: coupling.writers.clone(),
                mutations_count: coupling.writers.len(),
            });
        }
        entity_topology.sort_by(|a, b| b.mutations_count.cmp(&a.mutations_count).then_with(|| a.table.cmp(&b.table)));

        let stats = FlowMapStats {
            total_flows: enriched_flows.len(),
            total_decisions,
            total_effects,
            total_tables_tracked: table_index.len(),
            total_events_tracked: event_index.len(),
            total_flags_tracked: flag_index.len(),
            unconsumed_events,
        };

        ConnectedFlowMap {
            version: "0.2.0".to_string(),
            generated_at: Utc::now().to_rfc3339(),
            flows: enriched_flows,
            domains,
            entity_topology,
            table_index,
            event_index,
            flag_index,
            service_index,
            stats,
            annotations,
        }
    }

    fn scan_steps(
        steps: &[Step],
        flow_id: &str,
        table_map: &mut BTreeMap<String, (HashSet<String>, HashSet<String>)>,
        event_map: &mut BTreeMap<String, (HashSet<String>, HashSet<String>)>,
        flag_map: &mut BTreeMap<String, HashSet<String>>,
        service_map: &mut BTreeMap<String, HashSet<String>>,
        decisions: &mut usize,
        effects: &mut usize,
    ) {
        for s in steps {
            match s.step_type.as_str() {
                "decision" => {
                    *decisions += 1;
                    if s.kind.as_deref() == Some("feature_flag") {
                        if let Some(cond) = &s.condition {
                            let clean = clean_feature_flag(cond);
                            flag_map.entry(clean).or_default().insert(flow_id.to_string());
                        }
                    }
                }
                "effect" => {
                    *effects += 1;
                    if let Some(target) = &s.target {
                        match s.kind.as_deref() {
                            Some("db_read") => {
                                table_map.entry(target.clone()).or_default().0.insert(flow_id.to_string());
                            }
                            Some("db_write") => {
                                table_map.entry(target.clone()).or_default().1.insert(flow_id.to_string());
                            }
                            Some("event_publish") => {
                                event_map.entry(target.clone()).or_default().0.insert(flow_id.to_string());
                            }
                            Some("http_call") => {
                                let svc = clean_service_name(target);
                                service_map.entry(svc).or_default().insert(flow_id.to_string());
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }

            if let Some(branches) = &s.branches {
                for b in branches {
                    Self::scan_steps(
                        &b.steps,
                        flow_id,
                        table_map,
                        event_map,
                        flag_map,
                        service_map,
                        decisions,
                        effects,
                    );
                }
            }

            if let Some(child_steps) = &s.steps {
                Self::scan_steps(
                    child_steps,
                    flow_id,
                    table_map,
                    event_map,
                    flag_map,
                    service_map,
                    decisions,
                    effects,
                );
            }
        }
    }
}

fn infer_domain(path: &str) -> (String, String) {
    let lower = path.to_lowercase();
    if lower.contains("weather") || lower.contains("telemetry") || lower.contains("reading") {
        ("weather".to_string(), "Weather Telemetry".to_string())
    } else if lower.contains("alert") || lower.contains("notification") {
        ("alerts".to_string(), "Alerts & Notifications".to_string())
    } else if lower.contains("station") || lower.contains("sensor") {
        ("stations".to_string(), "Station Management".to_string())
    } else if lower.contains("forecast") {
        ("forecast".to_string(), "Forecasting Engine".to_string())
    } else if lower.contains("auth") || lower.contains("login") || lower.contains("token") {
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

fn infer_scenario(method: &str, path: &str) -> String {
    let lower = path.to_lowercase();
    match (method, lower.as_str()) {
        ("POST", p) if p.contains("/readings") => "Record Station Weather Telemetry".to_string(),
        ("GET", p) if p.contains("/readings") => "Query Historical Weather Observations".to_string(),
        ("POST", p) if p.contains("/alerts") => "Broadcast Severe Weather Warning".to_string(),
        ("GET", p) if p.contains("/stations") => "List Registered Weather Stations".to_string(),
        ("POST", p) if p.contains("/stations") => "Register New Weather Station".to_string(),
        ("POST", p) if p.contains("otp") || p.contains("login") => "Operator Authentication Request".to_string(),
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

fn domain_icon(domain_id: &str) -> String {
    match domain_id {
        "weather" | "telemetry" => "🌤️".to_string(),
        "alerts" => "🚨".to_string(),
        "stations" => "📡".to_string(),
        "forecast" => "📊".to_string(),
        "auth" => "🔐".to_string(),
        "admin" => "🛡️".to_string(),
        _ => "⚙️".to_string(),
    }
}

fn domain_description(domain_id: &str, name: &str) -> String {
    match domain_id {
        "weather" | "telemetry" => "Ingestion, validation, and time-series persistence of environmental sensor telemetry.".to_string(),
        "alerts" => "Severe weather condition evaluation, alert dispatch, and notification routing.".to_string(),
        "stations" => "Registry, geographical positioning, sensor metadata, and calibration intervals.".to_string(),
        "forecast" => "Atmospheric modeling, predictive analysis, and meteorological forecast generation.".to_string(),
        "auth" => "Secures endpoints via JWT bearer tokens and operator authorization policies.".to_string(),
        "admin" => "Privileged administrative actions including station fleet management and system settings.".to_string(),
        _ => format!("Core domain responsibilities for {}.", name),
    }
}

fn clean_feature_flag(cond: &str) -> String {
    let s = cond
        .replace("RequireFeature(Feature.", "")
        .replace("RequireFeature(", "")
        .replace(')', "")
        .replace('"', "")
        .trim()
        .to_string();
    if s.is_empty() {
        cond.to_string()
    } else {
        s
    }
}

fn clean_service_name(target: &str) -> String {
    let parts: Vec<&str> = target.split('.').collect();
    if parts.len() > 3 {
        parts[parts.len() - 3..].join(".")
    } else {
        target.to_string()
    }
}
