using System.Runtime.InteropServices;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Microsoft.Windows.AppLifecycle;

namespace Andruna.Manager;

/// <summary>
/// Entry point. The manager runs once per user and session: a second start
/// (Start menu, the .apk file association, andruna-launch starting the
/// notification bridge, a notification click) hands its activation to the
/// running instance and exits.
/// </summary>
public static class Program
{
    /// <summary>The single-instance key; also named in the mutex andruna-launch checks.</summary>
    public const string InstanceKey = "Andruna.Manager";

    [STAThread]
    private static int Main()
    {
        WinRT.ComWrappersSupport.InitializeComWrappers();

        // The uninstaller removes the Windows notification registration; this
        // must not reach a running instance.
        if (Environment.GetCommandLineArgs().Contains("--unregister"))
        {
            Notifications.ToastPresenter.Unregister();
            return 0;
        }

        var current = AppInstance.GetCurrent();
        var activation = current.GetActivatedEventArgs();
        var main = AppInstance.FindOrRegisterForKey(InstanceKey);
        if (!main.IsCurrent)
        {
            RedirectTo(main, activation);
            return 0;
        }

        Application.Start(p =>
        {
            var context = new DispatcherQueueSynchronizationContext(DispatcherQueue.GetForCurrentThread());
            SynchronizationContext.SetSynchronizationContext(context);
            _ = p;
            _ = new App(activation);
        });
        return 0;
    }

    /// <summary>
    /// Passes the activation to the running instance and lets it take the
    /// foreground. The redirect is awaited off this STA thread while COM
    /// calls are still pumped.
    /// </summary>
    private static void RedirectTo(AppInstance main, AppActivationArguments activation)
    {
        AllowSetForegroundWindow(main.ProcessId);
        var done = CreateEventW(IntPtr.Zero, true, false, null);
        _ = Task.Run(() =>
        {
            try
            {
                main.RedirectActivationToAsync(activation).AsTask().Wait();
            }
            finally
            {
                SetEvent(done);
            }
        });
        _ = CoWaitForMultipleObjects(0, 0xFFFFFFFF, 1, [done], out _);
        CloseHandle(done);
    }

    [DllImport("user32.dll")]
    private static extern bool AllowSetForegroundWindow(uint processId);

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode)]
    private static extern IntPtr CreateEventW(IntPtr attributes, bool manualReset, bool initialState, string? name);

    [DllImport("kernel32.dll")]
    private static extern bool SetEvent(IntPtr handle);

    [DllImport("kernel32.dll")]
    private static extern bool CloseHandle(IntPtr handle);

    [DllImport("ole32.dll")]
    private static extern uint CoWaitForMultipleObjects(uint flags, uint timeout, uint count, IntPtr[] handles, out uint index);
}
