using System.Reflection;
using System.Text.Json;
using System.Text.Json.Serialization;
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.CSharp;
using Microsoft.CodeAnalysis.CSharp.Syntax;

// Usage:
//   CSharpFlowExtractor <projectDir> <outDir> [--refs <dirWithDlls>] [--only <ControllerName>]

var opts = Cli.Parse(args);
Directory.CreateDirectory(opts.OutDir);

var trees = Directory.EnumerateFiles(opts.ProjectDir, "*.cs", SearchOption.AllDirectories)
    .Where(f => !f.Contains($"{Path.DirectorySeparatorChar}obj{Path.DirectorySeparatorChar}")
             && !f.Contains($"{Path.DirectorySeparatorChar}bin{Path.DirectorySeparatorChar}")
             && !f.Contains($"{Path.DirectorySeparatorChar}Migrations{Path.DirectorySeparatorChar}"))
    .Select(f => CSharpSyntaxTree.ParseText(File.ReadAllText(f), path: f))
    .ToList();

// ImplicitUsings: reuse the project's generated global usings, else fall back to Web SDK defaults.
var gu = Directory.EnumerateFiles(opts.ProjectDir, "*.GlobalUsings.g.cs", SearchOption.AllDirectories).FirstOrDefault();
trees.Add(CSharpSyntaxTree.ParseText(gu != null ? File.ReadAllText(gu) : string.Join("\n",
    new[] { "System", "System.Collections.Generic", "System.IO", "System.Linq", "System.Net.Http", "System.Threading", "System.Threading.Tasks",
            "System.Net.Http.Json", "Microsoft.AspNetCore.Builder", "Microsoft.AspNetCore.Hosting", "Microsoft.AspNetCore.Http",
            "Microsoft.AspNetCore.Routing", "Microsoft.Extensions.Configuration", "Microsoft.Extensions.DependencyInjection",
            "Microsoft.Extensions.Hosting", "Microsoft.Extensions.Logging" }.Select(n => $"global using global::{n};")), path: "<implicit-usings>"));

var projectAsm = Path.GetFileName(opts.ProjectDir.TrimEnd('\\', '/'));

var refs = new List<MetadataReference>();
var refDir = opts.RefsDir;
if (refDir != null && Directory.Exists(refDir))
{
    foreach (var dll in Directory.EnumerateFiles(refDir, "*.dll"))
    {
        if (Path.GetFileNameWithoutExtension(dll).Equals(projectAsm, StringComparison.OrdinalIgnoreCase)) continue;
        try { _ = AssemblyName.GetAssemblyName(dll); refs.Add(MetadataReference.CreateFromFile(dll)); } catch { /* native dll */ }
    }
}
var tpa = ((string?)AppContext.GetData("TRUSTED_PLATFORM_ASSEMBLIES"))?.Split(Path.PathSeparator) ?? [];
var dotnetRoot = Path.GetFullPath(Path.Combine(Path.GetDirectoryName(typeof(object).Assembly.Location)!, "..", ".."));
var aspDir = Path.Combine(dotnetRoot, "Microsoft.AspNetCore.App");
if (Directory.Exists(aspDir))
{
    var best = Directory.GetDirectories(aspDir).OrderByDescending(d => d).FirstOrDefault(d => Path.GetFileName(d).StartsWith("9."));
    if (best != null)
        foreach (var dll in Directory.EnumerateFiles(best, "*.dll"))
        {
            var n = Path.GetFileName(dll);
            if (!refs.Any(r => Path.GetFileName(((PortableExecutableReference)r).FilePath!) == n))
                try { _ = AssemblyName.GetAssemblyName(dll); refs.Add(MetadataReference.CreateFromFile(dll)); } catch { }
        }
}
foreach (var p in tpa)
{
    var n = Path.GetFileName(p);
    if (!refs.Any(r => Path.GetFileName(((PortableExecutableReference)r).FilePath!) == n))
        refs.Add(MetadataReference.CreateFromFile(p));
}

var compilation = CSharpCompilation.Create("flow", trees, refs,
    new CSharpCompilationOptions(OutputKind.DynamicallyLinkedLibrary, nullableContextOptions: NullableContextOptions.Enable));

// 1. Comprehensive Type & Method Registry
var methodIndex = new Dictionary<IMethodSymbol, MethodDeclarationSyntax>(SymbolEqualityComparer.Default);
var interfaceImplementations = new Dictionary<string, List<string>>(StringComparer.OrdinalIgnoreCase);
var classByName = new Dictionary<string, ClassDeclarationSyntax>(StringComparer.OrdinalIgnoreCase);
var methodsByClassAndName = new Dictionary<string, MethodDeclarationSyntax>(StringComparer.OrdinalIgnoreCase);
var methodsByName = new Dictionary<string, List<MethodDeclarationSyntax>>(StringComparer.OrdinalIgnoreCase);

