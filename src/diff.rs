use crate::model::*;
use colored::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlowDiffReport {
    pub baseline_version: String,
    pub current_version: String,
    pub flows_added: Vec<String>,
    pub flows_removed: Vec<String>,
    pub flows_modified: BTreeMap<String, FlowDelta>,
    pub breaking_changes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlowDelta {
    pub guards_added: Vec<String>,
    pub guards_removed: Vec<String>,
    pub effects_added: Vec<String>,
    pub effects_removed: Vec<String>,
    pub outcomes_added: Vec<String>,
    pub outcomes_removed: Vec<String>,
    pub is_breaking: bool,
}

pub struct FlowDiffer;

impl FlowDiffer {
    pub fn diff(baseline: &ConnectedFlowMap, current: &ConnectedFlowMap) -> FlowDiffReport {
        let mut base_map = BTreeMap::new();
        for f in &baseline.flows {
            base_map.insert(&f.id, f);
        }

        let mut curr_map = BTreeMap::new();
        for f in &current.flows {
            curr_map.insert(&f.id, f);
        }

        let mut flows_added = Vec::new();
        let mut flows_removed = Vec::new();
        let mut flows_modified = BTreeMap::new();
        let mut breaking_changes = Vec::new();

        // 1. Detect removed flows (breaking if public)
        for (id, _) in &base_map {
            if !curr_map.contains_key(id) {
                flows_removed.push((*id).clone());
                breaking_changes.push(format!("Endpoint removed: {}", id));
            }
        }

        // 2. Detect added flows
        for (id, _) in &curr_map {
            if !base_map.contains_key(id) {
                flows_added.push((*id).clone());
            }
        }

        // 3. Detect modified flows
        for (id, base_flow) in &base_map {
            if let Some(curr_flow) = curr_map.get(id) {
                let delta = Self::diff_flow(base_flow, curr_flow);
                if delta.has_changes() {
                    if delta.is_breaking {
                        breaking_changes.push(format!("Breaking behavioral change in {}: outcomes removed or guards relaxed", id));
                    }
                    flows_modified.insert((*id).clone(), delta);
                }
            }
        }

        FlowDiffReport {
            baseline_version: baseline.generated_at.clone(),
            current_version: current.generated_at.clone(),
            flows_added,
            flows_removed,
            flows_modified,
            breaking_changes,
        }
    }

    fn diff_flow(base: &Flow, curr: &Flow) -> FlowDelta {
        let base_guards: HashSet<_> = base.guards.iter().filter_map(|g| g.condition.clone()).collect();
        let curr_guards: HashSet<_> = curr.guards.iter().filter_map(|g| g.condition.clone()).collect();

        let guards_added: Vec<_> = curr_guards.difference(&base_guards).cloned().collect();
        let guards_removed: Vec<_> = base_guards.difference(&curr_guards).cloned().collect();

        // Effects diff (DB writes, events emitted, external service calls)
        let base_effects: HashSet<_> = Self::extract_effects(base);
        let curr_effects: HashSet<_> = Self::extract_effects(curr);

        let effects_added: Vec<_> = curr_effects.difference(&base_effects).cloned().collect();
        let effects_removed: Vec<_> = base_effects.difference(&curr_effects).cloned().collect();

        // Outcomes diff
        let base_outcomes: HashSet<_> = Self::extract_outcomes(base);
        let curr_outcomes: HashSet<_> = Self::extract_outcomes(curr);

        let outcomes_added: Vec<_> = curr_outcomes.difference(&base_outcomes).cloned().collect();
        let outcomes_removed: Vec<_> = base_outcomes.difference(&curr_outcomes).cloned().collect();

        // Breaking rules:
        // - A guard removed might be a security regression
        // - A previously expected outcome (e.g. 200 OK) removed is an API break
        let is_breaking = !guards_removed.is_empty() || !outcomes_removed.is_empty();

        FlowDelta {
            guards_added,
            guards_removed,
            effects_added,
            effects_removed,
            outcomes_added,
            outcomes_removed,
            is_breaking,
        }
    }

    fn extract_effects(flow: &Flow) -> HashSet<String> {
        let mut set = HashSet::new();
        for w in &flow.coupling.writes {
            set.insert(format!("db_write: {}", w));
        }
        for e in &flow.coupling.events_published {
            set.insert(format!("event: {}", e));
        }
        for s in &flow.coupling.calls_services {
            set.insert(format!("service_call: {}", s));
        }
        set
    }

    fn extract_outcomes(flow: &Flow) -> HashSet<String> {
        let mut set = HashSet::new();
        fn collect(steps: &[Step], set: &mut HashSet<String>) {
            for s in steps {
                if s.step_type == "return" {
                    if let Some(st) = &s.status {
                        set.insert(st.clone());
                    }
                }
                if let Some(branches) = &s.branches {
                    for b in branches {
                        collect(&b.steps, set);
                    }
                }
                if let Some(child_steps) = &s.steps {
                    collect(child_steps, set);
                }
            }
        }
        collect(&flow.steps, &mut set);
        set
    }

    pub fn print_report(report: &FlowDiffReport) {
        println!("{}", "============================================================".bright_blue());
        println!("{}", "              FLOWMAP BEHAVIORAL DIFF REPORT                ".bold().bright_white());
        println!("{}", "============================================================".bright_blue());
        println!("Baseline: {}", report.baseline_version.dimmed());
        println!("Current:  {}", report.current_version.dimmed());
        println!();

        if !report.breaking_changes.is_empty() {
            println!("{}", "[ BREAKING CHANGES & HAZARDS ]".bold().bright_red());
            for b in &report.breaking_changes {
                println!("  {} {}", "!".bold().red(), b.bright_yellow());
            }
            println!();
        }

        if !report.flows_added.is_empty() {
            println!("{}", format!("[ ADDED FLOWS ] (+{})", report.flows_added.len()).bold().bright_green());
            for f in &report.flows_added {
                println!("  + {}", f.green());
            }
            println!();
        }

        if !report.flows_removed.is_empty() {
            println!("{}", format!("[ REMOVED FLOWS ] (-{})", report.flows_removed.len()).bold().bright_red());
            for f in &report.flows_removed {
                println!("  - {}", f.red());
            }
            println!();
        }

        if !report.flows_modified.is_empty() {
            println!("{}", format!("[ MODIFIED BEHAVIOR ] ({} flows)", report.flows_modified.len()).bold().bright_yellow());
            for (id, delta) in &report.flows_modified {
                let badge = if delta.is_breaking { "[BREAKING]".bold().red() } else { "[MODIFIED]".bold().yellow() };
                println!("  * {} {}", id.bold().white(), badge);
                for g in &delta.guards_added {
                    println!("      + guard added:   {}", g.bright_cyan());
                }
                for g in &delta.guards_removed {
                    println!("      - guard removed: {}", g.bright_red());
                }
                for e in &delta.effects_added {
                    println!("      + effect added:  {}", e.bright_green());
                }
                for e in &delta.effects_removed {
                    println!("      - effect removed:{}", e.bright_red());
                }
                for o in &delta.outcomes_added {
                    println!("      + outcome added: {}", o.bright_green());
                }
                for o in &delta.outcomes_removed {
                    println!("      - outcome removed:{}", o.bright_red());
                }
            }
            println!();
        }

        if report.flows_added.is_empty() && report.flows_removed.is_empty() && report.flows_modified.is_empty() {
            println!("{}", "No behavioral differences detected between versions.".green());
        }
    }
}

impl FlowDelta {
    pub fn has_changes(&self) -> bool {
        !self.guards_added.is_empty()
            || !self.guards_removed.is_empty()
            || !self.effects_added.is_empty()
            || !self.effects_removed.is_empty()
            || !self.outcomes_added.is_empty()
            || !self.outcomes_removed.is_empty()
    }
}
