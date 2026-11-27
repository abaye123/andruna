using System.Security.Cryptography;
using System.Text;
using Andruna.Manager.Core.Localization;
using Andruna.Manager.Core.Sysd;

namespace Andruna.Manager.Core.Events;

/// <summary>
/// A Windows app notification for an Android notification. Windows limits
/// tag and group to 64 characters, so they are derived from the Android
/// identity (package, id, tag) and hashed when longer.
/// </summary>
public sealed record ToastRequest(string Tag, string Group, string Title, string Text, string Attribution, string Arguments);

/// <summary>Shows and removes Windows app notifications (the Windows App SDK in the app, a recorder in tests).</summary>
public interface IToastPresenter
{
    void Show(ToastRequest toast);

    void Remove(string tag, string group);
}

/// <summary>An app process that ended abnormally, explained for the user (P-23).</summary>
public sealed record CrashReport(string Package, string Label, uint Pid, uint ExitCode, string Reason, DateTimeOffset Time, string Title, string Explanation);

/// <summary>Shows the crash dialog.</summary>
public interface ICrashPresenter
{
    void ShowCrash(CrashReport report);
}

/// <summary>
/// The bridge from sysd events to Windows: notifications become app
/// notifications whose click opens the app through andruna-launch, crashes
/// open the crash dialog.
/// </summary>
public sealed class NotificationBridge(IToastPresenter toasts, ICrashPresenter crashes, Func<string, string?> labelOf, IStrings strings) : IEventSink
{
    /// <summary>Most toasts shown per app and minute; Android apps can post far more than Windows should show.</summary>
    public const int MaxToastsPerMinute = 20;

    private readonly Dictionary<string, Queue<DateTime>> _recent = new(StringComparer.Ordinal);

    /// <summary>Notifications not shown because an app posted too many.</summary>
    public int Throttled { get; private set; }

    public void OnEvent(SysdEvent e)
    {
        switch (e)
        {
            case SysdEvent.NotificationPosted n:
                if (!Allow(n.Package))
                {
                    Throttled++;
                    return;
                }

                var label = labelOf(n.Package) ?? n.Package;
                toasts.Show(new ToastRequest(
                    TagFor(n.Id, n.Tag),
                    GroupFor(n.Package),
                    string.IsNullOrWhiteSpace(n.Title) ? label : n.Title,
                    n.Text,
                    label,
                    OpenArguments(n.Package)));
                break;
            case SysdEvent.NotificationRemoved r:
                toasts.Remove(TagFor(r.Id, r.Tag), GroupFor(r.Package));
                break;
            case SysdEvent.AppCrashed c:
                crashes.ShowCrash(Explain(c, labelOf(c.Package) ?? c.Package, strings));
                break;
        }
    }

    /// <summary>The arguments a notification click carries.</summary>
    public static string OpenArguments(string package) => $"action=open;package={package}";

    /// <summary>The package a notification click asks to open, or null for other arguments.</summary>
    public static string? PackageToOpen(string arguments)
    {
        var parts = arguments.Split(';', StringSplitOptions.RemoveEmptyEntries)
            .Select(p => p.Split('=', 2))
            .Where(p => p.Length == 2)
            .ToDictionary(p => p[0], p => p[1], StringComparer.Ordinal);
        return parts.GetValueOrDefault("action") == "open" && parts.TryGetValue("package", out var package) && PackageNames.IsValid(package)
            ? package
            : null;
    }

    public static string TagFor(int id, string tag) => Fit($"{id}:{tag}");

    public static string GroupFor(string package) => Fit(package);

    /// <summary>The user-facing explanation of a crash.</summary>
    public static CrashReport Explain(SysdEvent.AppCrashed c, string label, IStrings strings)
    {
        var reason = c.Reason.Trim();
        var explanation = !string.IsNullOrEmpty(reason)
            ? strings.Format("CrashReasonApp", reason)
            : c.ExitCode switch
            {
                0xC0000005 => strings.Get("CrashReasonAccessViolation"),
                0xC00000FD => strings.Get("CrashReasonStackOverflow"),
                0xC0000409 => strings.Get("CrashReasonFastFail"),
                0xC0000374 => strings.Get("CrashReasonHeapCorruption"),
                0xC0000017 or 0xC000012D => strings.Get("CrashReasonOutOfMemory"),
                _ => strings.Format("CrashReasonExitCode", $"0x{c.ExitCode:X8}"),
            };
        return new CrashReport(
            c.Package,
            label,
            c.Pid,
            c.ExitCode,
            reason,
            DateTimeOffset.FromUnixTimeMilliseconds((long)Math.Min(c.TimeMs, (ulong)long.MaxValue)),
            strings.Format("CrashTitle", label),
            explanation);
    }

    private bool Allow(string package)
    {
        var now = DateTime.UtcNow;
        if (!_recent.TryGetValue(package, out var times))
        {
            times = new Queue<DateTime>();
            _recent[package] = times;
        }

        while (times.Count > 0 && now - times.Peek() > TimeSpan.FromMinutes(1))
        {
            times.Dequeue();
        }

        if (times.Count >= MaxToastsPerMinute)
        {
            return false;
        }

        times.Enqueue(now);
        return true;
    }

    private static string Fit(string value)
    {
        if (value.Length <= 64)
        {
            return value;
        }

        var hash = Convert.ToHexString(SHA256.HashData(Encoding.UTF8.GetBytes(value)));
        return value[..24] + "~" + hash[..32];
    }
}

/// <summary>Android package names (the rule of crates/andruna-ipc is_valid_package_name).</summary>
public static class PackageNames
{
    public static bool IsValid(string name)
    {
        if (name.Length is 0 or > 255)
        {
            return false;
        }

        var segments = name.Split('.');
        return segments.Length >= 2 && segments.All(s =>
            s.Length > 0 && char.IsAsciiLetter(s[0]) && s.All(c => char.IsAsciiLetterOrDigit(c) || c == '_'));
    }
}
