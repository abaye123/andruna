using Andruna.Manager.Core.Localization;
using Andruna.Manager.Core.Mvvm;

namespace Andruna.Manager.Core.ViewModels;

/// <summary>Where loading the notices file stands.</summary>
public enum NoticesState
{
    Loading,
    Loaded,
    Missing,
    Failed,
}

/// <summary>
/// The third-party notices viewer (M-6): reads THIRD_PARTY_NOTICES.txt, which
/// ships next to the manager, off the UI thread. A missing or unreadable file
/// is reported as such; the page never shows an empty text as if it were the
/// notices.
/// </summary>
public sealed class NoticesViewModel : ObservableObject
{
    /// <summary>The file name of the notices next to the manager executable.</summary>
    public const string FileName = "THIRD_PARTY_NOTICES.txt";

    private readonly IStrings _strings;
    private NoticesState _state = NoticesState.Loading;
    private string _text = string.Empty;
    private UserMessage? _problem;

    public NoticesViewModel(string path, IStrings strings)
    {
        FilePath = path;
        _strings = strings;
    }

    /// <summary>The full path of the notices file.</summary>
    public string FilePath { get; }

    public NoticesState State
    {
        get => _state;
        private set
        {
            if (SetProperty(ref _state, value))
            {
                OnPropertyChanged(nameof(IsLoading));
                OnPropertyChanged(nameof(IsLoaded));
            }
        }
    }

    public bool IsLoading => State == NoticesState.Loading;

    /// <summary>True when <see cref="Text"/> holds the notices; the file can then also be opened outside the app.</summary>
    public bool IsLoaded => State == NoticesState.Loaded;

    /// <summary>The notices, with line endings as in the file. Empty unless loaded.</summary>
    public string Text
    {
        get => _text;
        private set => SetProperty(ref _text, value);
    }

    /// <summary>Why the notices cannot be shown, or null.</summary>
    public UserMessage? Problem
    {
        get => _problem;
        private set
        {
            if (SetProperty(ref _problem, value))
            {
                OnPropertyChanged(nameof(HasProblem));
            }
        }
    }

    public bool HasProblem => Problem is not null;

    public async Task LoadAsync(CancellationToken cancellationToken = default)
    {
        State = NoticesState.Loading;
        Problem = null;
        Text = string.Empty;
        try
        {
            // ReadAllTextAsync detects a byte order mark and defaults to UTF-8.
            var text = await File.ReadAllTextAsync(FilePath, cancellationToken);
            Text = text;
            State = NoticesState.Loaded;
        }
        catch (Exception e) when (e is FileNotFoundException or DirectoryNotFoundException)
        {
            Problem = new UserMessage(_strings.Get("NoticesMissingTitle"), _strings.Format("NoticesMissingMessage", FileName));
            State = NoticesState.Missing;
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException)
        {
            Problem = new UserMessage(_strings.Get("NoticesReadFailedTitle"), _strings.Format("NoticesReadFailedMessage", e.Message));
            State = NoticesState.Failed;
        }
    }
}
