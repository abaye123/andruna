namespace Andruna.Manager.Core.Platform;

/// <summary>
/// What the manager was started (or activated again) for, from its command
/// line:
/// <list type="bullet">
/// <item><c>--page library|settings|notices|diagnostics</c>: open that page (default: library).</item>
/// <item><c>--app &lt;package&gt;</c>: the page of one installed app.</item>
/// <item><c>--install &lt;file&gt;</c>, or a package file as the only argument (the file association): the install dialog.</item>
/// <item><c>--diagnostics &lt;package&gt;</c>: the diagnostics page for one app (from the crash dialog).</item>
/// <item><c>--background</c>: no window; only the notification bridge (andruna-launch starts this).</item>
/// </list>
/// </summary>
public sealed record LaunchRequest(string Page, string? Package = null, string? InstallFile = null, bool Background = false)
{
    /// <summary>Package files the manager installs (P-3).</summary>
    public static readonly IReadOnlyList<string> PackageFileTypes = [".apk", ".apks", ".xapk", ".apkm"];

    public static bool IsPackageFile(string path) =>
        PackageFileTypes.Contains(Path.GetExtension(path), StringComparer.OrdinalIgnoreCase);

    /// <summary>Parses the arguments, without the program name.</summary>
    public static LaunchRequest Parse(IReadOnlyList<string> args)
    {
        var request = new LaunchRequest("library");
        for (var i = 0; i < args.Count; i++)
        {
            var next = i + 1 < args.Count ? args[i + 1] : null;
            switch (args[i])
            {
                case "--background":
                    request = request with { Background = true };
                    break;
                case "--page" when next is not null:
                    request = request with { Page = next };
                    i++;
                    break;
                case "--app" when next is not null:
                    request = request with { Page = "app", Package = next };
                    i++;
                    break;
                case "--diagnostics" when next is not null:
                    request = request with { Page = "diagnostics", Package = next };
                    i++;
                    break;
                case "--install" when next is not null:
                    request = request with { InstallFile = next };
                    i++;
                    break;
                case var a when !a.StartsWith("--", StringComparison.Ordinal) && IsPackageFile(a):
                    request = request with { InstallFile = a };
                    break;
            }
        }

        return request;
    }

    /// <summary>
    /// Splits a Windows command line (as an activation redirected from a
    /// second instance carries it) into arguments with the rules of
    /// CommandLineToArgvW, and drops the program path when it is the first
    /// argument.
    /// </summary>
    public static IReadOnlyList<string> SplitCommandLine(string commandLine)
    {
        var args = new List<string>();
        var current = new System.Text.StringBuilder();
        var inQuotes = false;
        var any = false;
        for (var i = 0; i < commandLine.Length; i++)
        {
            var c = commandLine[i];
            if (c == '\\')
            {
                var slashes = 0;
                while (i < commandLine.Length && commandLine[i] == '\\')
                {
                    slashes++;
                    i++;
                }

                if (i < commandLine.Length && commandLine[i] == '"')
                {
                    current.Append('\\', slashes / 2);
                    if (slashes % 2 == 1)
                    {
                        current.Append('"');
                    }
                    else
                    {
                        inQuotes = !inQuotes;
                    }
                }
                else
                {
                    current.Append('\\', slashes);
                    i--;
                }

                any = true;
            }
            else if (c == '"')
            {
                inQuotes = !inQuotes;
                any = true;
            }
            else if (char.IsWhiteSpace(c) && !inQuotes)
            {
                if (any)
                {
                    args.Add(current.ToString());
                    current.Clear();
                    any = false;
                }
            }
            else
            {
                current.Append(c);
                any = true;
            }
        }

        if (any)
        {
            args.Add(current.ToString());
        }

        if (args.Count > 0 && args[0].EndsWith(".exe", StringComparison.OrdinalIgnoreCase))
        {
            args.RemoveAt(0);
        }

        return args;
    }
}
