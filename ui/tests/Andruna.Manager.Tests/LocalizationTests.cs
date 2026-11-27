using System.Globalization;
using System.Runtime.CompilerServices;
using System.Text.RegularExpressions;
using System.Xml.Linq;
using Andruna.Manager.Core.Localization;

namespace Andruna.Manager.Tests;

public partial class LocalizationTests
{
    private static string UiDir([CallerFilePath] string here = "") =>
        Path.GetFullPath(Path.Combine(Path.GetDirectoryName(here)!, "..", ".."));

    private static string StringsDir => Path.Combine(UiDir(), "src", "Andruna.Manager", "Strings");

    private static Dictionary<string, string> Load(string language) =>
        XDocument.Load(Path.Combine(StringsDir, language, "Resources.resw"))
            .Root!.Elements("data")
            .ToDictionary(e => (string)e.Attribute("name")!, e => (string)e.Element("value")!);

    [Theory]
    [InlineData(AppLanguage.Hebrew, "en-US", Languages.Hebrew)]
    [InlineData(AppLanguage.English, "he-IL", Languages.English)]
    [InlineData(AppLanguage.System, "he-IL", Languages.Hebrew)]
    [InlineData(AppLanguage.System, "he", Languages.Hebrew)]
    [InlineData(AppLanguage.System, "en-GB", Languages.English)]
    [InlineData(AppLanguage.System, "fr-FR", Languages.English)]
    public void Resolve_PicksAShippedLanguage(AppLanguage choice, string system, string expected) =>
        Assert.Equal(expected, Languages.Resolve(choice, CultureInfo.GetCultureInfo(system)));

    [Fact]
    public void Hebrew_IsRightToLeft_EnglishIsNot()
    {
        Assert.True(Languages.IsRightToLeft(Languages.Hebrew));
        Assert.False(Languages.IsRightToLeft(Languages.English));
    }

    [Fact]
    public void BothLanguages_HaveTheSameKeys_AndNoEmptyValues()
    {
        var en = Load("en-US");
        var he = Load("he-IL");

        Assert.Equal(en.Keys.Order(), he.Keys.Order());
        Assert.All(en.Concat(he), kv => Assert.False(string.IsNullOrWhiteSpace(kv.Value), kv.Key));
    }

    [Fact]
    public void FormatStrings_HaveTheSamePlaceholders()
    {
        var en = Load("en-US");
        var he = Load("he-IL");
        foreach (var key in en.Keys)
        {
            var a = Placeholder().Matches(en[key]).Select(m => m.Value).Order();
            var b = Placeholder().Matches(he[key]).Select(m => m.Value).Order();
            Assert.True(a.SequenceEqual(b), key);
        }
    }

    [Fact]
    public void NoLongDashes()
    {
        foreach (var value in Load("en-US").Values.Concat(Load("he-IL").Values))
        {
            Assert.DoesNotContain('\u2013', value);
            Assert.DoesNotContain('\u2014', value);
        }
    }

    [Fact]
    public void InstallerLanguages_HaveTheSameStrings()
    {
        var lang = Path.Combine(UiDir(), "..", "installer", "lang");
        string[] Keys(string file) => File.ReadAllLines(Path.Combine(lang, file))
            .Select(l => LangString().Match(l))
            .Where(m => m.Success)
            .Select(m => m.Groups["key"].Value)
            .Order()
            .ToArray();

        Assert.NotEmpty(Keys("en-US.nsh"));
        Assert.Equal(Keys("en-US.nsh"), Keys("he-IL.nsh"));
    }

    [Fact]
    public void EveryKeyUsedInCode_Exists()
    {
        var keys = Load("en-US").Keys.ToHashSet();
        var used = new HashSet<string>();
        foreach (var file in Directory.EnumerateFiles(Path.Combine(UiDir(), "src"), "*.*", SearchOption.AllDirectories)
                     .Where(f => f.EndsWith(".cs") || f.EndsWith(".xaml"))
                     .Where(f => !f.Contains($"{Path.DirectorySeparatorChar}obj{Path.DirectorySeparatorChar}")))
        {
            var text = File.ReadAllText(file);
            foreach (Match m in UsedKey().Matches(text))
            {
                used.Add(m.Groups["key"].Value);
            }
        }

        Assert.NotEmpty(used);
        Assert.Equal(string.Empty, string.Join(", ", used.Where(k => !keys.Contains(k))));
    }

    [GeneratedRegex(@"^LangString (?<key>\w+) ")]
    private static partial Regex LangString();

    [GeneratedRegex(@"\{\d+\}")]
    private static partial Regex Placeholder();

    // Get("Key"), Format("Key", ...), {l:Str Key=Key}
    [GeneratedRegex(@"(?:\b(?:Get|Format)\(""(?<key>[A-Za-z]+)""|Key=(?<key>[A-Za-z]+)\})")]
    private static partial Regex UsedKey();
}
