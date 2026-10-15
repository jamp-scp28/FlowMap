#!/usr/bin/env python3
"""
FlowMap Python AST Extractor
Discovers HTTP routes (FastAPI, Flask, Starlette), Celery workers,
event publishers/subscribers, database operations, and feature flags.
"""

import ast
import json
import os
import sys
from pathlib import Path


class PythonFlowExtractor(ast.NodeVisitor):
    def __init__(self, filepath: str, rel_path: str):
        self.filepath = filepath
        self.rel_path = rel_path.replace("\\", "/")
        self.flows = []
        self.current_class = None

    def visit_ClassDef(self, node: ast.ClassDef):
        prev_class = self.current_class
        self.current_class = node.name
        self.generic_visit(node)
        self.current_class = prev_class

    def visit_FunctionDef(self, node: ast.FunctionDef):
        self._analyze_function(node)
        self.generic_visit(node)

    def visit_AsyncFunctionDef(self, node: ast.AsyncFunctionDef):
        self._analyze_function(node)
        self.generic_visit(node)

    def _analyze_function(self, node):
        flow_info = self._detect_flow_trigger(node)
        if not flow_info:
            return

        flow_kind, method, path, topic = flow_info

        # Infer domain and scenario
        domain, domain_id = self._infer_domain(path or topic or node.name)
        scenario = self._infer_scenario(method, path or topic or node.name)

        flow_id = (
            f"{method.upper()} {path.lstrip('/')}"
            if flow_kind == "http" and path
            else (f"{topic}Handler" if topic else node.name)
        )

        steps = []
        guards = []
        configs_and_flags = []
        events_published = set()
        events_consumed = set()
        db_writes = set()
        db_reads = set()
        tables_touched = []
        external_calls = set()

        if topic:
            events_consumed.add(topic)

        # Inspect statements inside function body
        step_counter = 1
        for stmt in ast.walk(node):
            if stmt is node:
                continue

            # 1. Feature flags / config branching
            if isinstance(stmt, ast.If):
                flag_name = self._extract_flag_name(stmt.test)
                if flag_name:
                    guards.append({
                        "type": "decision",
                        "kind": "feature_flag",
                        "condition": flag_name,
                        "line": stmt.lineno,
                        "confidence": "extracted"
                    })
                    configs_and_flags.append({
                        "name": flag_name,
                        "location": f"{self.rel_path}:{stmt.lineno}",
                        "kind": "feature_flag"
                    })
                    step_counter += 1

            # 2. Function calls (events, db, external, domain)
            elif isinstance(stmt, ast.Call):
                call_info = self._analyze_call(stmt)
                if not call_info:
                    continue

                call_kind, call_detail = call_info

                if call_kind == "event_publish":
                    if call_detail and call_detail not in events_published:
                        events_published.add(call_detail)
                        steps.append({
                            "type": "effect",
                            "kind": "event_publish",
                            "target": call_detail,
                            "op": "publish",
                            "line": stmt.lineno,
                            "confidence": "extracted"
                        })
                        step_counter += 1
                elif call_kind == "db_write":
                    if call_detail:
                        db_writes.add(call_detail)
                        if not any(t["table"] == call_detail and t["operation"] == "UPDATE" for t in tables_touched):
                            tables_touched.append({"table": call_detail, "operation": "UPDATE"})
                        steps.append({
                            "type": "effect",
                            "kind": "db_write",
                            "target": call_detail,
                            "op": "write",
                            "line": stmt.lineno,
                            "confidence": "extracted"
                        })
                        step_counter += 1
                elif call_kind == "db_read":
                    if call_detail:
                        db_reads.add(call_detail)
                        if not any(t["table"] == call_detail and t["operation"] == "SELECT" for t in tables_touched):
                            tables_touched.append({"table": call_detail, "operation": "SELECT"})
                        steps.append({
                            "type": "effect",
                            "kind": "db_read",
                            "target": call_detail,
                            "op": "read",
                            "line": stmt.lineno,
                            "confidence": "extracted"
                        })
                        step_counter += 1
                elif call_kind == "external_call":
                    external_calls.add(call_detail)
                    steps.append({
                        "type": "effect",
                        "kind": "http_call",
                        "target": call_detail,
                        "line": stmt.lineno,
                        "confidence": "extracted"
                    })
                    step_counter += 1
                elif call_kind == "call":
                    steps.append({
                        "type": "call",
                        "symbol": call_detail,
                        "line": stmt.lineno,
                        "confidence": "extracted"
                    })
                    step_counter += 1

        trigger_obj = {"type": flow_kind}
        if method:
            trigger_obj["method"] = method.upper()
        if path:
            trigger_obj["path"] = path
        if topic:
            trigger_obj["topic"] = topic

        symbol = f"{self.current_class}.{node.name}" if self.current_class else node.name

        flow = {
            "id": flow_id,
            "kind": flow_kind,
            "domain": domain,
            "domain_id": domain_id,
            "scenario": scenario,
            "trigger": trigger_obj,
            "handler": {
                "symbol": symbol,
                "file": self.rel_path,
                "line": node.lineno,
                "signature": f"def {node.name}(...)",
            },
            "contract": {
                "parameters": [],
                "dependencies": []
            },
            "input": [],
            "guards": guards,
            "steps": steps,
            "coupling": {
                "reads": sorted(list(db_reads)),
                "writes": sorted(list(db_writes)),
                "events_published": sorted(list(events_published)),
                "events_consumed": sorted(list(events_consumed)),
                "external_calls": sorted(list(external_calls)),
            },
            "tables_touched": tables_touched,
            "configs_and_flags": configs_and_flags,
            "gaps": []
        }

        self.flows.append(flow)

    def _detect_flow_trigger(self, node):
        """Returns (kind, method, path, topic) or None"""
        for dec in node.decorator_list:
            dec_str = self._ast_to_str(dec)

            # FastAPI / Starlette / Flask route decorators
            # e.g. @app.get("/orders/{id}"), @router.post("/items")
            for m in ["get", "post", "put", "delete", "patch", "options", "head"]:
                if f".{m}(" in dec_str or dec_str.endswith(f".{m}"):
                    path = self._extract_first_arg_str(dec) or "/"
                    return ("http", m.upper(), path, None)

            # Flask route: @app.route("/orders", methods=["POST"])
            if ".route(" in dec_str:
                path = self._extract_first_arg_str(dec) or "/"
                method = self._extract_route_methods(dec) or "GET"
                return ("http", method.upper(), path, None)

            # Celery task
            if ".task" in dec_str or dec_str == "shared_task":
                return ("background_job", None, None, node.name)

            # Event subscriber / consumer decorators
            # e.g. @subscribe("OrderSubmittedIntegrationEvent"), @event_handler("...")
            for sub in ["subscribe", "event_handler", "consumer", "on_event", "listen"]:
                if sub in dec_str:
                    topic = self._extract_first_arg_str(dec) or node.name
                    return ("event_handler", None, None, topic)

            # Azure Functions trigger decorators
            if "service_bus" in dec_str or "event_hub" in dec_str or "queue_trigger" in dec_str:
                topic = self._extract_first_arg_str(dec) or node.name
                return ("event_handler", None, None, topic)

        # Convention fallback: function name like handle_order_submitted or consume_order
        lower_name = node.name.lower()
        if lower_name.startswith("handle_") or lower_name.startswith("consume_") or lower_name.startswith("on_"):
            # Check type annotation of first parameter
            if node.args.args:
                first_arg = node.args.args[0]
                if first_arg.annotation:
                    type_str = self._ast_to_str(first_arg.annotation)
                    if "event" in type_str.lower() or "message" in type_str.lower() or "command" in type_str.lower():
                        return ("event_handler", None, None, type_str)

        return None

    def _analyze_call(self, call_node):
        func_str = self._ast_to_str(call_node.func)
        func_lower = func_str.lower()

        # 1. Event publishing
        # e.g. bus.publish(OrderSubmittedIntegrationEvent(...)), producer.send("OrderSubmitted", ...), emit(...)
        for pub in ["publish", "publish_event", "send_event", "emit", "basic_publish"]:
            if pub in func_lower:
                event_name = self._extract_event_name_from_call(call_node)
                if event_name:
                    return ("event_publish", event_name)

        if "delay" in func_lower or "apply_async" in func_lower:
            target = func_str.split(".")[0]
            return ("event_publish", target)

        # 2. Database writes (SQLAlchemy, Django ORM, Mongo, etc.)
        if "session.add" in func_lower or "db.session.add" in func_lower:
            table = self._extract_first_arg_str_or_type(call_node)
            return ("db_write", table or "Database")

        if ".save(" in func_str or ".commit(" in func_str:
            target = func_str.split(".")[0]
            return ("db_write", target if target != "session" and target != "db" else "Database")

        if ".objects.create" in func_str or ".insert_one" in func_str or ".insert(" in func_str:
            table = func_str.split(".")[0]
            return ("db_write", table)

        # 3. Database reads
        if "session.query" in func_lower or ".objects.filter" in func_str or ".objects.get" in func_str:
            table = self._extract_first_arg_str_or_type(call_node) or func_str.split(".")[0]
            return ("db_read", table)

        # 4. External HTTP calls
        if any(h in func_lower for h in ["requests.get", "requests.post", "requests.put", "requests.delete", "httpx.get", "httpx.post", "aiohttp"]):
            url = self._extract_first_arg_str(call_node) or func_str
            return ("external_call", url)

        # 5. Method calls on domain aggregates (e.g. order.submit(), cart.checkout())
        parts = func_str.split(".")
        if len(parts) == 2 and not any(p in func_lower for p in ["self.", "logger.", "log.", "print", "str", "len"]):
            return ("call", f"{func_str}()")

        return None

    def _extract_event_name_from_call(self, call_node):
        if not call_node.args:
            return None
        first_arg = call_node.args[0]
        # Literal string: bus.publish("OrderSubmittedIntegrationEvent", ...)
        if isinstance(first_arg, ast.Constant) and isinstance(first_arg.value, str):
            return first_arg.value
        # Instantiation: bus.publish(OrderSubmittedIntegrationEvent(...))
        if isinstance(first_arg, ast.Call):
            return self._ast_to_str(first_arg.func)
        # Variable name
        if isinstance(first_arg, ast.Name):
            return first_arg.id
        return None

    def _extract_flag_name(self, test_node):
        test_str = self._ast_to_str(test_node)
        test_lower = test_str.lower()
        if any(w in test_lower for w in ["flag", "feature", "config", "settings", "enable", "is_enabled"]):
            # Extract clean token
            parts = test_str.replace("(", " ").replace(")", " ").replace(".", " ").replace("!", " ").replace("'", " ").replace('"', " ").split()
            for p in parts:
                p_lower = p.lower()
                if "enable" in p_lower or "flag" in p_lower or "feature" in p_lower or "auto" in p_lower:
                    return p
            return test_str.strip()
        return None

    def _extract_first_arg_str_or_type(self, call_node):
        if not call_node.args:
            return None
        arg = call_node.args[0]
        if isinstance(arg, ast.Constant) and isinstance(arg.value, str):
            return arg.value
        if isinstance(arg, ast.Name):
            return arg.id
        if isinstance(arg, ast.Call):
            return self._ast_to_str(arg.func)
        return None

    def _extract_first_arg_str(self, node):
        if hasattr(node, "args") and node.args:
            first = node.args[0]
            if isinstance(first, ast.Constant) and isinstance(first.value, str):
                return first.value
        return None

    def _extract_route_methods(self, dec_node):
        if hasattr(dec_node, "keywords"):
            for kw in dec_node.keywords:
                if kw.arg == "methods" and isinstance(kw.value, (ast.List, ast.Tuple, ast.Set)):
                    for el in kw.value.elts:
                        if isinstance(el, ast.Constant) and isinstance(el.value, str):
                            return el.value
        return None

    def _ast_to_str(self, node):
        if isinstance(node, ast.Name):
            return node.id
        elif isinstance(node, ast.Attribute):
            return f"{self._ast_to_str(node.value)}.{node.attr}"
        elif isinstance(node, ast.Call):
            func_str = self._ast_to_str(node.func)
            return f"{func_str}(...)"
        elif isinstance(node, ast.Constant):
            return repr(node.value)
        return ""

    def _infer_domain(self, text):
        clean = text.strip("/").replace("{", "").replace("}", "")
        parts = [p for p in clean.split("/") if p and p not in ["api", "v1", "v2"]]
        domain_str = parts[0] if parts else "Core"
        domain_str = domain_str.split("_")[0].split("-")[0].capitalize()
        return domain_str, domain_str.lower()

    def _infer_scenario(self, method, text):
        clean = text.strip("/").replace("{", "").replace("}", "").replace("/", " ")
        if method:
            return f"{method.upper()} {clean}"
        return clean.capitalize()


