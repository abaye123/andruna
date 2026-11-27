using Andruna.Manager.Core.Events;
using Microsoft.Windows.AppNotifications;
using Microsoft.Windows.AppNotifications.Builder;

namespace Andruna.Manager.Notifications;

/// <summary>
/// Android notifications as Windows app notifications (D-14), through the
/// Windows App SDK. The manager registers as the notification source
/// "Andruna"; the app's label is the attribution line. A click runs the app
/// through andruna-launch (<see cref="Invoked"/>); when the manager is not
/// running, Windows starts it with the click as its activation.
/// </summary>
internal sealed class ToastPresenter : IToastPresenter
{
    private readonly bool _supported;
    private readonly string _language;

    public ToastPresenter(string language)
    {
        _language = language;
        _supported = AppNotificationManager.IsSupported();
        if (!_supported)
        {
            return;
        }

        var manager = AppNotificationManager.Default;
        manager.NotificationInvoked += (_, args) => Invoked?.Invoke(this, NotificationBridge.PackageToOpen(args.Argument));
        try
        {
            manager.Register("Andruna", new Uri(Path.Combine(AppContext.BaseDirectory, "Assets", "Andruna.ico")));
        }
        catch (Exception e) when (e is System.Runtime.InteropServices.COMException or ArgumentException)
        {
            _supported = false;
        }
    }

    /// <summary>A notification was clicked; the package to open, or null.</summary>
    public event EventHandler<string?>? Invoked;

    /// <summary>Removes the registration (the uninstaller runs <c>--unregister</c>).</summary>
    public static void Unregister()
    {
        if (AppNotificationManager.IsSupported())
        {
            try
            {
                AppNotificationManager.Default.UnregisterAll();
            }
            catch (System.Runtime.InteropServices.COMException)
            {
            }
        }
    }

    public void Show(ToastRequest toast)
    {
        if (!_supported)
        {
            return;
        }

        var builder = new AppNotificationBuilder()
            .AddText(toast.Title, new AppNotificationTextProperties().SetMaxLines(2))
            .SetAttributionText(toast.Attribution, _language)
            .SetTag(toast.Tag)
            .SetGroup(toast.Group);
        if (!string.IsNullOrWhiteSpace(toast.Text))
        {
            builder.AddText(toast.Text);
        }

        foreach (var part in toast.Arguments.Split(';', StringSplitOptions.RemoveEmptyEntries).Select(p => p.Split('=', 2)).Where(p => p.Length == 2))
        {
            builder.AddArgument(part[0], part[1]);
        }

        AppNotificationManager.Default.Show(builder.BuildNotification());
    }

    public void Remove(string tag, string group)
    {
        if (_supported)
        {
            _ = AppNotificationManager.Default.RemoveByTagAndGroupAsync(tag, group);
        }
    }
}
