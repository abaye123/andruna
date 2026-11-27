namespace Andruna.Manager.Core.Platform;

/// <summary>
/// The data directory tree (02-architecture.md, "Data on disk"):
/// <c>%LOCALAPPDATA%\Andruna</c> with <c>app\</c>, <c>data\</c>, <c>logs\</c>
/// and <c>diagnostics\</c>.
/// </summary>
public sealed class AndrunaPaths(string root)
{
    /// <summary>The tree of the current user.</summary>
    public static AndrunaPaths ForCurrentUser() => new(Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "Andruna"));

    public string Root { get; } = root;

    public string Logs => Path.Combine(Root, "logs");

    /// <summary>Installed APKs and compiled code of a package.</summary>
    public string AppDir(string package) => Path.Combine(Root, "app", package);

    /// <summary>The private data of a package.</summary>
    public string DataDir(string package) => Path.Combine(Root, "data", package);

    /// <summary>
    /// The installed APK files of a package (base and splits), or none when
    /// the package directory has no APKs.
    /// </summary>
    public IReadOnlyList<string> InstalledApks(string package)
    {
        try
        {
            return Directory.GetFiles(AppDir(package), "*.apk").Order(StringComparer.OrdinalIgnoreCase).ToList();
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException)
        {
            return [];
        }
    }

    /// <summary>Total size of the files under a directory; 0 when it does not exist.</summary>
    public static ulong DirectorySize(string directory)
    {
        if (!Directory.Exists(directory))
        {
            return 0;
        }

        ulong total = 0;
        var options = new EnumerationOptions { RecurseSubdirectories = true, IgnoreInaccessible = true, AttributesToSkip = FileAttributes.ReparsePoint };
        foreach (var file in new DirectoryInfo(directory).EnumerateFiles("*", options))
        {
            total += (ulong)file.Length;
        }

        return total;
    }

    /// <summary>The last <paramref name="maxLines"/> lines of a log file that sysd may be writing; empty when it does not exist.</summary>
    public static IReadOnlyList<string> Tail(string path, int maxLines)
    {
        try
        {
            using var stream = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.ReadWrite | FileShare.Delete);
            using var reader = new StreamReader(stream);
            var lines = new Queue<string>(maxLines);
            while (reader.ReadLine() is { } line)
            {
                if (lines.Count == maxLines)
                {
                    lines.Dequeue();
                }

                lines.Enqueue(line);
            }

            return [.. lines];
        }
        catch (Exception e) when (e is FileNotFoundException or DirectoryNotFoundException)
        {
            return [];
        }
    }
}
