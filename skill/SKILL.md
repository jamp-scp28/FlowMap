---
name: flowmap
description: >-
  Behavior and Flow Mapping engine for C#, Rust, and Python codebases. Use to extract execution paths,
  build connected behavior graphs, inspect feature flags, audit architectural hazards, compute
  blast radius / impact analysis before and after code changes, federate cross-repo microservices, and detect breaking regressions.
---

# FlowMap Agent Skill

FlowMap provides **behavioral code understanding** (Action X &rarr; Results Y, Z), replacing the bottleneck
of line-by-line review. It models:
- Entry points (HTTP routes, event consumers, background jobs)
- Deep decision logic (feature flags, validations, role checks, state conditions)
- Sinks (DB writes, DB reads, external HTTP calls, published domain events)
- Cross-flow coupling (shared database tables, event mesh, flag blast radius)

The CLI binary `flowmap.exe` (and its companion extractors) can be placed in your `PATH` or invoked directly.

> [!IMPORTANT]
> **Privacy & Local Execution:** FlowMap operates 100% locally and offline via static AST parsing. It transmits no telemetry or data. When acting as an AI pair-programmer, never read or transmit files containing secrets, API keys, `.env`, or connection strings.

---

## Standard Workflows for Agents

### Workflow 1: Mapping a New Repository (One-Step Automated Mapping)

When the user asks: *"Map this repository"* or *"Help me understand the flows in repo X"*:

1. **Run the Automated Scanner:**
   ```bash
   flowmap scan <path-to-repository>
   ```
   *Example:*
   ```bash
   flowmap scan ./services/weather-api
   ```
   The scanner automatically:
   - Detects the project technology (C# `.csproj/.sln`, Rust `Cargo.toml`, or Python `requirements.txt/pyproject.toml/.py`).
   - Recursively traverses deep method call chains, dependency injection interfaces, MediatR handlers, and domain aggregate roots.
   - Discovers HTTP routes, background queue workers, and event handlers (`IEventHandler<T>`, `IConsumer<T>`, `@subscribe`, Celery tasks).
   - Links bidirectional Pub/Sub topology (Producers ➔ Domain Events ➔ Handlers).
   - Generates clean, zero-pollution artifacts strictly inside `<repo>/flows/`:
     - `<repo>/flows/FLOWS.md` (Markdown flowbook with Mermaid diagrams & decision matrices)
     - `<repo>/flows/index.html` (Interactive visual behavior explorer)
     - `<repo>/flows/flowmap.json` (Connected behavior graph dataset)
   - Runs an automatic behavioral and architectural audit.

2. **Report Summary to User:**
   - Present total bounded contexts, business journeys, state-mutating flows, tracked tables, and pub/sub events.
   - Highlight any anomalies discovered by the audit (dead event topics, premature state mutations).
   - Point the user to the generated clickable artifact links (`<repo>/flows/FLOWS.md` and `<repo>/flows/index.html`).

---

### Workflow 2: Pre-Edit Impact Analysis (Blast Radius Check)

**ALWAYS run impact analysis BEFORE modifying shared domain classes, repositories, or entities!**

When editing a file (e.g. `Quote.cs` or `animal/repository.rs`):
```bash
flowmap impact --files "Quote.cs"
```
Or for a specific method/symbol:
```bash
flowmap impact --symbols "StartDeposit"
```

#### How to Interpret the Output:
- **Direct Impacts:** The handler or called method was directly edited. Check that input/output contracts remain intact.
- **State Coupling Impacts (CRITICAL):** These flows were **NOT** edited, but they read or modify the same database table.
  *Example:* If you alter the status enum transition in `Quote.cs`, verify all other flows that query `Quotes` by status!
- **Event Consumer Impacts:** Handlers that listen to events emitted by the edited flow.

---

### Workflow 3: Feature Branch Behavioral Delta & PR Blast Radius (`flowmap delta`)

When finishing work on a feature branch (`feature/x`), verify behavioral gaps and generate a PR summary:

1. **Run Git-Native Delta Analysis:**
   ```bash
   flowmap delta --against main --pr
   ```
   The engine automatically:
   - Retrieves the baseline map from `main` in memory without switching branches or stashing.
   - Scans the active feature branch and identifies added, modified, and removed flows.
   - Detects **Architectural & Security Hazards**: unconsumed domain events (dead topics), dropped authorization guards, and removed HTTP outcomes.
   - Computes **Secondary Blast Radius**: all existing flows coupled via shared DB tables or events.
   - Generates `<repo>/flows/PR_DELTA.md` ready to paste into GitHub/GitLab pull requests.

2. **Run Strict CI/CD Check:**
   ```bash
   flowmap delta --against main --strict
   ```

---

### Workflow 4: Inspecting a Specific Flow

To examine the execution pipeline and decision tree of an endpoint:
```bash
flowmap show <flow_id_or_substring>
```
*Example:* `flowmap show start-deposit`

---

### Workflow 5: Querying Coupled State & Flags

```bash
# What flows touch this DB table?
flowmap query --table WeatherReadings

# Who publishes and consumes this event?
flowmap query --event SevereWeatherAlertEvent

# What flows are gated by this feature flag?
flowmap query --flag EnableAdvancedForecasts

# Search by keyword
flowmap query --search telemetry
```

---

### Workflow 6: Cross-Repository Pub/Sub Mesh Federation (`flowmap federate`)

When working across multiple microservices (e.g., Service A emits an event consumed by Service B):

```bash
flowmap federate ../order-service ../warehouse-service ../billing-service --out federated_flowmap.json
```
This stitches the event publishers in Service A with the subscribers in Service B, eliminates false dead-event warnings, and identifies shared cross-service database coupling.
