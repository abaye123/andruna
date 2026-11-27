using System.Diagnostics;

namespace Andruna.Manager.Core.Platform;

/// <summary>Opens Android apps the way Start menu shortcuts do.</summary>
public interface IAppLauncher
{
    /// <summary>Starts <paramref name="package"/> (or one of its activities). Returns false with the reason when andruna-launch could not be run.</summary>
    bool TryOpen(string package, string? activity, out string? error);
}

/// <summary>
/// Runs andruna-launch.exe next to the manager with
/// <c>&lt;package&gt;[/&lt;activity&gt;]</c> (crates/andruna-launch/README.md).
/// andruna-launch shows its own error dialog when sysd refuses.
/// </summary>
public sealed class LaunchExecutable(string directory) : IAppLauncher
{
    public const string ExecutableName = "andruna-launch.exe";

    public string ExecutablePath { get; } = Path.Combine(directory, ExecutableName);

    /// <summary>The argument andruna-launch takes for a package and an optional activity.</summary>
    public static string Target(string package, string? activity) =>
        string.IsNullOrEmpty(activity) ? package : $"{package}/{activity}";

    public bool TryOpen(string package, string? activity, out string? error)
    {
        if (!File.Exists(ExecutablePath))
        {
            error = $"{ExecutableName} is missing";
            return false;
        }

        try
        {
            var info = new ProcessStartInfo(ExecutablePath) { UseShellExecute = false, WorkingDirectory = Path.GetDirectoryName(ExecutablePath) };
            info.ArgumentList.Add(Target(package, activity));
            using var process = Process.Start(info);
            error = process is null ? "the process did not start" : null;
            return process is not null;
        }
        catch (Exception e) when (e is System.ComponentModel.Win32Exception or InvalidOperationException or IOException)
        {
            error = e.Message;
            return false;
        }
    }
}
