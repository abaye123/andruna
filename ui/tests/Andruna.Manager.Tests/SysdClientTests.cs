using System.Buffers.Binary;
using System.IO.Pipes;
using Andruna.Manager.Core.Sysd;

namespace Andruna.Manager.Tests;

public class SysdClientTests
{
    private static SysdClient Absent(int ms = 200) =>
        new("andruna-test-absent-" + Guid.NewGuid().ToString("N"), TimeSpan.FromMilliseconds(ms));

    [Fact]
    public void PipeName_FollowsTheIpcEndpoint()
    {
        Assert.Equal(
            "Andruna.sysd.S-1-5-21-1-2-3-1001.2.default",
            SysdEndpoint.PipeName("S-1-5-21-1-2-3-1001", 2));

        var name = SysdEndpoint.PipeNameForCurrentUser();
        var sid = SysdEndpoint.CurrentUserSid().Value;
        Assert.Equal($"Andruna.sysd.{sid}.{System.Diagnostics.Process.GetCurrentProcess().SessionId}.default", name);
    }

    [Fact]
    public async Task NoServer_IsNotRunning()
    {
        using var client = Absent();

        var status = await client.GetStatusAsync();

        Assert.Equal(SysdOutcome.NotRunning, status.Outcome);
        Assert.Null(status.Value);
    }

    [Fact]
    public async Task Status_IsDecoded()
    {
        await using var sysd = FakeSysd.Running();
        using var client = sysd.Client();

        var status = await client.GetStatusAsync();

        Assert.True(status.IsOk, status.Detail);
        Assert.Equal(new SysdStatus(4321, 61_000, 1, 0, 30_000, "0.1.0"), status.Value);
    }

    [Fact]
    public async Task Ping_ReturnsTheServerProtocolVersion()
    {
        await using var sysd = FakeSysd.Running();
        using var client = sysd.Client();

        var ping = await client.PingAsync();

        Assert.True(ping.IsOk, ping.Detail);
        Assert.Equal(SysdWire.ProtocolVersion, ping.Value);
    }

    [Fact]
    public async Task ListPackages_IsDecoded()
    {
        await using var sysd = FakeSysd.Running("org.example.one", "org.example.two");
        using var client = sysd.Client();

        var list = await client.ListPackagesAsync();

        Assert.True(list.IsOk, list.Detail);
        Assert.Equal(["org.example.one", "org.example.two"], list.Value!.Select(p => p.Name));
    }

    [Fact]
    public async Task StartPackage_NotInstalled_IsAServiceErrorWithItsCode()
    {
        string? asked = null;
        await using var sysd = new FakeSysd((id, req) =>
        {
            asked = req.Package;
            return FakeSysd.Answer(id, req, []);
        });
        using var client = sysd.Client();

        var started = await client.StartPackageAsync("org.example.none");

        Assert.Equal("org.example.none", asked);
        Assert.Equal(SysdOutcome.ServiceError, started.Outcome);
        Assert.Equal(SysdErrorCode.NotInstalled, started.ErrorCode);
    }

    [Fact]
    public async Task StartPackage_And_Focus_Succeed()
    {
        await using var sysd = new FakeSysd((id, req) => req.Kind == SysdWire.Kind.StartPackage
            ? TestWire.Encode(new SysdResponse.Started(77, true), id)
            : TestWire.Encode(new SysdResponse.Focused(), id));
        using var client = sysd.Client();

        var started = await client.StartPackageAsync("org.example.one");
        var focused = await client.FocusWindowAsync("org.example.one");

        Assert.Equal(new SysdResponse.Started(77, true), started.Value);
        Assert.True(focused.IsOk, focused.Detail);
    }

