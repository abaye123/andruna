namespace Andruna.Manager;

/// <summary>
/// Debug builds only: environment variables that point the manager at a
/// development sysd instance and data tree and at a separate settings file,
/// so screenshots and manual tests never touch the user's real Andruna.
/// Release builds ignore them.
/// <list type="bullet">
/// <item><c>ANDRUNA_MANAGER_INSTANCE</c>: the sysd instance name (as <c>andruna-sysd --instance</c>).</item>
/// <item><c>ANDRUNA_MANAGER_ROOT</c>: the data tree (as <c>andruna-sysd --root</c>).</item>
/// <item><c>ANDRUNA_MANAGER_SETTINGS</c>: the settings file.</item>
/// </list>
/// </summary>
internal static class DebugOverrides
{
#if DEBUG
    public static string? PipeName => Environment.GetEnvironmentVariable("ANDRUNA_MANAGER_INSTANCE") is { Length: > 0 } instance
        ? Core.Sysd.SysdEndpoint.PipeNameForCurrentUser(instance)
        : null;

    public static string? DataRoot => Environment.GetEnvironmentVariable("ANDRUNA_MANAGER_ROOT") is { Length: > 0 } r ? r : null;

    public static string? SettingsPath => Environment.GetEnvironmentVariable("ANDRUNA_MANAGER_SETTINGS") is { Length: > 0 } s ? s : null;
#else
    public static string? PipeName => null;

    public static string? DataRoot => null;

    public static string? SettingsPath => null;
#endif
}
