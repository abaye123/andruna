using System.Globalization;

namespace Andruna.Manager.Core.Localization;

/// <summary>Looks up user-facing text from the per-language resource files.</summary>
public interface IStrings
{
    /// <summary>The string for <paramref name="key"/> in the current language.</summary>
    string Get(string key);

    /// <summary>The string for <paramref name="key"/> with <c>{0}</c>-style arguments filled in.</summary>
    string Format(string key, params object?[] args) =>
        string.Format(CultureInfo.CurrentCulture, Get(key), args);
}

/// <summary>The language choice in settings.</summary>
public enum AppLanguage
{
    System,
    Hebrew,
    English,
}

/// <summary>Maps the language choice to the resource language the app uses.</summary>
public static class Languages
{
    public const string Hebrew = "he-IL";
    public const string English = "en-US";

    /// <summary>
    /// The resource language for a choice. "System" follows the Windows display
    /// language: Hebrew if it is Hebrew, English otherwise (the only two languages
    /// the app ships).
    /// </summary>
    public static string Resolve(AppLanguage choice, CultureInfo systemUiCulture) => choice switch
    {
        AppLanguage.Hebrew => Hebrew,
        AppLanguage.English => English,
        _ => IsHebrew(systemUiCulture) ? Hebrew : English,
    };

    public static bool IsRightToLeft(string languageTag) =>
        CultureInfo.GetCultureInfo(languageTag).TextInfo.IsRightToLeft;

    private static bool IsHebrew(CultureInfo culture)
    {
        // "iw" is the legacy code some systems still report for Hebrew.
        for (var c = culture; !Equals(c, CultureInfo.InvariantCulture); c = c.Parent)
        {
            if (c.Name is "he" or "iw")
            {
                return true;
            }
        }

        return false;
    }
}
