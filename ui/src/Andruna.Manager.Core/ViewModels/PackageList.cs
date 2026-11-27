using Andruna.Manager.Core.Sysd;

namespace Andruna.Manager.Core.ViewModels;

/// <summary>
/// Loads the installed packages: with label, version, icon and size when sysd
/// knows ListPackageInfo (0x0009), otherwise only their names
/// (ListPackages, 0x0005), and says which.
/// </summary>
public static class PackageList
{
    /// <summary>The packages, and whether sysd reported details for them.</summary>
    public static async Task<(SysdResult<IReadOnlyList<PackageSummary>> Result, bool HasDetails)> LoadAsync(ISysdClient client, string locale)
    {
        var infos = await client.ListPackageInfoAsync(locale);
        if (infos.IsOk)
        {
            return (SysdResult<IReadOnlyList<PackageSummary>>.Ok(
                infos.Value!.Select(i => new PackageSummary(i.Package, i)).ToList()), true);
        }

        if (!infos.IsUnsupported)
        {
            return (infos.Cast<IReadOnlyList<PackageSummary>>(), false);
        }

        return (await client.ListPackagesAsync(), false);
    }

    /// <summary>Bytes as the user reads them: "12.3 MB" (decimal units, as Windows Settings shows app sizes).</summary>
    public static string FormatSize(ulong bytes, IFormatProvider? culture = null)
    {
        string[] units = ["B", "KB", "MB", "GB", "TB"];
        double value = bytes;
        var unit = 0;
        while (value >= 1000 && unit < units.Length - 1)
        {
            value /= 1000;
            unit++;
        }

        return unit == 0
            ? string.Format(culture, "{0} {1}", bytes, units[0])
            : string.Format(culture, "{0:0.#} {1}", value, units[unit]);
    }

    /// <summary>
    /// "1.2 (7)": the version name with the version code, isolated as left to
    /// right text (U+2066 ... U+2069) so it reads the same inside Hebrew text.
    /// </summary>
    public static string FormatVersion(PackageInfo info) => "\u2066" +
        (string.IsNullOrEmpty(info.VersionName) ? info.VersionCode.ToString(System.Globalization.CultureInfo.InvariantCulture) : $"{info.VersionName} ({info.VersionCode})") +
        "\u2069";
}
