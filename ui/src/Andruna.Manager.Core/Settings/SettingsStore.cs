using System.Text.Json;
using System.Text.Json.Serialization;
using Andruna.Manager.Core.Localization;

namespace Andruna.Manager.Core.Settings;

/// <summary>The theme choice in settings.</summary>
public enum AppTheme
{
    System,
    Light,
    Dark,
}

/// <summary>Preferences of the management app itself.</summary>
public sealed record ManagerSettings
{
    public AppLanguage Language { get; init; } = AppLanguage.System;

    public AppTheme Theme { get; init; } = AppTheme.System;
}

/// <summary>
/// Reads and writes <see cref="ManagerSettings"/> as JSON. A missing or
/// unreadable file gives the defaults; writes replace the file atomically.
/// </summary>
public sealed class SettingsStore(string path)
{
    private static readonly JsonSerializerOptions Options = new()
    {
        WriteIndented = true,
        PropertyNamingPolicy = JsonNamingPolicy.SnakeCaseLower,
        Converters = { new JsonStringEnumConverter(JsonNamingPolicy.SnakeCaseLower, allowIntegerValues: false) },
    };

    /// <summary>%LOCALAPPDATA%\Andruna\system\manager.json (see 02-architecture.md, "Data on disk").</summary>
    public static string DefaultPath => System.IO.Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
        "Andruna",
        "system",
        "manager.json");

    public string Path { get; } = path;

    public ManagerSettings Load()
    {
        try
        {
            using var stream = File.OpenRead(Path);
            return JsonSerializer.Deserialize<ManagerSettings>(stream, Options) ?? new ManagerSettings();
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException or JsonException)
        {
            return new ManagerSettings();
        }
    }

    public void Save(ManagerSettings settings)
    {
        Directory.CreateDirectory(System.IO.Path.GetDirectoryName(Path)!);
        var temp = Path + ".tmp";
        File.WriteAllBytes(temp, JsonSerializer.SerializeToUtf8Bytes(settings, Options));
        File.Move(temp, Path, overwrite: true);
    }
}