foreach (var t in trees)
{
    var sm = compilation.GetSemanticModel(t);
    var root = t.GetRoot();
    foreach (var cls in root.DescendantNodes().OfType<ClassDeclarationSyntax>())
    {
        classByName[cls.Identifier.Text] = cls;
        if (cls.BaseList != null)
        {
            foreach (var bt in cls.BaseList.Types)
            {
                var raw = bt.Type.ToString();
                var iface = raw.Split('<')[0].Split('.').Last();
                if (!interfaceImplementations.ContainsKey(iface))
                    interfaceImplementations[iface] = new();
                if (!interfaceImplementations[iface].Contains(cls.Identifier.Text))
                    interfaceImplementations[iface].Add(cls.Identifier.Text);
            }
        }
        foreach (var m in cls.Members.OfType<MethodDeclarationSyntax>())
        {
            if (sm.GetDeclaredSymbol(m) is IMethodSymbol ms)
                methodIndex[ms.OriginalDefinition] = m;
            var fullKey = $"{cls.Identifier.Text}.{m.Identifier.Text}";
            methodsByClassAndName[fullKey] = m;
            if (!methodsByName.ContainsKey(m.Identifier.Text))
                methodsByName[m.Identifier.Text] = new();
            methodsByName[m.Identifier.Text].Add(m);
        }
    }
}

var registry = new TypeRegistry(methodIndex, interfaceImplementations, classByName, methodsByClassAndName, methodsByName);

string[] httpAttrs = ["HttpGet", "HttpPost", "HttpPut", "HttpPatch", "HttpDelete"];
int count = 0;

// 2. Scan HTTP Controllers
foreach (var tree in trees)
{
    var model = compilation.GetSemanticModel(tree);
    foreach (var cls in tree.GetRoot().DescendantNodes().OfType<ClassDeclarationSyntax>())
    {
        if (opts.Only != null && cls.Identifier.Text != opts.Only) continue;
        var classRoute = cls.AttributeLists.SelectMany(a => a.Attributes).FirstOrDefault(a => AttrName(a) == "Route");
        var prefix = classRoute?.ArgumentList?.Arguments.FirstOrDefault()?.ToString().Trim('"') ?? "";
        foreach (var m in cls.Members.OfType<MethodDeclarationSyntax>())
        {
            var http = m.AttributeLists.SelectMany(a => a.Attributes).FirstOrDefault(a => httpAttrs.Contains(AttrName(a)));
            if (http == null) continue;
            var verb = AttrName(http)!.Replace("Http", "").ToUpperInvariant();
            var tmpl = http.ArgumentList?.Arguments.FirstOrDefault()?.ToString().Trim('"') ?? "";
            var path = string.Join('/', new[] { prefix, tmpl }.Where(s => s.Length > 0));
            var flow = FlowBuilder.Build(compilation, model, cls, m, verb, path, tree.FilePath, registry);
            var file = Path.Combine(opts.OutDir, $"{cls.Identifier.Text}.{m.Identifier.Text}.json");
            File.WriteAllText(file, JsonSerializer.Serialize(flow, Json.Options));
            count++;
        }
    }
}

// 3. Scan Event Handlers & Message Consumers (IEventHandler<T>, IConsumer<T>, INotificationHandler<T>)
foreach (var tree in trees)
{
    var model = compilation.GetSemanticModel(tree);
    foreach (var cls in tree.GetRoot().DescendantNodes().OfType<ClassDeclarationSyntax>())
    {
        if (cls.BaseList == null) continue;
        foreach (var baseType in cls.BaseList.Types)
        {
            var raw = baseType.Type.ToString();
            if (raw.Contains("IEventHandler<") || raw.Contains("IConsumer<") || raw.Contains("INotificationHandler<"))
            {
                var eventTypeName = raw.Substring(raw.IndexOf('<') + 1).TrimEnd('>').Split('.').Last();
                var handleMethod = cls.Members.OfType<MethodDeclarationSyntax>()
                    .FirstOrDefault(m => m.Identifier.Text is "HandleAsync" or "Handle" or "Consume");
                if (handleMethod == null) continue;

                var flow = FlowBuilder.BuildEventHandler(compilation, model, cls, handleMethod, eventTypeName, tree.FilePath, registry);
                var file = Path.Combine(opts.OutDir, $"{cls.Identifier.Text}.{handleMethod.Identifier.Text}.json");
                File.WriteAllText(file, JsonSerializer.Serialize(flow, Json.Options));
                count++;
            }
        }
    }
}

Console.WriteLine($"Wrote {count} flows to {opts.OutDir}");
var errs = compilation.GetDiagnostics().Count(d => d.Severity == DiagnosticSeverity.Error);
Console.WriteLine($"(compilation diagnostics: {errs} errors; unresolved symbols lower confidence)");

static string? AttrName(AttributeSyntax a) => a.Name.ToString().Split('.').Last().Replace("Attribute", "");

record Cli(string ProjectDir, string OutDir, string? RefsDir, string? Only)
{
    public static Cli Parse(string[] a)
    {
        if (a.Length < 2) { Console.Error.WriteLine("usage: <projectDir> <outDir> [--refs dir] [--only Controller]"); Environment.Exit(1); }
        string? refs = null, only = null;
        for (int i = 2; i < a.Length - 1; i++)
        {
            if (a[i] == "--refs") refs = a[i + 1];
            if (a[i] == "--only") only = a[i + 1];
        }
        return new(a[0], a[1], refs, only);
    }
}

static class Json
{
    public static readonly JsonSerializerOptions Options = new()
    {
        WriteIndented = true,
        DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull,
    };
}

record TypeRegistry(
    Dictionary<IMethodSymbol, MethodDeclarationSyntax> MethodIndex,
    Dictionary<string, List<string>> InterfaceImplementations,
    Dictionary<string, ClassDeclarationSyntax> ClassByName,
    Dictionary<string, MethodDeclarationSyntax> MethodsByClassAndName,
    Dictionary<string, List<MethodDeclarationSyntax>> MethodsByName
);

static class FlowBuilder
{
    const int MaxDepth = 5;