    [Fact]
    public async Task Connection_IsKeptOpen_AcrossCalls_WithIncreasingIds()
    {
        var ids = new List<uint>();
        await using var sysd = new FakeSysd((id, req) =>
        {
            lock (ids)
            {
                ids.Add(id);
            }

            return FakeSysd.Answer(id, req, []);
        });
        using var client = sysd.Client();

        await client.PingAsync();
        await client.GetStatusAsync();
        await client.ListPackagesAsync();

        Assert.True(client.IsConnected);
        Assert.Equal((uint)Environment.ProcessId, client.ServerPid);
        Assert.Equal(1, sysd.Connections);
        Assert.Equal([1u, 2u, 3u], ids);
    }

    [Fact]
    public async Task ConcurrentCalls_AreSerializedOnOneConnection()
    {
        await using var sysd = FakeSysd.Running();
        using var client = sysd.Client();

        var results = await Task.WhenAll(Enumerable.Range(0, 20).Select(_ => client.GetStatusAsync()));

        Assert.All(results, r => Assert.True(r.IsOk, r.Detail));
        Assert.Equal(1, sysd.Connections);
    }

    [Fact]
    public async Task ClosedConnection_IsReopened_Once()
    {
        var calls = 0;
        await using var sysd = new FakeSysd((id, req) =>
        {
            // The second request finds the connection closed, as after a sysd restart.
            return Interlocked.Increment(ref calls) == 2 ? null : FakeSysd.Answer(id, req, []);
        });
        using var client = sysd.Client();

        Assert.True((await client.PingAsync()).IsOk);
        var status = await client.GetStatusAsync();

        Assert.True(status.IsOk, status.Detail);
        Assert.Equal(2, sysd.Connections);
    }

    [Fact]
    public async Task ServiceError_CarriesTheCode()
    {
        await using var sysd = new FakeSysd((id, _) => TestWire.Encode(new SysdResponse.Error(SysdErrorCode.Busy, "too many connections"), id));
        using var client = sysd.Client();

        var status = await client.GetStatusAsync();

        Assert.Equal(SysdOutcome.ServiceError, status.Outcome);
        Assert.Equal(SysdErrorCode.Busy, status.ErrorCode);
        Assert.Contains("too many connections", status.Detail);
    }

    [Fact]
    public async Task UnknownErrorCode_IsAFailure_AndKeepsItsNumber()
    {
        await using var sysd = new FakeSysd((id, _) => TestWire.Encode(new SysdResponse.Error((SysdErrorCode)999, ""), id));
        using var client = sysd.Client();

        var status = await client.GetStatusAsync();

        Assert.Equal(SysdOutcome.ServiceError, status.Outcome);
        Assert.Equal((SysdErrorCode)999, status.ErrorCode);
        Assert.False(status.ErrorCode!.Value.IsKnown());
    }

    [Fact]
    public async Task Garbage_IsProtocolError_AndDropsTheConnection()
    {
        await using var sysd = new FakeSysd((_, _) => "garbage!garbage"u8.ToArray());
        using var client = sysd.Client();

        var status = await client.GetStatusAsync();

        Assert.Equal(SysdOutcome.ProtocolError, status.Outcome);
        Assert.False(client.IsConnected);
    }

    [Fact]
    public async Task OtherVersion_IsProtocolError()
    {
        await using var sysd = new FakeSysd((id, _) =>
        {
            var p = TestWire.Encode(new SysdResponse.Pong(2), id);
            p[0] = 2;
            return p;
        });
        using var client = sysd.Client();

        var ping = await client.PingAsync();

        Assert.Equal(SysdOutcome.ProtocolError, ping.Outcome);
        Assert.Contains("version 2", ping.Detail);
    }

    [Fact]
    public async Task WrongId_IsProtocolError()
    {
        await using var sysd = new FakeSysd((id, req) => FakeSysd.Answer(id + 1000, req, []));
        using var client = sysd.Client();

        var status = await client.GetStatusAsync();

        Assert.Equal(SysdOutcome.ProtocolError, status.Outcome);
    }

