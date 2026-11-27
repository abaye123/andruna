using System.IO.Pipes;
using System.Text;
using System.Text.Json;
using System.Text.Json.Nodes;
using Andruna.Manager.Core.Localization;
using Andruna.Manager.Core.Sysd;

namespace Andruna.Manager.Tests;

/// <summary>
/// Test-only stand-in for andruna-sysd: a real named pipe server speaking the
/// wire protocol, so the production client code runs unchanged against it.
/// </summary>
internal sealed class FakeSysd : IAsyncDisposable
{
    private readonly CancellationTokenSource _stop = new();
    private readonly Task _loop;

    public FakeSysd(Func<string, JsonNode?, byte[]?> handler)
    {
        PipeName = "andruna-test-" + Guid.NewGuid().ToString("N");
        Handler = handler;
        _loop = Task.Run(RunAsync);
    }

    public string PipeName { get; }

    /// <summary>Maps (method, id) to the raw response frame payload; null drops the connection.</summary>
    public Func<string, JsonNode?, byte[]?> Handler { get; set; }

    public static byte[] Result(JsonNode? id, object result) =>
        Encoding.UTF8.GetBytes(new JsonObject
        {
            ["id"] = id?.DeepClone(),
            ["result"] = JsonSerializer.SerializeToNode(result, SysdWire.JsonOptions),
        }.ToJsonString());

    public static byte[] Error(JsonNode? id, string code, string message) =>
        Encoding.UTF8.GetBytes(new JsonObject
        {
            ["id"] = id?.DeepClone(),
            ["error"] = new JsonObject { ["code"] = code, ["message"] = message },
        }.ToJsonString());

    /// <summary>A sysd that answers status and list_packages.</summary>
    public static FakeSysd Running(params PackageSummary[] packages) => new((method, id) => method switch
    {
        "ping" => Result(id, new { }),
        "status" => Result(id, new SysdStatus("0.1.0", SysdWire.ProtocolVersion, 1234)),
        "list_packages" => Result(id, new PackageList(packages)),
        _ => Error(id, "unknown_method", method),
    });

    public SysdClient Client() => new(PipeName, TimeSpan.FromSeconds(2), TimeSpan.FromSeconds(2));

    public async ValueTask DisposeAsync()
    {
        _stop.Cancel();
        try
        {
            await _loop;
        }
        catch (OperationCanceledException)
        {
        }
    }

    private async Task RunAsync()
    {
        while (!_stop.IsCancellationRequested)
        {
            await using var server = new NamedPipeServerStream(
                PipeName, PipeDirection.InOut, NamedPipeServerStream.MaxAllowedServerInstances,
                PipeTransmissionMode.Byte, PipeOptions.Asynchronous | PipeOptions.CurrentUserOnly);
            try
            {
                await server.WaitForConnectionAsync(_stop.Token);
                var request = JsonNode.Parse(await SysdWire.ReadFrameAsync(server, _stop.Token))!;
                var response = Handler(request["method"]!.GetValue<string>(), request["id"]);
                if (response is not null)
                {
                    await SysdWire.WriteFrameAsync(server, response, _stop.Token);
                    await server.FlushAsync(_stop.Token);
                    server.WaitForPipeDrain();
                }
            }
            catch (OperationCanceledException)
            {
                return;
            }
            catch (IOException)
            {
                // The client went away; serve the next one.
            }
        }
    }
}

/// <summary>Test-only service host.</summary>
internal sealed class FakeHost(bool installed, Func<bool>? start = null) : IServiceHost
{
    public int StartCalls { get; private set; }

    public bool IsInstalled => installed;

    public bool TryStart(out string? error)
    {
        StartCalls++;
        var ok = start?.Invoke() ?? false;
        error = ok ? null : "could not start";
        return ok;
    }
}

/// <summary>Test-only strings: the key itself, with arguments appended.</summary>
internal sealed class KeyStrings : IStrings
{
    public string Get(string key) => key;

    public string Format(string key, params object?[] args) => key + ":" + string.Join("|", args);
}