    public static Dictionary<string, object?> Build(
        Compilation comp, SemanticModel model, ClassDeclarationSyntax cls, MethodDeclarationSyntax m,
        string verb, string path, string file, TypeRegistry registry)
    {
        var guards = new List<object>();
        foreach (var a in cls.AttributeLists.Concat(m.AttributeLists).SelectMany(l => l.Attributes))
        {
            var n = a.Name.ToString().Split('.').Last().Replace("Attribute", "");
            if (n is "Authorize") guards.Add(Step("decision", ("kind", "authn"), ("condition", a.ToString()), ("confidence", "extracted")));
            if (n is "RequireFeature") guards.Add(Step("decision", ("kind", "feature_flag"), ("condition", a.ToString()), ("confidence", "extracted")));
            if (n is "RequireRole" or "RequirePermission" or "AllowAnonymous")
                guards.Add(Step("decision", ("kind", "authz"), ("condition", a.ToString()), ("confidence", "extracted")));
        }

        var visited = new HashSet<MethodDeclarationSyntax>();
        var ctx = new Ctx(comp, registry, visited);
        var steps = m.Body != null ? ctx.WalkBlock(m.Body, model, 0)
                  : m.ExpressionBody != null ? ctx.ExprSteps(m.ExpressionBody.Expression, model, 0)
                  : new List<object>();

        var domainId = cls.Identifier.Text.ToLower().Replace("controller", "");
        var domainName = cls.Identifier.Text.Replace("Controller", "") + " Operations";
        if (domainId.Contains("weather") || domainId.Contains("telemetry")) { domainId = "weather"; domainName = "Weather & Telemetry"; }
        else if (domainId.Contains("alert") || domainId.Contains("notification")) { domainId = "alerts"; domainName = "Alerts & Notifications"; }
        else if (domainId.Contains("station") || domainId.Contains("sensor")) { domainId = "stations"; domainName = "Weather Stations & Sensors"; }
        else if (domainId.Contains("forecast")) { domainId = "forecast"; domainName = "Forecasting Services"; }

        var scenario = $"{verb} {cls.Identifier.Text.Replace("Controller", "")} {m.Identifier.Text}";

        var ctor = cls.Members.OfType<ConstructorDeclarationSyntax>().FirstOrDefault();
        var deps = new List<object>();
        if (ctor != null)
        {
            foreach (var p in ctor.ParameterList.Parameters)
            {
                var pType = p.Type?.ToString() ?? "";
                var kind = pType.Contains("DbContext") ? "dbcontext" : pType.Contains("Client") ? "client" : pType.Contains("Service") ? "service" : "dependency";
                deps.Add(new { name = pType, kind, source = "Constructor DI" });
            }
        }

        var reqBodyParam = m.ParameterList.Parameters.FirstOrDefault(p =>
            p.AttributeLists.Any(a => a.Attributes.Any(x => x.Name.ToString().Contains("FromBody")))
            || (p.Type != null && !p.Type.ToString().StartsWith("int") && !p.Type.ToString().StartsWith("string") && !p.Type.ToString().StartsWith("Guid") && !p.Type.ToString().StartsWith("CancellationToken")));

        object? reqBodyInfo = null;
        if (reqBodyParam != null && reqBodyParam.Type != null)
        {
            var reqTypeName = reqBodyParam.Type.ToString().Split('.').Last();
            var reqClass = registry.ClassByName.GetValueOrDefault(reqTypeName);

            var fields = new List<object>();
            if (reqClass != null)
            {
                foreach (var prop in reqClass.Members.OfType<PropertyDeclarationSyntax>())
                {
                    fields.Add(new
                    {
                        name = prop.Identifier.Text,
                        field_type = prop.Type.ToString(),
                        required = !prop.Type.ToString().EndsWith("?"),
                        description = (string?)null
                    });
                }
            }
            reqBodyInfo = new
            {
                type_name = reqTypeName,
                fields,
            };
        }

        var statusCodes = new List<object>
        {
            new { code = verb == "POST" ? 201 : 200, description = "Success" }
        };

        var couplingObj = Coupling(steps);

        var tablesTouched = new List<object>();
        var couplingDict = JsonSerializer.Deserialize<Dictionary<string, JsonElement>>(JsonSerializer.Serialize(couplingObj)) ?? new();
        if (couplingDict.TryGetValue("reads", out var readsEl) && readsEl.ValueKind == JsonValueKind.Array)
        {
            foreach (var r in readsEl.EnumerateArray())
            {
                tablesTouched.Add(new { table = r.GetString() ?? "", operation = "SELECT", transactional = false });
            }
        }
        if (couplingDict.TryGetValue("writes", out var writesEl) && writesEl.ValueKind == JsonValueKind.Array)
        {
            foreach (var w in writesEl.EnumerateArray())
            {
                var op = verb == "POST" ? "INSERT" : verb == "DELETE" ? "DELETE" : "UPDATE";
                tablesTouched.Add(new { table = w.GetString() ?? "", operation = op, transactional = true });
            }
        }

        return new()
        {
            ["id"] = $"{verb} {path}",
            ["kind"] = "http",
            ["domain"] = domainName,
            ["domain_id"] = domainId,
            ["scenario"] = scenario,
            ["trigger"] = new { method = verb, path, declared_in = $"{cls.Identifier.Text}.{m.Identifier.Text}" },
            ["handler"] = new
            {
                symbol = $"{cls.Identifier.Text}.{m.Identifier.Text}",
                file,
                line = Line(m),
                signature = $"{m.Identifier.Text}({string.Join(", ", m.ParameterList.Parameters.Select(p => p.ToString()))}) -> {m.ReturnType}",
            },
            ["contract"] = new
            {
                request_body = reqBodyInfo,
                parameters = m.ParameterList.Parameters
                    .Where(p => p != reqBodyParam && p.Type?.ToString() != "CancellationToken")
                    .Select(p => new { source = "param", name = p.Identifier.Text, type = p.Type?.ToString(), description = (string?)null }),
                response = new { type_name = m.ReturnType?.ToString(), status_codes = statusCodes },
                dependencies = deps,
            },
            ["input"] = m.ParameterList.Parameters
                .Where(p => p.AttributeLists.Any(a => a.Attributes.Any(x => x.Name.ToString().StartsWith("From"))))
                .Select(p => new { source = p.AttributeLists.First().Attributes.First().Name.ToString().Replace("From", "").ToLower(), name = p.Identifier.Text, type = p.Type?.ToString() }),
            ["guards"] = guards,
            ["steps"] = steps,
            ["coupling"] = couplingObj,
            ["tables_touched"] = tablesTouched,
        };
    }

