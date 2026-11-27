using Andruna.Manager.Core.Localization;

namespace Andruna.Manager.Core.ViewModels;

/// <summary>One requested Android permission, with a plain-language description when one is known.</summary>
public sealed record PermissionItem(string Name, string Description, bool IsSensitive);

public static class Permissions
{
    /// <summary>
    /// Permissions with a description in the resources (key <c>Perm_&lt;NAME&gt;</c>), and
    /// whether Android treats them as dangerous (runtime) permissions.
    /// </summary>
    public static readonly IReadOnlyDictionary<string, bool> Known = new Dictionary<string, bool>(StringComparer.Ordinal)
    {
        ["INTERNET"] = false,
        ["ACCESS_NETWORK_STATE"] = false,
        ["POST_NOTIFICATIONS"] = true,
        ["CAMERA"] = true,
        ["RECORD_AUDIO"] = true,
        ["ACCESS_FINE_LOCATION"] = true,
        ["ACCESS_COARSE_LOCATION"] = true,
        ["READ_CONTACTS"] = true,
        ["READ_EXTERNAL_STORAGE"] = true,
        ["WRITE_EXTERNAL_STORAGE"] = true,
        ["READ_MEDIA_IMAGES"] = true,
        ["READ_MEDIA_VIDEO"] = true,
        ["BLUETOOTH_CONNECT"] = true,
        ["READ_PHONE_STATE"] = true,
        ["VIBRATE"] = false,
        ["WAKE_LOCK"] = false,
        ["RECEIVE_BOOT_COMPLETED"] = false,
        ["FOREGROUND_SERVICE"] = false,
    };

    private const string AndroidPrefix = "android.permission.";

    /// <summary>The permissions as the user reads them: sensitive ones first, then known ones, then the rest by name.</summary>
    public static IReadOnlyList<PermissionItem> Describe(IEnumerable<string> names, IStrings strings) => names
        .Distinct(StringComparer.Ordinal)
        .Select(n =>
        {
            var shortName = n.StartsWith(AndroidPrefix, StringComparison.Ordinal) ? n[AndroidPrefix.Length..] : null;
            return shortName is not null && Known.TryGetValue(shortName, out var sensitive)
                ? new PermissionItem(n, strings.Get("Perm_" + shortName), sensitive)
                : new PermissionItem(n, strings.Get("PermOther"), false);
        })
        .OrderByDescending(p => p.IsSensitive)
        .ThenBy(p => p.Description == strings.Get("PermOther"))
        .ThenBy(p => p.Name, StringComparer.Ordinal)
        .ToList();
}
