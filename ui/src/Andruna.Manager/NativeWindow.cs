using System.Runtime.InteropServices;
using Microsoft.UI.Xaml;
using WinRT.Interop;

namespace Andruna.Manager;

/// <summary>The few Win32 calls the window needs that WinUI does not expose.</summary>
internal static class NativeWindow
{
    private const int GwlExStyle = -20;
    private const long WsExLayoutRtl = 0x00400000;

    public static nint Handle(Window window) => WindowNative.GetWindowHandle(window);

    /// <summary>Scale factor of the monitor the window is on (1.0 at 96 DPI).</summary>
    public static double GetScale(Window window)
    {
        var dpi = GetDpiForWindow(Handle(window));
        return dpi == 0 ? 1.0 : dpi / 96.0;
    }

    /// <summary>
    /// Mirrors the window frame for a right-to-left language, so the caption
    /// buttons are on the left as in Hebrew Windows.
    /// </summary>
    public static void SetRightToLeftLayout(Window window)
    {
        var hwnd = Handle(window);
        var style = GetWindowLongPtrW(hwnd, GwlExStyle);
        SetWindowLongPtrW(hwnd, GwlExStyle, (nint)((long)style | WsExLayoutRtl));
    }

    /// <summary>Brings the window to the foreground (allowed when the activation came from the user).</summary>
    public static void SetForeground(Window window) => SetForegroundWindow(Handle(window));

    [DllImport("user32.dll", ExactSpelling = true)]
    private static extern bool SetForegroundWindow(nint hwnd);

    [DllImport("user32.dll", ExactSpelling = true)]
    private static extern uint GetDpiForWindow(nint hwnd);

    [DllImport("user32.dll", ExactSpelling = true)]
    private static extern nint GetWindowLongPtrW(nint hwnd, int index);

    [DllImport("user32.dll", ExactSpelling = true)]
    private static extern nint SetWindowLongPtrW(nint hwnd, int index, nint value);
}