    public static Dictionary<string, object?> BuildEventHandler(
        Compilation comp, SemanticModel model, ClassDeclarationSyntax cls, MethodDeclarationSyntax m,
        string eventTypeName, string file, TypeRegistry registry)
    {
        var visited = new HashSet<MethodDeclarationSyntax>();
        var ctx = new Ctx(comp, registry, visited);
        var steps = m.Body != null ? ctx.WalkBlock(m.Body, model, 0)
                  : m.ExpressionBody != null ? ctx.ExprSteps(m.ExpressionBody.Expression, model, 0)
                  : new List<object>();

        var domainId = "events";
        var domainName = "Event Handlers";
        var normFile = file.Replace('\\', '/');
        if (normFile.Contains("/Weather/") || normFile.Contains("/Telemetry/")) { domainId = "weather"; domainName = "Weather & Telemetry"; }
        else if (normFile.Contains("/Alerts/") || normFile.Contains("/Notifications/")) { domainId = "alerts"; domainName = "Alerts & Notifications"; }
        else if (normFile.Contains("/Stations/")) { domainId = "stations"; domainName = "Weather Stations & Sensors"; }
        else if (normFile.Contains("/Forecast/")) { domainId = "forecast"; domainName = "Forecasting Services"; }

        var scenario = $"Handle {eventTypeName} ({cls.Identifier.Text})";
        var ctor = cls.Members.OfType<ConstructorDeclarationSyntax>().FirstOrDefault();
        var deps = new List<object>();
        if (ctor != null)
        {
            foreach (var p in ctor.ParameterList.Parameters)
            {
                var pType = p.Type?.ToString() ?? "";
                var kind = pType.Contains("DbContext") ? "dbcontext" : pType.Contains("Client") ? "client" : pType.Contains("Service") ? "service" : "dependency";
                deps.Add(new { name = pType, kind, source = "Constructor DI" });
            }
        }

        var couplingObj = Coupling(steps, new List<string> { eventTypeName });

        var tablesTouched = new List<object>();
        var couplingDict = JsonSerializer.Deserialize<Dictionary<string, JsonElement>>(JsonSerializer.Serialize(couplingObj)) ?? new();
        if (couplingDict.TryGetValue("reads", out var readsEl) && readsEl.ValueKind == JsonValueKind.Array)
        {
            foreach (var r in readsEl.EnumerateArray())
                tablesTouched.Add(new { table = r.GetString() ?? "", operation = "SELECT", transactional = false });
        }
        if (couplingDict.TryGetValue("writes", out var writesEl) && writesEl.ValueKind == JsonValueKind.Array)
        {
            foreach (var w in writesEl.EnumerateArray())
                tablesTouched.Add(new { table = w.GetString() ?? "", operation = "UPDATE", transactional = true });
        }

        return new()
        {
            ["id"] = $"EVENT {eventTypeName} -> {cls.Identifier.Text}",
            ["kind"] = "event",
            ["domain"] = domainName,
            ["domain_id"] = domainId,
            ["scenario"] = scenario,
            ["trigger"] = new { topic = eventTypeName, declared_in = $"{cls.Identifier.Text}.{m.Identifier.Text}" },
            ["handler"] = new
            {
                symbol = $"{cls.Identifier.Text}.{m.Identifier.Text}",
                file,
                line = Line(m),
                signature = $"{m.Identifier.Text}({string.Join(", ", m.ParameterList.Parameters.Select(p => p.ToString()))}) -> {m.ReturnType}",
            },
            ["contract"] = new
            {
                parameters = new[] { new { source = "event_message", name = "@event", type = eventTypeName, description = "Domain Event Payload" } },
                response = new { type_name = m.ReturnType?.ToString(), status_codes = new[] { new { code = 200, description = "Acknowledged / Handled" } } },
                dependencies = deps,
            },
            ["input"] = new[] { new { source = "event", name = "@event", type = eventTypeName } },
            ["guards"] = new List<object>(),
            ["steps"] = steps,
            ["coupling"] = couplingObj,
            ["tables_touched"] = tablesTouched,
        };
    }

