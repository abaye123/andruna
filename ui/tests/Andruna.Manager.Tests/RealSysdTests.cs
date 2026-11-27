using System.Diagnostics;
using System.Runtime.CompilerServices;
using Andruna.Manager.Core.Sysd;

namespace Andruna.Manager.Tests;

/// <summary>
/// The client against the real andruna-sysd.exe from the Rust build, on its
/// own instance and data root, so a running product sysd is never touched.
/// The executable is taken from ANDRUNA_SYSD_EXE or target\&lt;triple&gt;\{release,debug}\;
/// when there is none (no Rust build on this machine) the test does nothing.
/// </summary>
public sealed class RealSysdTests : IDisposable
{
    private readonly string _root = Path.Combine(Path.GetTempPath(), "andruna-ui-real-" + Guid.NewGuid().ToString("N"));
    private Process? _sysd;

    public void Dispose()
    {
        if (_sysd is { HasExited: false })
        {
            _sysd.Kill();
            _sysd.WaitForExit(5000);
        }

        _sysd?.Dispose();
        if (Directory.Exists(_root))
        {
            Directory.Delete(_root, recursive: true);
        }
    }

    private static string? FindSysd([CallerFilePath] string here = "")
    {
        var fromEnv = Environment.GetEnvironmentVariable("ANDRUNA_SYSD_EXE");
        if (!string.IsNullOrEmpty(fromEnv))
        {
            return fromEnv;
        }

        var target = Path.GetFullPath(Path.Combine(Path.GetDirectoryName(here)!, "..", "..", "..", "target"));
        var triple = System.Runtime.InteropServices.RuntimeInformation.OSArchitecture == System.Runtime.InteropServices.Architecture.Arm64
            ? "aarch64-pc-windows-gnullvm"
            : "x86_64-pc-windows-gnullvm";
        return new[] { "release", "debug" }
            .Select(p => Path.Combine(target, triple, p, "andruna-sysd.exe"))
            .FirstOrDefault(File.Exists);
    }

    [Fact]
    public async Task Client_SpeaksTheProtocol_OfTheRealSysd()
    {
        var exe = FindSysd();
        if (exe is null)
        {
            return;
        }

        var instance = $"ui-test-{Environment.ProcessId}";
        Directory.CreateDirectory(_root);
        _sysd = Process.Start(new ProcessStartInfo(exe)
        {
            ArgumentList = { "--instance", instance, "--root", _root, "--idle-timeout-ms", "10000" },
            UseShellExecute = false,
        })!;

        using var client = new SysdClient(SysdEndpoint.PipeNameForCurrentUser(instance), TimeSpan.FromMilliseconds(200));
        var host = new FakeHost(installed: true, start: () => false);
        var connection = new ServiceConnection(client, host, TimeSpan.FromSeconds(5), TimeSpan.FromHours(1));

        // sysd needs a moment to create its pipe; the connection retries for up to 5 s.
        var snapshot = await connection.CheckAsync();
        for (var i = 0; i < 50 && snapshot.State != ServiceState.Running; i++)
        {
            await Task.Delay(100);
            snapshot = await connection.CheckAsync();
        }

        Assert.Equal(ServiceState.Running, snapshot.State);
        Assert.Equal((uint)_sysd.Id, snapshot.Status!.Pid);
        Assert.Equal((uint)_sysd.Id, client.ServerPid);
        Assert.Equal(0u, snapshot.Status.Hosts);
        Assert.Equal(10_000ul, snapshot.Status.IdleTimeoutMs);
        Assert.False(string.IsNullOrEmpty(snapshot.Status.Version));

        Assert.Equal(SysdWire.ProtocolVersion, (await client.PingAsync()).Value);
        Assert.Empty((await client.ListPackagesAsync()).Value!);

        var notInstalled = await client.StartPackageAsync("org.example.none");
        Assert.Equal(SysdOutcome.ServiceError, notInstalled.Outcome);
        Assert.Equal(SysdErrorCode.NotInstalled, notInstalled.ErrorCode);

        var invalid = await client.StartPackageAsync("not a package");
        Assert.Equal(SysdErrorCode.InvalidPackageName, invalid.ErrorCode);

        var focus = await client.FocusWindowAsync("org.example.none");
        Assert.Equal(SysdOutcome.ServiceError, focus.Outcome);

        // Installed packages are the directories app\<package>\.
        Directory.CreateDirectory(Path.Combine(_root, "app", "org.example.one"));
        Assert.Equal(["org.example.one"], (await client.ListPackagesAsync()).Value!.Select(p => p.Name));

        // All of it went over one kept connection, which sysd counts.
        var status = await client.GetStatusAsync();
        Assert.Equal(1u, status.Value!.Connections);
    }
}
