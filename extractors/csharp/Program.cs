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
var foundDllNames = new HashSet<string>(StringComparer.OrdinalIgnoreCase);

// 1. Explicit refs directory
if (opts.RefsDir != null && Directory.Exists(opts.RefsDir))
{
    foreach (var dll in Directory.EnumerateFiles(opts.RefsDir, "*.dll"))
    {
        if (Path.GetFileNameWithoutExtension(dll).Equals(projectAsm, StringComparison.OrdinalIgnoreCase)) continue;
        try
        {
            var n = Path.GetFileName(dll);
            if (foundDllNames.Add(n))
            {
                _ = AssemblyName.GetAssemblyName(dll);
                refs.Add(MetadataReference.CreateFromFile(dll));
            }
        }
        catch { /* native dll */ }
    }
}

// 2. Auto-discover DLLs in project bin directories (bin/Debug, bin/Release, etc.) - G10
if (Directory.Exists(opts.ProjectDir))
{
    try
    {
        foreach (var binDir in Directory.EnumerateDirectories(opts.ProjectDir, "bin", SearchOption.AllDirectories))
        {
            foreach (var dll in Directory.EnumerateFiles(binDir, "*.dll", SearchOption.AllDirectories))
            {
                var n = Path.GetFileName(dll);
                if (Path.GetFileNameWithoutExtension(dll).Equals(projectAsm, StringComparison.OrdinalIgnoreCase)) continue;
                if (foundDllNames.Add(n))
                {
                    try { refs.Add(MetadataReference.CreateFromFile(dll)); } catch { }
                }
            }
        }
    }
    catch { }

    // 3. Auto-discover NuGet packages via project.assets.json - G10
    try
    {
        var userNuget = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.UserProfile), ".nuget", "packages");
        foreach (var assetsJson in Directory.EnumerateFiles(opts.ProjectDir, "project.assets.json", SearchOption.AllDirectories))
        {
            try
            {
                using var doc = JsonDocument.Parse(File.ReadAllText(assetsJson));
                var packageFolders = new List<string>();
                if (doc.RootElement.TryGetProperty("packageFolders", out var pf))
                {
                    foreach (var prop in pf.EnumerateObject()) packageFolders.Add(prop.Name);
                }
                if (packageFolders.Count == 0 && Directory.Exists(userNuget)) packageFolders.Add(userNuget);

                if (doc.RootElement.TryGetProperty("targets", out var targets))
                {
                    foreach (var target in targets.EnumerateObject())
                    {
                        foreach (var pkg in target.Value.EnumerateObject())
                        {
                            if (pkg.Value.TryGetProperty("compile", out var compile))
                            {
                                foreach (var dllProp in compile.EnumerateObject())
                                {
                                    var relPath = Path.Combine(pkg.Name, dllProp.Name).Replace('/', Path.DirectorySeparatorChar);
                                    foreach (var baseDir in packageFolders)
                                    {
                                        var fullPath = Path.Combine(baseDir, relPath);
                                        if (File.Exists(fullPath) && foundDllNames.Add(Path.GetFileName(fullPath)))
                                        {
                                            try { refs.Add(MetadataReference.CreateFromFile(fullPath)); } catch { }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            catch { }
        }
    }
    catch { }
}

var tpa = ((string?)AppContext.GetData("TRUSTED_PLATFORM_ASSEMBLIES"))?.Split(Path.PathSeparator) ?? [];
var dotnetRoot = Path.GetFullPath(Path.Combine(Path.GetDirectoryName(typeof(object).Assembly.Location)!, "..", ".."));
var aspDir = Path.Combine(dotnetRoot, "Microsoft.AspNetCore.App");
if (Directory.Exists(aspDir))
{
    var best = Directory.GetDirectories(aspDir).OrderByDescending(d => d).FirstOrDefault(d => Path.GetFileName(d).StartsWith("9.") || Path.GetFileName(d).StartsWith("8."));
    if (best != null)
        foreach (var dll in Directory.EnumerateFiles(best, "*.dll"))
        {
            var n = Path.GetFileName(dll);
            if (foundDllNames.Add(n))
                try { _ = AssemblyName.GetAssemblyName(dll); refs.Add(MetadataReference.CreateFromFile(dll)); } catch { }
        }
}
foreach (var p in tpa)
{
    var n = Path.GetFileName(p);
    if (foundDllNames.Add(n))
        try { refs.Add(MetadataReference.CreateFromFile(p)); } catch { }
}

var compilation = CSharpCompilation.Create("flow", trees, refs,
    new CSharpCompilationOptions(OutputKind.DynamicallyLinkedLibrary, nullableContextOptions: NullableContextOptions.Enable));

// 1. Comprehensive Type & Method Registry
var methodIndex = new Dictionary<IMethodSymbol, MethodDeclarationSyntax>(SymbolEqualityComparer.Default);
var interfaceImplementations = new Dictionary<string, List<string>>(StringComparer.OrdinalIgnoreCase);
var classByName = new Dictionary<string, ClassDeclarationSyntax>(StringComparer.OrdinalIgnoreCase);
var methodsByClassAndName = new Dictionary<string, MethodDeclarationSyntax>(StringComparer.OrdinalIgnoreCase);
var methodsByName = new Dictionary<string, List<MethodDeclarationSyntax>>(StringComparer.OrdinalIgnoreCase);
var requestHandlersByCommand = new Dictionary<string, HandlerRegistration>(StringComparer.OrdinalIgnoreCase);

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

                // Syntactic detection of IRequestHandler<TCommand, ...> or RequestHandler<TCommand, ...>
                if (raw.Contains("IRequestHandler<") || raw.Contains("RequestHandler<"))
                {
                    var cmdType = raw.Substring(raw.IndexOf('<') + 1).Split(',')[0].Split('>')[0].Trim().Split('.').Last();
                    var handleM = cls.Members.OfType<MethodDeclarationSyntax>()
                        .FirstOrDefault(m => m.Identifier.Text is "Handle" or "HandleAsync");
                    if (handleM != null && !string.IsNullOrWhiteSpace(cmdType))
                    {
                        requestHandlersByCommand[cmdType] = new HandlerRegistration(cls, handleM, cmdType);
                    }
                }
            }
        }

        // Semantic model detection of IRequestHandler
        if (sm.GetDeclaredSymbol(cls) is INamedTypeSymbol nts)
        {
            foreach (var iface in nts.AllInterfaces)
            {
                if (iface.Name is "IRequestHandler" && iface.TypeArguments.Length > 0)
                {
                    var cmdType = iface.TypeArguments[0].Name;
                    var handleM = cls.Members.OfType<MethodDeclarationSyntax>()
                        .FirstOrDefault(m => m.Identifier.Text is "Handle" or "HandleAsync");
                    if (handleM != null && !string.IsNullOrWhiteSpace(cmdType))
                    {
                        requestHandlersByCommand[cmdType] = new HandlerRegistration(cls, handleM, cmdType);
                    }
                }
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

var registry = new TypeRegistry(methodIndex, interfaceImplementations, classByName, methodsByClassAndName, methodsByName, requestHandlersByCommand);

string[] httpAttrs = ["HttpGet", "HttpPost", "HttpPut", "HttpPatch", "HttpDelete"];
int count = 0;

// 2. Scan HTTP Controllers (G1: Route Token Expansion)
foreach (var tree in trees)
{
    var model = compilation.GetSemanticModel(tree);
    foreach (var cls in tree.GetRoot().DescendantNodes().OfType<ClassDeclarationSyntax>())
    {
        if (opts.Only != null && cls.Identifier.Text != opts.Only) continue;
        var classRoute = cls.AttributeLists.SelectMany(a => a.Attributes).FirstOrDefault(a => RouteHelper.AttrName(a) == "Route");
        var prefix = classRoute?.ArgumentList?.Arguments.FirstOrDefault()?.ToString().Trim('"') ?? "";
        foreach (var m in cls.Members.OfType<MethodDeclarationSyntax>())
        {
            var http = m.AttributeLists.SelectMany(a => a.Attributes).FirstOrDefault(a => httpAttrs.Contains(RouteHelper.AttrName(a)));
            if (http == null) continue;
            var verb = RouteHelper.AttrName(http)!.Replace("Http", "").ToUpperInvariant();
            var tmpl = http.ArgumentList?.Arguments.FirstOrDefault()?.ToString().Trim('"') ?? "";
            var rawCombined = string.Join('/', new[] { prefix, tmpl }.Where(s => s.Length > 0));
            var path = RouteHelper.ResolveRoute(rawCombined, cls.Identifier.Text, m.Identifier.Text);
            var flow = FlowBuilder.Build(compilation, model, cls, m, verb, path, tree.FilePath, registry);
            var file = Path.Combine(opts.OutDir, $"{cls.Identifier.Text}.{m.Identifier.Text}.json");
            File.WriteAllText(file, JsonSerializer.Serialize(flow, Json.Options));
            count++;
        }
    }
}

// 2b. Scan Minimal API Endpoints (G7: Minimal-API endpoint classes & top-level statements)
string[] mapMethods = ["MapGet", "MapPost", "MapPut", "MapPatch", "MapDelete"];
foreach (var tree in trees)
{
    var model = compilation.GetSemanticModel(tree);
    var root = tree.GetRoot();

    var invocations = root.DescendantNodes().OfType<InvocationExpressionSyntax>()
        .Where(inv => inv.Expression is MemberAccessExpressionSyntax ma && mapMethods.Contains(ma.Name.Identifier.Text));

    foreach (var inv in invocations)
    {
        var ma = (MemberAccessExpressionSyntax)inv.Expression;
        var mapMethodName = ma.Name.Identifier.Text;
        var verb = mapMethodName.Replace("Map", "").ToUpperInvariant();

        var invArgs = inv.ArgumentList.Arguments;
        if (invArgs.Count == 0) continue;

        var routeArg = invArgs[0].Expression.ToString().Trim('"');
        var handlerArg = invArgs.Count > 1 ? invArgs[1].Expression : null;

        var enclosingClass = inv.Ancestors().OfType<ClassDeclarationSyntax>().FirstOrDefault();
        var className = enclosingClass?.Identifier.Text ?? "Program";

        MethodDeclarationSyntax? targetMethod = null;
        if (handlerArg is IdentifierNameSyntax idSyntax)
        {
            targetMethod = enclosingClass?.Members.OfType<MethodDeclarationSyntax>().FirstOrDefault(m => m.Identifier.Text == idSyntax.Identifier.Text);
        }
        else if (handlerArg is MemberAccessExpressionSyntax maHandler)
        {
            var targetName = maHandler.Name.Identifier.Text;
            targetMethod = registry.MethodsByName.TryGetValue(targetName, out var mList) ? mList.FirstOrDefault() : null;
        }

        var flow = FlowBuilder.BuildMinimalApi(compilation, model, enclosingClass, targetMethod, inv, handlerArg, verb, routeArg, tree.FilePath, registry);
        var safeRoute = routeArg.Replace('/', '_').Replace('{', '_').Replace('}', '_').Replace(':', '_').Trim('_');
        if (string.IsNullOrEmpty(safeRoute)) safeRoute = "root";
        var file = Path.Combine(opts.OutDir, $"{className}_{verb}_{safeRoute}.json");
        File.WriteAllText(file, JsonSerializer.Serialize(flow, Json.Options));
        count++;
    }
}

// 3. Scan Event Handlers & Message Consumers (G8: Consumers derived from generic base classes)
foreach (var tree in trees)
{
    var model = compilation.GetSemanticModel(tree);
    foreach (var cls in tree.GetRoot().DescendantNodes().OfType<ClassDeclarationSyntax>())
    {
        var discoveredEvents = new HashSet<string>(StringComparer.OrdinalIgnoreCase);

        // Check semantic model interfaces (AllInterfaces checks entire hierarchy)
        if (model.GetDeclaredSymbol(cls) is INamedTypeSymbol nts)
        {
            foreach (var iface in nts.AllInterfaces)
            {
                if (iface.Name is "IConsumer" or "IEventHandler" or "INotificationHandler" or "IIntegrationEventHandler")
                {
                    if (iface.TypeArguments.Length > 0)
                    {
                        var evName = iface.TypeArguments[0].Name;
                        if (!string.IsNullOrWhiteSpace(evName))
                            discoveredEvents.Add(evName);
                    }
                }
            }
        }

        // Syntactic fallback for base classes (e.g. IntegrationEventHandler<T>)
        if (cls.BaseList != null)
        {
            foreach (var bt in cls.BaseList.Types)
            {
                var raw = bt.Type.ToString();
                if (raw.Contains('<') && raw.Contains('>'))
                {
                    var baseName = raw.Split('<')[0].Split('.').Last();
                    var argName = raw.Substring(raw.IndexOf('<') + 1).Split('>')[0].Split(',')[0].Trim().Split('.').Last();
                    if (baseName.Contains("EventHandler") || baseName.Contains("Consumer") || baseName.Contains("NotificationHandler"))
                    {
                        if (!string.IsNullOrWhiteSpace(argName))
                            discoveredEvents.Add(argName);
                    }
                    else if (classByName.TryGetValue(baseName, out var baseCls) && baseCls.BaseList != null)
                    {
                        if (baseCls.BaseList.Types.Any(t => t.ToString().Contains("IConsumer") || t.ToString().Contains("IEventHandler") || t.ToString().Contains("INotificationHandler")))
                        {
                            if (!string.IsNullOrWhiteSpace(argName))
                                discoveredEvents.Add(argName);
                        }
                    }
                }
            }
        }

        foreach (var eventTypeName in discoveredEvents)
        {
            if (string.IsNullOrWhiteSpace(eventTypeName)) continue;
            var handleMethod = cls.Members.OfType<MethodDeclarationSyntax>()
                .FirstOrDefault(m => m.Identifier.Text is "HandleAsync" or "Handle" or "Consume");

            if (handleMethod == null && cls.BaseList?.Types.Count > 0)
            {
                var baseName = cls.BaseList.Types[0].Type.ToString().Split('<')[0].Split('.').Last();
                if (classByName.TryGetValue(baseName, out var baseCls))
                {
                    handleMethod = baseCls.Members.OfType<MethodDeclarationSyntax>()
                        .FirstOrDefault(m => m.Identifier.Text is "HandleAsync" or "Handle" or "Consume");
                }
            }

            if (handleMethod == null) continue;

            var flow = FlowBuilder.BuildEventHandler(compilation, model, cls, handleMethod, eventTypeName, tree.FilePath, registry);
            var file = Path.Combine(opts.OutDir, $"{cls.Identifier.Text}.{handleMethod.Identifier.Text}_{eventTypeName}.json");
            File.WriteAllText(file, JsonSerializer.Serialize(flow, Json.Options));
            count++;
        }
    }
}

Console.WriteLine($"Wrote {count} flows to {opts.OutDir}");
var errs = compilation.GetDiagnostics().Count(d => d.Severity == DiagnosticSeverity.Error);
Console.WriteLine($"(compilation diagnostics: {errs} errors; reference assemblies resolved: {refs.Count})");

public static class RouteHelper
{
    public static string? AttrName(AttributeSyntax a) => a.Name.ToString().Split('.').Last().Replace("Attribute", "");

    public static string ResolveRoute(string template, string controllerClass, string actionMethod)
    {
        var ctrlName = controllerClass.EndsWith("Controller", StringComparison.OrdinalIgnoreCase) && controllerClass.Length > 10
            ? controllerClass.Substring(0, controllerClass.Length - "Controller".Length)
            : controllerClass;
        var actName = actionMethod.EndsWith("Async", StringComparison.OrdinalIgnoreCase) && actionMethod.Length > 5
            ? actionMethod.Substring(0, actionMethod.Length - "Async".Length)
            : actionMethod;

        var resolved = template
            .Replace("[controller]", ctrlName, StringComparison.OrdinalIgnoreCase)
            .Replace("{controller}", ctrlName, StringComparison.OrdinalIgnoreCase)
            .Replace("[action]", actName, StringComparison.OrdinalIgnoreCase)
            .Replace("{action}", actName, StringComparison.OrdinalIgnoreCase);

        var parts = resolved.Split('/', StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries);
        return string.Join('/', parts);
    }
}

record HandlerRegistration(ClassDeclarationSyntax Class, MethodDeclarationSyntax Method, string CommandType);

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
    Dictionary<string, List<MethodDeclarationSyntax>> MethodsByName,
    Dictionary<string, HandlerRegistration> RequestHandlersByCommand
);

static class FlowBuilder
{
    const int MaxDepth = 6;

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
        var ctx = new Ctx(comp, registry, visited, cls.Identifier.Text);
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
                var tbl = r.GetString();
                if (!string.IsNullOrWhiteSpace(tbl) && tbl != "DbContext")
                    tablesTouched.Add(new { table = tbl, operation = "SELECT", transactional = false });
            }
        }
        if (couplingDict.TryGetValue("writes", out var writesEl) && writesEl.ValueKind == JsonValueKind.Array)
        {
            foreach (var w in writesEl.EnumerateArray())
            {
                var tbl = w.GetString();
                if (!string.IsNullOrWhiteSpace(tbl) && tbl != "DbContext")
                {
                    var op = verb == "POST" ? "INSERT" : verb == "DELETE" ? "DELETE" : "UPDATE";
                    tablesTouched.Add(new { table = tbl, operation = op, transactional = true });
                }
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
            ["configs_and_flags"] = ctx.TrackedFlags.ToList(),
        };
    }

    public static Dictionary<string, object?> BuildMinimalApi(
        Compilation comp, SemanticModel model, ClassDeclarationSyntax? cls, MethodDeclarationSyntax? m,
        InvocationExpressionSyntax inv, ExpressionSyntax? handlerArg,
        string verb, string rawPath, string file, TypeRegistry registry)
    {
        var className = cls?.Identifier.Text ?? "Program";
        var methodName = m?.Identifier.Text ?? $"{verb}_{rawPath.Replace('/', '_').Replace('{', '_').Replace('}', '_')}";
        var path = RouteHelper.ResolveRoute(rawPath, className, methodName);

        var guards = new List<object>();
        var visited = new HashSet<MethodDeclarationSyntax>();
        var ctx = new Ctx(comp, registry, visited, className);
        var steps = new List<object>();

        if (handlerArg is LambdaExpressionSyntax lambda)
        {
            if (lambda.Body is BlockSyntax b)
                steps.AddRange(ctx.WalkBlock(b, model, 0));
            else if (lambda.Body is ExpressionSyntax expr)
                steps.AddRange(ctx.ExprSteps(expr, model, 0));
        }
        else if (m != null)
        {
            if (m.Body != null)
                steps.AddRange(ctx.WalkBlock(m.Body, model, 0));
            else if (m.ExpressionBody != null)
                steps.AddRange(ctx.ExprSteps(m.ExpressionBody.Expression, model, 0));
        }

        var domainId = className.ToLower().Replace("endpoint", "").Replace("module", "").Replace("controller", "");
        if (string.IsNullOrEmpty(domainId)) domainId = "api";
        var domainName = $"{className} Operations";

        var couplingObj = Coupling(steps);
        var tablesTouched = new List<object>();
        var couplingDict = JsonSerializer.Deserialize<Dictionary<string, JsonElement>>(JsonSerializer.Serialize(couplingObj)) ?? new();
        if (couplingDict.TryGetValue("reads", out var readsEl) && readsEl.ValueKind == JsonValueKind.Array)
        {
            foreach (var r in readsEl.EnumerateArray())
            {
                var tbl = r.GetString();
                if (!string.IsNullOrWhiteSpace(tbl) && tbl != "DbContext")
                    tablesTouched.Add(new { table = tbl, operation = "SELECT", transactional = false });
            }
        }
        if (couplingDict.TryGetValue("writes", out var writesEl) && writesEl.ValueKind == JsonValueKind.Array)
        {
            foreach (var w in writesEl.EnumerateArray())
            {
                var tbl = w.GetString();
                if (!string.IsNullOrWhiteSpace(tbl) && tbl != "DbContext")
                    tablesTouched.Add(new { table = tbl, operation = verb == "POST" ? "INSERT" : "UPDATE", transactional = true });
            }
        }

        return new()
        {
            ["id"] = $"{verb} {path}",
            ["kind"] = "http",
            ["domain"] = domainName,
            ["domain_id"] = domainId,
            ["scenario"] = $"{verb} {path}",
            ["trigger"] = new { method = verb, path, declared_in = $"{className}.{methodName}" },
            ["handler"] = new
            {
                symbol = $"{className}.{methodName}",
                file,
                line = Line(inv),
                signature = $"{methodName}()",
            },
            ["contract"] = new
            {
                response = new { status_codes = new[] { new { code = verb == "POST" ? 201 : 200, description = "Success" } } },
            },
            ["guards"] = guards,
            ["steps"] = steps,
            ["coupling"] = couplingObj,
            ["tables_touched"] = tablesTouched,
            ["configs_and_flags"] = ctx.TrackedFlags.ToList(),
        };
    }

    public static Dictionary<string, object?> BuildEventHandler(
        Compilation comp, SemanticModel model, ClassDeclarationSyntax cls, MethodDeclarationSyntax m,
        string eventTypeName, string file, TypeRegistry registry)
    {
        var visited = new HashSet<MethodDeclarationSyntax>();
        var ctx = new Ctx(comp, registry, visited, cls.Identifier.Text);
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
            {
                var tbl = r.GetString();
                if (!string.IsNullOrWhiteSpace(tbl) && tbl != "DbContext")
                    tablesTouched.Add(new { table = tbl, operation = "SELECT", transactional = false });
            }
        }
        if (couplingDict.TryGetValue("writes", out var writesEl) && writesEl.ValueKind == JsonValueKind.Array)
        {
            foreach (var w in writesEl.EnumerateArray())
            {
                var tbl = w.GetString();
                if (!string.IsNullOrWhiteSpace(tbl) && tbl != "DbContext")
                    tablesTouched.Add(new { table = tbl, operation = "UPDATE", transactional = true });
            }
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
            ["configs_and_flags"] = ctx.TrackedFlags.ToList(),
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
            .Select(d => d.GetValueOrDefault("target") as string ?? "")
            .Where(s => !string.IsNullOrWhiteSpace(s) && s != "DbContext" && s != "\"\"")
            .Distinct().OrderBy(x => x).ToList();

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

    sealed class Ctx(Compilation comp, TypeRegistry registry, HashSet<MethodDeclarationSyntax> visited, string contextName = "")
    {
        public HashSet<string> TrackedTables { get; } = new(StringComparer.OrdinalIgnoreCase);
        public HashSet<string> TrackedFlags { get; } = new(StringComparer.OrdinalIgnoreCase);

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
                    var (condKind, flagName) = ClassifyCondition(ifs.Condition, model);
                    steps.AddRange(ExprSteps(ifs.Condition, model, depth));
                    var branches = new List<object>
                    {
                        new Dictionary<string, object?> { ["when"] = "true", ["steps"] = WalkStatement(ifs.Statement, model, depth) },
                    };
                    if (ifs.Else != null)
                        branches.Add(new Dictionary<string, object?> { ["when"] = "false", ["steps"] = WalkStatement(ifs.Else.Statement, model, depth) });

                    var step = Step("decision",
                        ("kind", condKind),
                        ("condition", Short(ifs.Condition)),
                        ("line", Line(ifs)),
                        ("branches", branches),
                        ("confidence", "extracted"));

                    if (flagName != null)
                    {
                        step["flag"] = flagName;
                        TrackedFlags.Add(flagName);
                    }

                    steps.Add(step);
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
                            var initStr = v.Initializer.Value.ToString();
                            if (initStr.Contains("Config") || initStr.Contains("Enabled") || initStr.Contains("Feature"))
                            {
                                var (_, fName) = ClassifyCondition(v.Initializer.Value, model);
                                if (fName != null) TrackedFlags.Add(fName);
                            }
                        }
                    return steps;

                case ExpressionStatementSyntax es:
                    if (es.Expression is AssignmentExpressionSyntax asg && asg.Left is MemberAccessExpressionSyntax maAsg)
                    {
                        var targetName = Short(asg.Left) ?? "";
                        steps.Add(Step("effect", ("kind", "state_mutation"), ("target", targetName), ("op", $"= {Short(asg.Right)}"), ("line", Line(es)), ("confidence", "extracted")));

                        // Track entity table write (G4)
                        var entityVar = maAsg.Expression.ToString();
                        var entityType = model.GetTypeInfo(maAsg.Expression).Type?.Name;
                        if (entityType != null && entityType != "Object")
                        {
                            TrackedTables.Add(Pluralize(entityType));
                        }
                        else
                        {
                            TrackedTables.Add(Pluralize(entityVar));
                        }
                    }
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

            // 1. EF Core: SaveChanges / SaveChangesAsync (G4: Handler Table Writes)
            if (name.StartsWith("SaveChanges"))
            {
                var tablesToRecord = TrackedTables.Where(t => t != "DbContext" && !string.IsNullOrWhiteSpace(t)).ToList();
                if (tablesToRecord.Count == 0)
                {
                    // Infer table from enclosing class/domain context
                    var inferred = InferTableFromContext(inv, model, contextName);
                    tablesToRecord.Add(inferred);
                }

                var writeSteps = new List<object>();
                foreach (var tbl in tablesToRecord)
                {
                    writeSteps.Add(Step("effect", ("kind", "db_write"), ("target", tbl), ("op", name), ("transactional", true), ("line", line), ("confidence", "resolved")));
                }
                return writeSteps;
            }

            // 2. Direct DbSet mutation or query: _dbContext.Orders.Add / FirstOrDefaultAsync / etc.
            var dbSet = DbSetOf(chainRoot, model);
            if (dbSet != null)
            {
                TrackedTables.Add(dbSet);
                var write = name is "Add" or "AddAsync" or "AddRange" or "Remove" or "RemoveRange" or "Update" or "ExecuteUpdateAsync" or "ExecuteDeleteAsync";
                return [Step("effect", ("kind", write ? "db_write" : "db_read"), ("target", dbSet), ("op", name), ("line", line), ("confidence", "resolved"))];
            }

            // 3. dbContext.Add(entity) or dbContext.Update(entity)
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
                    var tablePlural = Pluralize(targetTable);
                    TrackedTables.Add(tablePlural);
                    return [Step("effect", ("kind", "db_write"), ("target", tablePlural), ("op", name), ("line", line), ("confidence", "resolved"))];
                }
            }

            // 4. Domain Event: AddDomainEvent(new SomeEvent(...)) - G3
            if (name == "AddDomainEvent")
            {
                var ev = inv.ArgumentList.Arguments.FirstOrDefault()?.Expression;
                var evType = ev is ObjectCreationExpressionSyntax oc ? oc.Type.ToString().Split('.').Last() : Short(ev)?.Split('.').Last();
                if (evType != null && evType.Contains("(")) evType = evType.Split('(')[0].Trim();
                if (!string.IsNullOrWhiteSpace(evType) && evType != "DomainEvent")
                {
                    return [Step("effect", ("kind", "event_publish"), ("target", evType), ("op", "AddDomainEvent"), ("deferred_until", "SaveChanges"), ("line", line), ("confidence", "extracted"))];
                }
                return [];
            }

            // 5. Mediator / Sender: mediator.Send(...) - G2 & G17
            var recvExpr = inv.Expression is MemberAccessExpressionSyntax maRecv ? maRecv.Expression.ToString() : "";
            var isMediatorSend = (name is "Send" or "SendAsync") &&
                (recvExpr.Contains("mediator", StringComparison.OrdinalIgnoreCase) ||
                 recvExpr.Contains("sender", StringComparison.OrdinalIgnoreCase) ||
                 recvExpr.StartsWith("_sender") || recvExpr.StartsWith("_mediator"));

            if (isMediatorSend)
            {
                string? commandTypeName = null;
                var arg = inv.ArgumentList.Arguments.FirstOrDefault()?.Expression;
                if (arg is ObjectCreationExpressionSyntax oc)
                    commandTypeName = oc.Type.ToString().Split('.').Last().Split('<')[0];
                else if (arg != null)
                {
                    var t = model.GetTypeInfo(arg).Type?.Name;
                    if (t != null && t != "Object") commandTypeName = t;
                    else
                    {
                        var argText = arg.ToString();
                        var localDecl = inv.Ancestors().OfType<BlockSyntax>().SelectMany(b => b.Statements.OfType<LocalDeclarationStatementSyntax>())
                            .SelectMany(ld => ld.Declaration.Variables)
                            .FirstOrDefault(v => v.Identifier.Text == argText);
                        if (localDecl?.Initializer?.Value is ObjectCreationExpressionSyntax oc2)
                            commandTypeName = oc2.Type.ToString().Split('.').Last().Split('<')[0];
                    }
                }

                if (inv.Expression is MemberAccessExpressionSyntax maGen && maGen.Name is GenericNameSyntax gn)
                {
                    commandTypeName ??= gn.TypeArgumentList.Arguments.FirstOrDefault()?.ToString().Split('.').Last();
                }

                if (commandTypeName != null && registry.RequestHandlersByCommand.TryGetValue(commandTypeName, out var handlerReg))
                {
                    var callNode = Step("call",
                        ("symbol", $"{handlerReg.Class.Identifier.Text}.{handlerReg.Method.Identifier.Text}"),
                        ("target", commandTypeName),
                        ("kind", "mediator_send"),
                        ("file", handlerReg.Method.SyntaxTree.FilePath),
                        ("line", Line(handlerReg.Method)),
                        ("call_line", line),
                        ("resolved_via", "mediator"),
                        ("confidence", "resolved"));

                    if (depth < MaxDepth && visited.Add(handlerReg.Method))
                    {
                        var childModel = comp.GetSemanticModel(handlerReg.Method.SyntaxTree);
                        var subCtx = new Ctx(comp, registry, visited, handlerReg.Class.Identifier.Text);
                        var subSteps = handlerReg.Method.Body != null
                            ? subCtx.WalkBlock(handlerReg.Method.Body, childModel, depth + 1)
                            : handlerReg.Method.ExpressionBody != null
                            ? subCtx.ExprSteps(handlerReg.Method.ExpressionBody.Expression, childModel, depth + 1)
                            : new List<object>();

                        callNode["steps"] = subSteps;
                        visited.Remove(handlerReg.Method);

                        // Bubble up tables and flags discovered in the mediator handler!
                        foreach (var t in subCtx.TrackedTables) TrackedTables.Add(t);
                        foreach (var f in subCtx.TrackedFlags) TrackedFlags.Add(f);
                    }
                    else
                    {
                        callNode["note"] = "not expanded (depth/cycle)";
                    }
                    return [callNode];
                }
                else if (commandTypeName != null)
                {
                    return [Step("call", ("symbol", $"IRequestHandler<{commandTypeName}>.Handle"), ("target", commandTypeName), ("kind", "mediator_send"), ("line", line), ("confidence", "unresolved"))];
                }
            }

            // 6. Pub/Sub Messaging: bus.Publish / PublishAsync / mediator.Publish (G5: Filter empty names)
            if (name is "Publish" or "PublishAsync")
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
                if (!string.IsNullOrWhiteSpace(evType) && evType != "Object" && evType != "\"\"")
                {
                    return [Step("effect", ("kind", "event_publish"), ("target", evType), ("op", name), ("line", line), ("confidence", "resolved"))];
                }
                return [];
            }

            var recvType = ReceiverType(inv, model);
            var recvName = recvType == null ? null : recvType.Split('<')[0];
            if (recvName != null && recvName.Contains("ILogger"))
                return [Step("effect", ("kind", "log"), ("target", "ILogger"), ("op", name), ("line", line), ("confidence", "resolved"))];
            if (recvName != null && (recvName.EndsWith("ApiClient") || recvName.EndsWith("HttpClient")))
                return [Step("effect", ("kind", "http_call"), ("target", $"{recvName}.{name}"), ("line", line), ("confidence", "resolved"))];

            // 7. Resolve In-Source Methods, Interface Implementations, and Domain Model Methods
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

            // Step B: Syntactic resolution fallback (vital for DI interfaces or domain entities like order.Submit)
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
                    TrackedTables.Add(Pluralize(recvTypeName));

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
                    var subCtx = new Ctx(comp, registry, visited, targetClassName ?? contextName);
                    var subSteps = targetDecl.Body != null
                        ? subCtx.WalkBlock(targetDecl.Body, childModel, depth + 1)
                        : targetDecl.ExpressionBody != null
                        ? subCtx.ExprSteps(targetDecl.ExpressionBody.Expression, childModel, depth + 1)
                        : new List<object>();

                    callNode["steps"] = subSteps;
                    visited.Remove(targetDecl);

                    foreach (var t in subCtx.TrackedTables) TrackedTables.Add(t);
                    foreach (var f in subCtx.TrackedFlags) TrackedFlags.Add(f);
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

        (string Kind, string? FlagName) ClassifyCondition(ExpressionSyntax cond, SemanticModel model)
        {
            var t = cond.ToString();
            var noCurrent = t.Replace("CurrentVersion", "");
            if (t.Contains("Forbid") || t.Contains("UserId") || t.Contains("TenantId") || (t.Contains("StationId") && t.Contains("!=")))
                return ("authz", null);
            if (t.Contains("Status")) return ("state_check", null);
            if (System.Text.RegularExpressions.Regex.IsMatch(noCurrent, @"[Vv]ersion")) return ("version_check", null);
            if (t.Contains("IsSuccess") || t.Contains("IsValid")) return ("validation", null);

            // G6: Configuration-driven branching & flags
            if (t.Contains("Config") || t.Contains("Enabled") || t.Contains("Feature") || t.Contains("Setting") || t.Contains("Flag") || t.Contains("Option") || t.Contains("Allow"))
            {
                var match = System.Text.RegularExpressions.Regex.Match(t, @"(?:\b|[._])([A-Za-z0-9_]*(?:Enabled|Feature|Config|Setting|Flag|Option|Allow)[A-Za-z0-9_]*)");
                var flagName = match.Success ? match.Groups[1].Value : t.Replace("!", "").Trim();
                TrackedFlags.Add(flagName);
                return ("feature_flag", flagName);
            }
            return ("state_check", null);
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

        static string Pluralize(string name)
        {
            var clean = name.TrimStart('_');
            if (clean.EndsWith("s", StringComparison.OrdinalIgnoreCase)) return clean;
            if (clean.EndsWith("y", StringComparison.OrdinalIgnoreCase) && clean.Length > 1 && !"aeiou".Contains(char.ToLowerInvariant(clean[^2])))
                return clean[..^1] + "ies";
            return clean + "s";
        }

        static string InferTableFromContext(InvocationExpressionSyntax inv, SemanticModel model, string fallback)
        {
            var m = inv.Ancestors().OfType<MethodDeclarationSyntax>().FirstOrDefault();
            if (m != null)
            {
                foreach (var p in m.ParameterList.Parameters)
                {
                    var pType = p.Type?.ToString()?.Split('.').Last();
                    if (pType != null && !pType.StartsWith("int") && !pType.StartsWith("string") && !pType.StartsWith("Guid") && !pType.StartsWith("CancellationToken"))
                        return Pluralize(pType);
                }
            }
            var clean = fallback.Replace("Controller", "").Replace("CommandHandler", "").Replace("Handler", "");
            return !string.IsNullOrWhiteSpace(clean) ? Pluralize(clean) : "Records";
        }
    }
}