    static object Coupling(List<object> steps, List<string>? eventsConsumed = null)
    {
        var all = new List<Dictionary<string, object?>>();
        void Collect(object? o)
        {
            switch (o)
            {
                case Dictionary<string, object?> d:
                    all.Add(d);
                    foreach (var v in d.Values) Collect(v);
                    break;
                case System.Collections.IEnumerable e when o is not string:
                    foreach (var x in e) Collect(x);
                    break;
            }
        }
        Collect(steps);
        List<string> Targets(string kind, string? op = null) => all
            .Where(d => d.GetValueOrDefault("kind") as string == kind && (op == null || (d.GetValueOrDefault("op") as string)?.StartsWith(op) == true))
            .Select(d => d.GetValueOrDefault("target") as string ?? "?").Distinct().OrderBy(x => x).ToList();
        return new
        {
            reads = Targets("db_read"),
            writes = Targets("db_write"),
            events_published = Targets("event_publish"),
            events_consumed = eventsConsumed ?? new List<string>(),
            calls_services = Targets("http_call"),
        };
    }

    public static int Line(SyntaxNode n) => n.GetLocation().GetLineSpan().StartLinePosition.Line + 1;

    public static Dictionary<string, object?> Step(string type, params (string, object?)[] kv)
    {
        var d = new Dictionary<string, object?> { ["type"] = type };
        foreach (var (k, v) in kv) d[k] = v;
        return d;
    }

    sealed class Ctx(Compilation comp, TypeRegistry registry, HashSet<MethodDeclarationSyntax> visited)
    {
        public List<object> WalkBlock(BlockSyntax? block, SemanticModel model, int depth)
        {
            var steps = new List<object>();
            if (block == null) return steps;
            foreach (var s in block.Statements) steps.AddRange(WalkStatement(s, model, depth));
            return steps;
        }

        List<object> WalkStatement(StatementSyntax s, SemanticModel model, int depth)
        {
            var steps = new List<object>();
            switch (s)
            {
                case BlockSyntax b:
                    return WalkBlock(b, model, depth);

                case IfStatementSyntax ifs:
                {
                    steps.AddRange(ExprSteps(ifs.Condition, model, depth));
                    var branches = new List<object>
                    {
                        new Dictionary<string, object?> { ["when"] = "true", ["steps"] = WalkStatement(ifs.Statement, model, depth) },
                    };
                    if (ifs.Else != null)
                        branches.Add(new Dictionary<string, object?> { ["when"] = "false", ["steps"] = WalkStatement(ifs.Else.Statement, model, depth) });
                    steps.Add(Step("decision",
                        ("kind", ClassifyCondition(ifs.Condition, model)),
                        ("condition", Short(ifs.Condition)),
                        ("line", Line(ifs)),
                        ("branches", branches),
                        ("confidence", "extracted")));
                    return steps;
                }

                case SwitchStatementSyntax sw:
                {
                    var branches = sw.Sections.Select(sec => (object)new Dictionary<string, object?>
                    {
                        ["when"] = string.Join(", ", sec.Labels.Select(l => l.ToString().Replace("\r", "").Replace("\n", " "))),
                        ["steps"] = sec.Statements.SelectMany(x => WalkStatement(x, model, depth)).ToList(),
                    }).ToList();
                    steps.Add(Step("decision", ("kind", "state_check"), ("condition", $"switch ({Short(sw.Expression)})"), ("line", Line(sw)), ("branches", branches), ("confidence", "extracted")));
                    return steps;
                }

                case ForEachStatementSyntax fe:
                    steps.AddRange(ExprSteps(fe.Expression, model, depth));
                    steps.Add(Step("loop", ("over", Short(fe.Expression)), ("line", Line(fe)), ("steps", WalkStatement(fe.Statement, model, depth))));
                    return steps;
                case ForStatementSyntax fo:
                    steps.Add(Step("loop", ("over", fo.Condition?.ToString()), ("line", Line(fo)), ("steps", WalkStatement(fo.Statement, model, depth))));
                    return steps;
                case WhileStatementSyntax wh:
                    steps.Add(Step("loop", ("over", Short(wh.Condition)), ("line", Line(wh)), ("steps", WalkStatement(wh.Statement, model, depth))));
                    return steps;

                case TryStatementSyntax tr:
                    steps.AddRange(WalkBlock(tr.Block, model, depth));
                    foreach (var c in tr.Catches)
                        steps.Add(Step("decision", ("kind", "error_handling"), ("condition", $"catch {c.Declaration?.Type}"), ("line", Line(c)),
                            ("branches", new List<object> { new Dictionary<string, object?> { ["when"] = "thrown", ["steps"] = WalkBlock(c.Block, model, depth) } })));
                    return steps;

                case ReturnStatementSyntax r:
                    if (r.Expression != null) steps.AddRange(ExprSteps(r.Expression, model, depth));
                    steps.Add(Step("return", ("status", ReturnStatus(r.Expression, model)), ("expr", Short(r.Expression)), ("line", Line(r))));
                    return steps;

                case ThrowStatementSyntax th:
                    steps.Add(Step("return", ("status", $"throws {th.Expression?.ToString().Split('(')[0].Replace("new ", "")}"), ("line", Line(th))));
                    return steps;

                case LocalDeclarationStatementSyntax ld:
                    foreach (var v in ld.Declaration.Variables)
                        if (v.Initializer != null)
                        {
                            steps.AddRange(ExprSteps(v.Initializer.Value, model, depth));
                            if (v.Initializer.Value.ToString().Contains("Config") && v.Initializer.Value.ToString().Contains("??"))
                                steps.Add(Step("decision", ("kind", "config"), ("condition", Short(v.Initializer.Value)), ("assigns", v.Identifier.Text), ("line", Line(v)), ("confidence", "extracted")));
                        }
                    return steps;

                case ExpressionStatementSyntax es:
                    if (es.Expression is AssignmentExpressionSyntax asg && asg.Left is MemberAccessExpressionSyntax)
                        steps.Add(Step("effect", ("kind", "state_mutation"), ("target", Short(asg.Left)), ("op", $"= {Short(asg.Right)}"), ("line", Line(es)), ("confidence", "extracted")));
                    steps.AddRange(ExprSteps(es.Expression, model, depth));
                    return steps;
            }
            return steps;
        }

