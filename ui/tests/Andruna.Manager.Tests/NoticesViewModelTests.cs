using System.Runtime.CompilerServices;
using System.Text;
using Andruna.Manager.Core.ViewModels;

namespace Andruna.Manager.Tests;

public sealed class NoticesViewModelTests : IDisposable
{
    private readonly string _dir = Path.Combine(Path.GetTempPath(), "andruna-notices-" + Guid.NewGuid().ToString("N"));

    public NoticesViewModelTests() => Directory.CreateDirectory(_dir);

    public void Dispose() => Directory.Delete(_dir, recursive: true);

    private string FilePath => Path.Combine(_dir, NoticesViewModel.FileName);

    [Fact]
    public void StartsLoading_WithNoTextAndNoProblem()
    {
        var vm = new NoticesViewModel(FilePath, new KeyStrings());

        Assert.Equal(NoticesState.Loading, vm.State);
        Assert.True(vm.IsLoading);
        Assert.False(vm.IsLoaded);
        Assert.Equal(string.Empty, vm.Text);
        Assert.False(vm.HasProblem);
    }

    [Fact]
    public async Task Loads_TheFileText()
    {
        // UTF-8 with a byte order mark and non-ASCII text: the mark is not part of the text.
        const string text = "Andruna third-party notices\r\n\r\nzlib (c) Jean-loup Gailly\nMIT é\n";
        await File.WriteAllTextAsync(FilePath, text, new UTF8Encoding(encoderShouldEmitUTF8Identifier: true));
        var vm = new NoticesViewModel(FilePath, new KeyStrings());

        await vm.LoadAsync();

        Assert.Equal(NoticesState.Loaded, vm.State);
        Assert.True(vm.IsLoaded);
        Assert.False(vm.IsLoading);
        Assert.Equal(text, vm.Text);
        Assert.Null(vm.Problem);
    }

    [Fact]
    public async Task MissingFile_IsReported_WithoutText()
    {
        var vm = new NoticesViewModel(FilePath, new KeyStrings());

        await vm.LoadAsync();

        Assert.Equal(NoticesState.Missing, vm.State);
        Assert.False(vm.IsLoaded);
        Assert.False(vm.IsLoading);
        Assert.Equal(string.Empty, vm.Text);
        Assert.True(vm.HasProblem);
        Assert.Equal("NoticesMissingTitle", vm.Problem!.Title);
        Assert.Equal("NoticesMissingMessage:" + NoticesViewModel.FileName, vm.Problem.Message);
    }

    [Fact]
    public async Task MissingFolder_IsReportedAsMissing()
    {
        var vm = new NoticesViewModel(Path.Combine(_dir, "gone", NoticesViewModel.FileName), new KeyStrings());

        await vm.LoadAsync();

        Assert.Equal(NoticesState.Missing, vm.State);
    }

    [Fact]
    public async Task UnreadableFile_IsReportedAsFailed()
    {
        await File.WriteAllTextAsync(FilePath, "notices");
        var vm = new NoticesViewModel(FilePath, new KeyStrings());

        // Another process holds the file open without sharing it.
        using (new FileStream(FilePath, FileMode.Open, FileAccess.ReadWrite, FileShare.None))
        {
            await vm.LoadAsync();
        }

        Assert.Equal(NoticesState.Failed, vm.State);
        Assert.Equal(string.Empty, vm.Text);
        Assert.Equal("NoticesReadFailedTitle", vm.Problem!.Title);
        Assert.StartsWith("NoticesReadFailedMessage:", vm.Problem.Message);
    }

    [Fact]
    public async Task Reload_AfterTheFileAppears_ClearsTheProblem()
    {
        var vm = new NoticesViewModel(FilePath, new KeyStrings());
        await vm.LoadAsync();
        Assert.Equal(NoticesState.Missing, vm.State);

        await File.WriteAllTextAsync(FilePath, "notices");
        await vm.LoadAsync();

        Assert.Equal(NoticesState.Loaded, vm.State);
        Assert.Equal("notices", vm.Text);
        Assert.False(vm.HasProblem);
    }

    [Fact]
    public async Task Cancel_Throws_AndLeavesNoText()
    {
        await File.WriteAllTextAsync(FilePath, "notices");
        var vm = new NoticesViewModel(FilePath, new KeyStrings());

        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => vm.LoadAsync(new CancellationToken(canceled: true)));
        Assert.Equal(string.Empty, vm.Text);
        Assert.False(vm.IsLoaded);
    }

    [Fact]
    public async Task ShippedNoticesFile_Loads()
    {
        // The repository file the manager project copies to its output as THIRD_PARTY_NOTICES.txt.
        var vm = new NoticesViewModel(Path.Combine(RepoRoot(), "THIRD_PARTY_NOTICES"), new KeyStrings());

        await vm.LoadAsync();

        Assert.Equal(NoticesState.Loaded, vm.State);
        Assert.False(string.IsNullOrWhiteSpace(vm.Text));
    }

    private static string RepoRoot([CallerFilePath] string here = "") =>
        Path.GetFullPath(Path.Combine(Path.GetDirectoryName(here)!, "..", "..", ".."));
}
