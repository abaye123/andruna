using Andruna.Manager.Core.Localization;
using Microsoft.UI.Xaml.Markup;
using Microsoft.Windows.ApplicationModel.Resources;

namespace Andruna.Manager.Localization;

/// <summary>
/// <see cref="IStrings"/> over the .resw files (Strings/&lt;language&gt;/Resources.resw),
/// for one fixed language chosen at startup.
/// </summary>
public sealed class ResourceStrings : IStrings
{
    private readonly ResourceContext _context;
    private readonly ResourceMap _map;

    public ResourceStrings(string language)
    {
        var manager = new ResourceManager();
        _context = manager.CreateResourceContext();
        _context.QualifierValues["Language"] = language;
        _map = manager.MainResourceMap.GetSubtree("Resources");
        Language = language;
    }

    public string Language { get; }

    public string Get(string key)
    {
        var candidate = _map.TryGetValue(key, _context);

        // A missing key is a bug; the unit tests check that every key used exists
        // in every language. Show the key rather than crash.
        return candidate?.ValueAsString ?? $"[{key}]";
    }
}

/// <summary>The strings of the running app.</summary>
public static class Loc
{
    private static IStrings? _strings;

    public static IStrings Strings
    {
        get => _strings ?? throw new InvalidOperationException("strings are used before startup set the language");
        set => _strings = value;
    }

    public static string Get(string key) => Strings.Get(key);

    public static string Format(string key, params object?[] args) => Strings.Format(key, args);
}

/// <summary>XAML markup extension: <c>Text="{l:Str Key=LibraryTitle}"</c>.</summary>
[MarkupExtensionReturnType(ReturnType = typeof(string))]
public sealed class Str : MarkupExtension
{
    public string Key { get; set; } = string.Empty;

    protected override object ProvideValue() => Loc.Get(Key);
}