        public List<object> ExprSteps(ExpressionSyntax expr, SemanticModel model, int depth)
        {
            var steps = new List<object>();
            var invs = expr.DescendantNodesAndSelf(n => n is not (LambdaExpressionSyntax or AnonymousMethodExpressionSyntax))
                .OfType<InvocationExpressionSyntax>()
                .Where(i => !(i.Parent is MemberAccessExpressionSyntax ma && ma.Parent is InvocationExpressionSyntax outer && outer.Expression == ma))
                .ToList();
            foreach (var inv in invs) steps.AddRange(Classify(inv, model, depth));
            return steps;
        }

        List<object> Classify(InvocationExpressionSyntax inv, SemanticModel model, int depth)
        {
            var steps = new List<object>();
            var name = inv.Expression switch
            {
                MemberAccessExpressionSyntax ma => ma.Name.Identifier.Text,
                IdentifierNameSyntax id => id.Identifier.Text,
                GenericNameSyntax g => g.Identifier.Text,
                _ => "",
            };
            var line = Line(inv);
            var chainRoot = ChainRoot(inv);
            var sym = model.GetSymbolInfo(inv).Symbol as IMethodSymbol;

            // DbContext.SaveChanges*
            if (name.StartsWith("SaveChanges"))
                return [Step("effect", ("kind", "db_write"), ("target", "DbContext"), ("op", name), ("transactional", true), ("line", line), ("confidence", "resolved"))];

            // Direct DbSet mutation or query: _dbContext.Quotes.Add / FirstOrDefaultAsync / etc.
            var dbSet = DbSetOf(chainRoot, model);
            if (dbSet != null)
            {
                var write = name is "Add" or "AddAsync" or "AddRange" or "Remove" or "RemoveRange" or "Update" or "ExecuteUpdateAsync" or "ExecuteDeleteAsync";
                return [Step("effect", ("kind", write ? "db_write" : "db_read"), ("target", dbSet), ("op", name), ("line", line), ("confidence", "resolved"))];
            }

            // dbContext.Add(entity) or dbContext.Update(entity) where target is on DbContext itself
            if (chainRoot.ToString().ToLower().Contains("dbcontext") && (name is "Add" or "AddAsync" or "AddRange" or "Remove" or "RemoveRange" or "Update"))
            {
                var arg = inv.ArgumentList.Arguments.FirstOrDefault()?.Expression;
                string? targetTable = null;
                if (arg is ObjectCreationExpressionSyntax oc)
                    targetTable = oc.Type.ToString().Split('.').Last();
                else if (arg != null)
                {
                    var t = model.GetTypeInfo(arg).Type?.Name;
                    if (t != null && t != "Object") targetTable = t;
                    else
                    {
                        var argText = arg.ToString();
                        var localDecl = inv.Ancestors().OfType<BlockSyntax>().SelectMany(b => b.Statements.OfType<LocalDeclarationStatementSyntax>())
                            .SelectMany(ld => ld.Declaration.Variables)
                            .FirstOrDefault(v => v.Identifier.Text == argText);
                        if (localDecl?.Initializer?.Value is ObjectCreationExpressionSyntax oc2)
                            targetTable = oc2.Type.ToString().Split('.').Last();
                    }
                }
                if (targetTable != null)
                {
                    var tablePlural = targetTable.EndsWith("s") ? targetTable : $"{targetTable}s";
                    return [Step("effect", ("kind", "db_write"), ("target", tablePlural), ("op", name), ("line", line), ("confidence", "resolved"))];
                }
            }

            // Domain Event: AddDomainEvent(new SomeEvent(...))
            if (name == "AddDomainEvent")
            {
                var ev = inv.ArgumentList.Arguments.FirstOrDefault()?.Expression;
                var evType = ev is ObjectCreationExpressionSyntax oc ? oc.Type.ToString().Split('.').Last() : Short(ev)?.Split('.').Last();
                if (evType != null && evType.Contains("(")) evType = evType.Split('(')[0].Trim();
                return [Step("effect", ("kind", "event_publish"), ("target", evType ?? "DomainEvent"), ("op", "AddDomainEvent"), ("deferred_until", "SaveChanges"), ("line", line), ("confidence", "extracted"))];
            }

            // Event Bus / Messaging: Publish / PublishAsync / Send
            if (name is "Publish" or "PublishAsync" or "Send" or "SendAsync")
            {
                string? evType = null;
                if (inv.Expression is MemberAccessExpressionSyntax ma && ma.Name is GenericNameSyntax gn)
                    evType = gn.TypeArgumentList.Arguments.FirstOrDefault()?.ToString().Split('.').Last();
                if (evType == null)
                {
                    var ev = inv.ArgumentList.Arguments.FirstOrDefault()?.Expression;
                    if (ev is ObjectCreationExpressionSyntax oc) evType = oc.Type.ToString().Split('.').Last();
                    else if (ev != null)
                    {
                        var tInfo = model.GetTypeInfo(ev).Type?.Name;
                        if (tInfo != null && tInfo != "Object") evType = tInfo;
                    }
                }
                if (evType != null)
                {
                    return [Step("effect", ("kind", "event_publish"), ("target", evType), ("op", name), ("line", line), ("confidence", "resolved"))];
                }
            }

            var recvType = ReceiverType(inv, model);
            var recvName = recvType == null ? null : recvType.Split('<')[0];
            if (recvName != null && recvName.Contains("ILogger"))
                return [Step("effect", ("kind", "log"), ("target", "ILogger"), ("op", name), ("line", line), ("confidence", "resolved"))];
            if (recvName != null && (recvName.EndsWith("ApiClient") || recvName.EndsWith("HttpClient")))
                return [Step("effect", ("kind", "http_call"), ("target", $"{recvName}.{name}"), ("line", line), ("confidence", "resolved"))];

            // 4. Resolve In-Source Methods, Interface Implementations, and Domain Model Methods
            MethodDeclarationSyntax? targetDecl = null;
            string? targetClassName = null;

            // Step A: Roslyn symbol resolution
            if (sym != null)
            {
                if (sym.ContainingType.TypeKind == TypeKind.Interface)
                {
                    var ifaceName = sym.ContainingType.Name;
                    var implName = registry.InterfaceImplementations.TryGetValue(ifaceName, out var impls) && impls.Count > 0
                        ? impls[0]
                        : (ifaceName.StartsWith("I") && registry.ClassByName.ContainsKey(ifaceName[1..]) ? ifaceName[1..] : null);

                    if (implName != null && registry.MethodsByClassAndName.TryGetValue($"{implName}.{sym.Name}", out var implDecl))
                    {
                        targetDecl = implDecl;
                        targetClassName = implName;
                    }
                }
                else if (registry.MethodIndex.TryGetValue(sym.OriginalDefinition, out var srcDecl))
                {
                    targetDecl = srcDecl;
                    targetClassName = sym.ContainingType.Name;
                }
            }

            // Step B: Syntactic resolution fallback (vital for DI interfaces or domain entities like station.RecordReading)
            if (targetDecl == null && inv.Expression is MemberAccessExpressionSyntax maCall)
            {
                var recvText = maCall.Expression.ToString();
                string? recvTypeName = null;

                if (registry.ClassByName.ContainsKey(recvText))
                {
                    recvTypeName = recvText;
                }
                else
                {
                    // Check local variable
                    var localDecl = inv.Ancestors().OfType<BlockSyntax>()
                        .SelectMany(b => b.Statements.OfType<LocalDeclarationStatementSyntax>())
                        .SelectMany(ld => ld.Declaration.Variables)
                        .FirstOrDefault(v => v.Identifier.Text == recvText);

                    if (localDecl?.Initializer?.Value is ObjectCreationExpressionSyntax oc)
                        recvTypeName = oc.Type.ToString().Split('.').Last();
                    else if (localDecl?.Initializer?.Value != null)
                    {
                        var t = model.GetTypeInfo(localDecl.Initializer.Value).Type?.Name;
                        if (t != null && t != "Object") recvTypeName = t;
                    }

                    // Check parameters in current method
                    if (recvTypeName == null)
                    {
                        var curMethod = inv.Ancestors().OfType<MethodDeclarationSyntax>().FirstOrDefault();
                        var param = curMethod?.ParameterList.Parameters.FirstOrDefault(p => p.Identifier.Text == recvText);
                        if (param?.Type != null)
                            recvTypeName = param.Type.ToString().Split('<')[0].Split('.').Last();
                    }

                    // Check fields or constructor params in current class
                    if (recvTypeName == null)
                    {
                        var curClass = inv.Ancestors().OfType<ClassDeclarationSyntax>().FirstOrDefault();
                        var field = curClass?.Members.OfType<FieldDeclarationSyntax>()
                            .SelectMany(f => f.Declaration.Variables.Select(v => (Name: v.Identifier.Text, Type: f.Declaration.Type.ToString())))
                            .FirstOrDefault(f => f.Name == recvText);
                        if (field?.Type != null)
                            recvTypeName = field.Value.Type.Split('<')[0].Split('.').Last();

                        if (recvTypeName == null)
                        {
                            var ctor = curClass?.Members.OfType<ConstructorDeclarationSyntax>().FirstOrDefault();
                            var ctorParam = ctor?.ParameterList.Parameters.FirstOrDefault(p => p.Identifier.Text == recvText.TrimStart('_'));
                            if (ctorParam?.Type != null)
                                recvTypeName = ctorParam.Type.ToString().Split('<')[0].Split('.').Last();
                        }
                    }
                }

                if (recvTypeName != null)
                {
                    if (registry.InterfaceImplementations.TryGetValue(recvTypeName, out var impls) && impls.Count > 0)
                        recvTypeName = impls[0];
                    else if (recvTypeName.StartsWith("I") && registry.ClassByName.ContainsKey(recvTypeName[1..]))
                        recvTypeName = recvTypeName[1..];

                    if (registry.MethodsByClassAndName.TryGetValue($"{recvTypeName}.{name}", out var foundDecl))
                    {
                        targetDecl = foundDecl;
                        targetClassName = recvTypeName;
                    }
                }

                // Sibling method in current class
                if (targetDecl == null)
                {
                    var curClass = inv.Ancestors().OfType<ClassDeclarationSyntax>().FirstOrDefault();
                    if (curClass != null && registry.MethodsByClassAndName.TryGetValue($"{curClass.Identifier.Text}.{name}", out var sibDecl))
                    {
                        targetDecl = sibDecl;
                        targetClassName = curClass.Identifier.Text;
                    }
                }

                // Unique method name in source
                if (targetDecl == null && registry.MethodsByName.TryGetValue(name, out var candidates) && candidates.Count == 1)
                {
                    targetDecl = candidates[0];
                    targetClassName = targetDecl.Ancestors().OfType<ClassDeclarationSyntax>().FirstOrDefault()?.Identifier.Text;
                }
            }

            if (targetDecl != null)
            {
                var callNode = Step("call",
                    ("symbol", $"{targetClassName ?? "Method"}.{targetDecl.Identifier.Text}"),
                    ("file", targetDecl.SyntaxTree.FilePath),
                    ("line", Line(targetDecl)),
                    ("call_line", line),
                    ("resolved_via", "static"),
                    ("confidence", "resolved"));

                if (depth < MaxDepth && visited.Add(targetDecl))
                {
                    var childModel = comp.GetSemanticModel(targetDecl.SyntaxTree);
                    var subSteps = new List<object>();
                    if (targetDecl.Body != null)
                        subSteps.AddRange(WalkBlock(targetDecl.Body, childModel, depth + 1));
                    else if (targetDecl.ExpressionBody != null)
                        subSteps.AddRange(ExprSteps(targetDecl.ExpressionBody.Expression, childModel, depth + 1));

                    callNode["steps"] = subSteps;
                    visited.Remove(targetDecl);
                }
                else
                {
                    callNode["note"] = "not expanded (depth/cycle)";
                }

                return [callNode];
            }

            return steps;
        }

