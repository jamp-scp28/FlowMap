use crate::model::*;
use colored::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditIssue {
    pub severity: String, // "CRITICAL", "HIGH", "MEDIUM", "LOW"
    pub category: String, // "SECURITY", "DATA_INTEGRITY", "EVENT_HEALTH", "ARCHITECTURE"
    pub flow_id: String,
    pub description: String,
    pub recommendation: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditReport {
    pub total_issues: usize,
    pub critical_count: usize,
    pub high_count: usize,
    pub medium_count: usize,
    pub issues: Vec<AuditIssue>,
}

pub struct FlowAuditor;

impl FlowAuditor {
    pub fn audit(map: &ConnectedFlowMap) -> AuditReport {
        let mut issues = Vec::new();

        // 1. Audit Unconsumed Events
        for evt in &map.stats.unconsumed_events {
            // Find who publishes it
            let publishers = map.event_index.get(evt).map(|e| e.publishers.clone()).unwrap_or_default();
            issues.push(AuditIssue {
                severity: "HIGH".to_string(),
                category: "EVENT_HEALTH".to_string(),
                flow_id: publishers.first().cloned().unwrap_or_else(|| "Unknown".to_string()),
                description: format!("Event '{}' is published by {} flow(s) but has NO registered consumer in the entire system.", evt, publishers.len()),
                recommendation: format!("Either implement an event consumer for '{}' or remove the dead domain event if deprecated.", evt),
            });
        }

        // 2. Audit Endpoints
        for flow in &map.flows {
            let method = flow.trigger.method.as_deref().unwrap_or("");
            let is_mutation = matches!(method.to_uppercase().as_str(), "POST" | "PUT" | "DELETE" | "PATCH");

            // A. Check for Unguarded Mutating Endpoints
            if is_mutation && !flow.coupling.writes.is_empty() {
                let has_auth = flow.guards.iter().any(|g| {
                    g.kind.as_deref() == Some("authn") || g.kind.as_deref() == Some("authz")
                });

                if !has_auth {
                    issues.push(AuditIssue {
                        severity: "CRITICAL".to_string(),
                        category: "SECURITY".to_string(),
                        flow_id: flow.id.clone(),
                        description: format!("Mutating endpoint '{}' performs DB writes but lacks explicit authentication/authorization guards.", flow.id),
                        recommendation: "Add [Authorize] or actor authorization guard before performing state mutations.".to_string(),
                    });
                }
            }

            // B. Check for State Hazard: Mutation BEFORE Authz/Ownership Check
            Self::check_premature_mutation(flow, &mut issues);

            // C. Check for Unvalidated Writes
            if is_mutation && !flow.coupling.writes.is_empty() {
                let has_validation = Self::has_validation_step(&flow.steps);
                if !has_validation {
                    issues.push(AuditIssue {
                        severity: "MEDIUM".to_string(),
                        category: "DATA_INTEGRITY".to_string(),
                        flow_id: flow.id.clone(),
                        description: format!("Flow '{}' mutates state without an explicit validation step detected.", flow.id),
                        recommendation: "Ensure request payload or domain invariants are validated before writing to persistence.".to_string(),
                    });
                }
            }
        }

        let critical_count = issues.iter().filter(|i| i.severity == "CRITICAL").count();
        let high_count = issues.iter().filter(|i| i.severity == "HIGH").count();
        let medium_count = issues.iter().filter(|i| i.severity == "MEDIUM").count();
        let total_issues = issues.len();

        AuditReport {
            total_issues,
            critical_count,
            high_count,
            medium_count,
            issues,
        }
    }

    fn check_premature_mutation(flow: &Flow, issues: &mut Vec<AuditIssue>) {
        let mut seen_mutation = false;
        let mut mutation_line = 0;

        for step in &flow.steps {
            if step.step_type == "effect" && step.kind.as_deref() == Some("state_mutation") {
                seen_mutation = true;
                mutation_line = step.line.unwrap_or(0);
            }
            if let Some(child_steps) = &step.steps {
                for cs in child_steps {
                    if cs.step_type == "effect" && cs.kind.as_deref() == Some("state_mutation") {
                        seen_mutation = true;
                        mutation_line = cs.line.unwrap_or(0);
                    }
                }
            }

            // If we encounter an authz check AFTER a mutation has already occurred:
            if seen_mutation && step.step_type == "decision" && step.kind.as_deref() == Some("authz") {
                issues.push(AuditIssue {
                    severity: "HIGH".to_string(),
                    category: "DATA_INTEGRITY".to_string(),
                    flow_id: flow.id.clone(),
                    description: format!(
                        "Premature state mutation at line {} occurs BEFORE authorization check at line {} ('{}').",
                        mutation_line,
                        step.line.unwrap_or(0),
                        step.condition.as_deref().unwrap_or("")
                    ),
                    recommendation: "Reorder operations: perform all identity and authorization checks before mutating tracked entities in memory.".to_string(),
                });
                break;
            }
        }
    }

    fn has_validation_step(steps: &[Step]) -> bool {
        for s in steps {
            if s.step_type == "decision" && s.kind.as_deref() == Some("validation") {
                return true;
            }
            if let Some(branches) = &s.branches {
                for b in branches {
                    if Self::has_validation_step(&b.steps) {
                        return true;
                    }
                }
            }
            if let Some(child) = &s.steps {
                if Self::has_validation_step(child) {
                    return true;
                }
            }
        }
        false
    }

    pub fn print_report(report: &AuditReport) {
        println!("{}", "============================================================".bright_blue());
        println!("{}", "              FLOWMAP ARCHITECTURAL & BEHAVIOR AUDIT         ".bold().bright_white());
        println!("{}", "============================================================".bright_blue());
        println!(
            "Total Issues Found: {} ({} Critical, {} High, {} Medium)\n",
            report.total_issues.to_string().bold(),
            report.critical_count.to_string().bold().red(),
            report.high_count.to_string().bold().yellow(),
            report.medium_count.to_string().bold().cyan()
        );

        if report.total_issues == 0 {
            println!("{}", "No architectural or behavioral hazards detected. Clean flow health!".green());
            return;
        }

        for issue in &report.issues {
            let sev_badge = match issue.severity.as_str() {
                "CRITICAL" => "[CRITICAL]".bold().bright_red(),
                "HIGH" => "[HIGH]".bold().bright_yellow(),
                "MEDIUM" => "[MEDIUM]".bold().bright_cyan(),
                _ => "[LOW]".bold().bright_white(),
            };

            println!("{} {} - {}", sev_badge, issue.category.bold().white(), issue.flow_id.bright_purple());
            println!("   Issue:          {}", issue.description);
            println!("   Recommendation: {}", issue.recommendation.bright_green());
            println!();
        }
    }
}