    [Fact]
    public async Task WrongResponseKind_IsProtocolError()
    {
        await using var sysd = new FakeSysd((id, _) => TestWire.Encode(new SysdResponse.Pong(1), id));
        using var client = sysd.Client();

        var status = await client.GetStatusAsync();

        Assert.Equal(SysdOutcome.ProtocolError, status.Outcome);
        Assert.Contains("Pong", status.Detail);
    }

    [Fact]
    public async Task TrailingBytes_AreProtocolError()
    {
        await using var sysd = new FakeSysd((id, req) => [.. FakeSysd.Answer(id, req, []), 0]);
        using var client = sysd.Client();

        var status = await client.GetStatusAsync();

        Assert.Equal(SysdOutcome.ProtocolError, status.Outcome);
        Assert.Contains("trailing", status.Detail);
    }

    [Fact]
    public async Task DroppedConnection_OnFirstCall_IsProtocolError()
    {
        await using var sysd = new FakeSysd((_, _) => null);
        using var client = sysd.Client();

        var status = await client.GetStatusAsync();

        Assert.Equal(SysdOutcome.ProtocolError, status.Outcome);
        Assert.Equal(1, sysd.Connections);
    }

    [Theory]
    [InlineData(SysdWire.MaxPayloadBytes + 1)]
    [InlineData(7)]
    [InlineData(0)]
    public async Task OutOfRangeFrameLength_IsRejectedWithoutReadingIt(uint length)
    {
        var name = "andruna-test-" + Guid.NewGuid().ToString("N");
        await using var server = new NamedPipeServerStream(name, PipeDirection.InOut, 1, PipeTransmissionMode.Byte, PipeOptions.Asynchronous | PipeOptions.CurrentUserOnly);
        var serve = Task.Run(async () =>
        {
            await server.WaitForConnectionAsync();
            await SysdWire.ReadFrameAsync(server, CancellationToken.None);
            var prefix = new byte[4];
            BinaryPrimitives.WriteUInt32LittleEndian(prefix, length);
            await server.WriteAsync(prefix);
        });

        using var client = new SysdClient(name, TimeSpan.FromSeconds(2));
        var status = await client.GetStatusAsync();
        await serve;

        Assert.Equal(SysdOutcome.ProtocolError, status.Outcome);
        Assert.Contains("out of range", status.Detail);
    }

    [Fact]
    public async Task RequestFrame_IsLengthPrefixedBinary()
    {
        var name = "andruna-test-" + Guid.NewGuid().ToString("N");
        await using var server = new NamedPipeServerStream(name, PipeDirection.InOut, 1, PipeTransmissionMode.Byte, PipeOptions.Asynchronous | PipeOptions.CurrentUserOnly);
        var received = Task.Run(async () =>
        {
            await server.WaitForConnectionAsync();
            var frame = new byte[4 + 25];
            await server.ReadExactlyAsync(frame);
            return frame;
        });

        using var client = new SysdClient(name, TimeSpan.FromSeconds(2), TimeSpan.FromMilliseconds(500));
        _ = client.StartPackageAsync("com.example.app");

        // u32 length 25, version 1, kind 3, id 1, u16 length 15, "com.example.app"
        Assert.Equal(
            Convert.FromHexString("19000000" + "0100" + "0300" + "01000000" + "0f00" + "636f6d2e6578616d706c652e617070"),
            await received);
    }

    [Fact]
    public async Task Timeout_IsProtocolError()
    {
        var name = "andruna-test-" + Guid.NewGuid().ToString("N");
        await using var server = new NamedPipeServerStream(name, PipeDirection.InOut, 1, PipeTransmissionMode.Byte, PipeOptions.Asynchronous | PipeOptions.CurrentUserOnly);
        _ = server.WaitForConnectionAsync();

        using var client = new SysdClient(name, TimeSpan.FromSeconds(2), TimeSpan.FromMilliseconds(200));
        var status = await client.GetStatusAsync();

        Assert.Equal(SysdOutcome.ProtocolError, status.Outcome);
        Assert.Contains("in time", status.Detail);
    }
}