        static ExpressionSyntax ChainRoot(InvocationExpressionSyntax inv)
        {
            ExpressionSyntax cur = inv;
            while (true)
            {
                if (cur is InvocationExpressionSyntax i) cur = i.Expression;
                else if (cur is MemberAccessExpressionSyntax ma) cur = ma.Expression;
                else if (cur is AwaitExpressionSyntax aw) cur = aw.Expression;
                else if (cur is ParenthesizedExpressionSyntax p) cur = p.Expression;
                else return cur;
                if (cur is IdentifierNameSyntax or ThisExpressionSyntax) return cur;
                if (cur is MemberAccessExpressionSyntax m2 && m2.Expression is IdentifierNameSyntax or ThisExpressionSyntax) { }
            }
        }

        static string? DbSetOf(ExpressionSyntax root, SemanticModel model)
        {
            for (SyntaxNode? cur = root; cur != null; cur = cur.Parent)
            {
                if (cur is MemberAccessExpressionSyntax ma)
                {
                    var s = model.GetSymbolInfo(ma).Symbol as IPropertySymbol;
                    if (s?.ContainingType != null && InheritsFrom(s.ContainingType, "DbContext") && s.Type.Name.StartsWith("DbSet", StringComparison.Ordinal)
                        || s?.ContainingType != null && s.ContainingType.Name.EndsWith("DbContext") && s.Type.TypeKind == TypeKind.Error)
                        return s.Name;
                }
                if (cur is StatementSyntax) break;
            }
            return null;
        }