def extract_project(project_dir: str, out_dir: str):
    project_path = Path(project_dir).resolve()
    out_path = Path(out_dir).resolve()
    out_path.mkdir(parents=True, exist_ok=True)

    ignore_dirs = {
        ".git", "venv", ".venv", "__pycache__", "node_modules",
        ".idea", ".vscode", "build", "dist", ".pytest_cache"
    }

    all_flows = []
    total_files = 0

    for root, dirs, files in os.walk(project_path):
        dirs[:] = [d for d in dirs if d not in ignore_dirs]
        for f in files:
            if f.endswith(".py"):
                full_path = os.path.join(root, f)
                rel_path = os.path.relpath(full_path, project_path)
                total_files += 1

                try:
                    with open(full_path, "r", encoding="utf-8") as py_file:
                        source = py_file.read()
                    tree = ast.parse(source, filename=full_path)
                    visitor = PythonFlowExtractor(full_path, rel_path)
                    visitor.visit(tree)
                    all_flows.extend(visitor.flows)
                except Exception as e:
                    print(f"Warning: Failed to parse Python file {rel_path}: {e}", file=sys.stderr)

    for flow in all_flows:
        clean_part = flow["trigger"].get("path") or flow["id"]
        for ch in ['/', '\\', ':', '<', '>', '"', '|', '?', '*', '{', '}', ' ']:
            clean_part = clean_part.replace(ch, "_")
        filename = f"{flow['domain_id']}_{flow['trigger'].get('method', 'FLOW')}_{clean_part}.json"
        target_file = out_path / filename
        with open(target_file, "w", encoding="utf-8") as out_f:
            json.dump(flow, out_f, indent=2)

    print(f"Extracted {len(all_flows)} Python flow(s) across {total_files} file(s) into {out_path}")


if __name__ == "__main__":
    if len(sys.argv) < 3:
        print("Usage: extractor.py <project_dir> <out_dir>", file=sys.stderr)
        sys.exit(1)
    extract_project(sys.argv[1], sys.argv[2])
