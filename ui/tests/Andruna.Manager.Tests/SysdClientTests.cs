using System.Buffers.Binary;
using System.IO.Pipes;
using System.Text;
using Andruna.Manager.Core.Sysd;

namespace Andruna.Manager.Tests;

public class SysdClientTests
{
    [Fact]
    public async Task NoServer_IsNotRunning()
    {
        var client = new SysdClient("andruna-test-absent-" + Guid.NewGuid().ToString("N"), TimeSpan.FromMilliseconds(200));

        var status = await client.GetStatusAsync();

        Assert.Equal(SysdOutcome.NotRunning, status.Outcome);
        Assert.Null(status.Value);
    }

    [Fact]
    public async Task Status_IsDecoded()
    {
        await using var sysd = FakeSysd.Running();

        var status = await sysd.Client().GetStatusAsync();

        Assert.True(status.IsOk, status.Detail);
        Assert.Equal("0.1.0", status.Value!.Version);
        Assert.Equal(SysdWire.ProtocolVersion, status.Value.Protocol);
        Assert.Equal(1234, status.Value.UptimeMs);
    }

    [Fact]
    public async Task Ping_Succeeds()
    {
        await using var sysd = FakeSysd.Running();

        var ping = await sysd.Client().PingAsync();

        Assert.True(ping.IsOk, ping.Detail);
    }

    [Fact]
    public async Task ListPackages_IsDecoded()
    {
        await using var sysd = FakeSysd.Running(
            new PackageSummary("org.example.one", "One", "1.2", 12),
            new PackageSummary("org.example.two", null, null, 1));

        var list = await sysd.Client().ListPackagesAsync();

        Assert.True(list.IsOk, list.Detail);
        Assert.Equal(2, list.Value!.Count);
        Assert.Equal("org.example.one", list.Value[0].Name);
        Assert.Equal("1.2", list.Value[0].VersionName);
        Assert.Null(list.Value[1].Label);
    }

    [Fact]
    public async Task ServiceError_IsReported()
    {
        await using var sysd = new FakeSysd((_, id) => FakeSysd.Error(id, "busy", "try later"));

        var status = await sysd.Client().GetStatusAsync();

        Assert.Equal(SysdOutcome.ServiceError, status.Outcome);
        Assert.Contains("busy", status.Detail);
    }

    [Fact]
    public async Task MalformedJson_IsProtocolError()
    {
        await using var sysd = new FakeSysd((_, _) => Encoding.UTF8.GetBytes("{not json"));

        var status = await sysd.Client().GetStatusAsync();

        Assert.Equal(SysdOutcome.ProtocolError, status.Outcome);
    }

    [Fact]
    public async Task WrongId_IsProtocolError()
    {
        await using var sysd = new FakeSysd((_, _) => FakeSysd.Result(999_999, new SysdStatus("1", 1, 1)));

        var status = await sysd.Client().GetStatusAsync();

        Assert.Equal(SysdOutcome.ProtocolError, status.Outcome);
    }

    [Fact]
    public async Task IncompleteResult_IsProtocolError()
    {
        await using var sysd = new FakeSysd((_, id) => FakeSysd.Result(id, new { protocol = 1 }));

        var status = await sysd.Client().GetStatusAsync();

        Assert.Equal(SysdOutcome.ProtocolError, status.Outcome);
    }

    [Fact]
    public async Task PackageWithoutName_IsProtocolError()
    {
        await using var sysd = new FakeSysd((_, id) => FakeSysd.Result(id, new { packages = new[] { new { label = "x" } } }));

        var list = await sysd.Client().ListPackagesAsync();

        Assert.Equal(SysdOutcome.ProtocolError, list.Outcome);
    }

    [Fact]
    public async Task DroppedConnection_IsProtocolError()
    {
        await using var sysd = new FakeSysd((_, _) => null);

        var status = await sysd.Client().GetStatusAsync();

        Assert.Equal(SysdOutcome.ProtocolError, status.Outcome);
    }

    [Fact]
    public async Task OversizedFrame_IsRejectedWithoutReadingIt()
    {
        var name = "andruna-test-" + Guid.NewGuid().ToString("N");
        await using var server = new NamedPipeServerStream(name, PipeDirection.InOut, 1, PipeTransmissionMode.Byte, PipeOptions.Asynchronous);
        var serve = Task.Run(async () =>
        {
            await server.WaitForConnectionAsync();
            await SysdWire.ReadFrameAsync(server, CancellationToken.None);
            var header = new byte[4];
            BinaryPrimitives.WriteUInt32LittleEndian(header, SysdWire.MaxMessageBytes + 1);
            await server.WriteAsync(header);
        });

        var status = await new SysdClient(name, TimeSpan.FromSeconds(2)).GetStatusAsync();
        await serve;

        Assert.Equal(SysdOutcome.ProtocolError, status.Outcome);
        Assert.Contains("exceeds", status.Detail);
    }

    [Fact]
    public async Task RequestFrame_IsLengthPrefixedSnakeCaseJson()
    {
        var name = "andruna-test-" + Guid.NewGuid().ToString("N");
        await using var server = new NamedPipeServerStream(name, PipeDirection.InOut, 1, PipeTransmissionMode.Byte, PipeOptions.Asynchronous);
        var received = Task.Run(async () =>
        {
            await server.WaitForConnectionAsync();
            var header = new byte[4];
            await server.ReadExactlyAsync(header);
            var payload = new byte[BinaryPrimitives.ReadUInt32LittleEndian(header)];
            await server.ReadExactlyAsync(payload);
            return Encoding.UTF8.GetString(payload);
        });

        _ = new SysdClient(name, TimeSpan.FromSeconds(2), TimeSpan.FromMilliseconds(500)).ListPackagesAsync();

        Assert.Equal("{\"id\":1,\"method\":\"list_packages\"}", await received);
    }
}