        static bool InheritsFrom(INamedTypeSymbol t, string name)
        {
            for (var b = t; b != null; b = b.BaseType) if (b.Name == name) return true;
            return false;
        }

        static string? ReceiverType(InvocationExpressionSyntax inv, SemanticModel model)
        {
            if (inv.Expression is MemberAccessExpressionSyntax ma)
                return model.GetTypeInfo(ma.Expression).Type?.ToDisplayString();
            return null;
        }

        static string ClassifyCondition(ExpressionSyntax cond, SemanticModel model)
        {
            var t = cond.ToString();
            var noCurrent = t.Replace("CurrentVersion", "");
            if (t.Contains("Forbid") || t.Contains("UserId") || t.Contains("TenantId") || t.Contains("StationId") && t.Contains("!=")) return "authz";
            if (t.Contains("Status")) return "state_check";
            if (System.Text.RegularExpressions.Regex.IsMatch(noCurrent, @"[Vv]ersion")) return "version_check";
            if (t.Contains("IsSuccess") || t.Contains("IsValid")) return "validation";
            if (t.Contains("Config") || t.Contains("Enabled")) return "config";
            return "state_check";
        }

        static string ReturnStatus(ExpressionSyntax? e, SemanticModel model)
        {
            var t = e?.ToString() ?? "";
            if (t.StartsWith("NotFound")) return "404 NotFound";
            if (t.StartsWith("Forbid")) return "403 Forbid";
            if (t.StartsWith("ValidationProblem")) return "400 ValidationProblem";
            if (t.StartsWith("BadRequest")) return "400 BadRequest";
            if (t.StartsWith("Conflict")) return "409 Conflict";
            if (t.StartsWith("Unauthorized")) return "401 Unauthorized";
            if (t.StartsWith("Ok(") || t.StartsWith("Ok ")) return "200 OK";
            if (t.StartsWith("Created") || t.StartsWith("CreatedAtAction")) return "201 Created";
            if (t.StartsWith("NoContent")) return "204 NoContent";
            if (t.Contains("OperationResult")) return "operation result";
            return "return (unclassified)";
        }

        static string? Short(SyntaxNode? n)
        {
            if (n == null) return null;
            var s = string.Join(" ", n.ToString().Split(['\r', '\n'], StringSplitOptions.RemoveEmptyEntries).Select(x => x.Trim()));
            return s.Length > 160 ? s[..157] + "..." : s;
        }
    }
}
