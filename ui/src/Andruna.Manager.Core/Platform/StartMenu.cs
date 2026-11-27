using System.Runtime.InteropServices;
using System.Runtime.InteropServices.ComTypes;

namespace Andruna.Manager.Core.Platform;

/// <summary>A Start menu shortcut of an Android app.</summary>
public sealed record AppShortcut(string Label, string Package, string? Activity, string AppUserModelId, string? IconPath);

/// <summary>
/// The Start menu shortcuts of Android apps, by the convention in
/// crates/andruna-launch/README.md: <c>Programs\&lt;label&gt;.lnk</c> in the
/// user's Start menu, running <c>andruna-launch.exe &lt;package&gt;[/&lt;activity&gt;]</c>
/// with the package's AppUserModelID. Windows offers no way for a program
/// to pin a desktop app to Start; the user pins the shortcut from All apps.
/// </summary>
public sealed class StartMenu(string programsDirectory, string launcherPath)
{
    /// <summary>The current user's Start menu programs folder and the installed launcher.</summary>
    public static StartMenu ForCurrentUser(string installDirectory) => new(
        Environment.GetFolderPath(Environment.SpecialFolder.Programs),
        Path.Combine(installDirectory, LaunchExecutable.ExecutableName));

    public string ProgramsDirectory { get; } = programsDirectory;

    /// <summary>The shortcut file for a label: invalid file name characters become spaces.</summary>
    public string ShortcutPath(string label)
    {
        var invalid = Path.GetInvalidFileNameChars();
        var name = new string(label.Select(c => invalid.Contains(c) ? ' ' : c).ToArray()).Trim().TrimEnd('.');
        return Path.Combine(ProgramsDirectory, (name.Length == 0 ? "App" : name) + ".lnk");
    }

    public bool Exists(AppShortcut shortcut) => File.Exists(ShortcutPath(shortcut.Label));

    /// <summary>Creates the shortcut, or rewrites it when it exists. Returns its path.</summary>
    public string Create(AppShortcut shortcut)
    {
        Directory.CreateDirectory(ProgramsDirectory);
        var path = ShortcutPath(shortcut.Label);
        var link = (IShellLinkW)new ShellLink();
        try
        {
            link.SetPath(launcherPath);
            link.SetArguments(LaunchExecutable.Target(shortcut.Package, shortcut.Activity));
            link.SetWorkingDirectory(Path.GetDirectoryName(launcherPath) ?? string.Empty);
            link.SetDescription(shortcut.Label);
            if (!string.IsNullOrEmpty(shortcut.IconPath) && File.Exists(shortcut.IconPath))
            {
                link.SetIconLocation(shortcut.IconPath, 0);
            }

            var store = (IPropertyStore)link;
            var key = AppUserModelIdKey;
            var value = PropVariant.FromString(shortcut.AppUserModelId);
            try
            {
                Marshal.ThrowExceptionForHR(store.SetValue(ref key, ref value));
                Marshal.ThrowExceptionForHR(store.Commit());
            }
            finally
            {
                value.Clear();
            }

            ((IPersistFile)link).Save(path, true);
        }
        finally
        {
            Marshal.ReleaseComObject(link);
        }

        return path;
    }

    /// <summary>Reads the target arguments and AppUserModelID of a shortcut (for tests and checks).</summary>
    public static (string Arguments, string? AppUserModelId) Read(string path)
    {
        var link = (IShellLinkW)new ShellLink();
        try
        {
            ((IPersistFile)link).Load(path, 0);
            var buffer = new char[1024];
            link.GetArguments(buffer, buffer.Length);
            var args = new string(buffer, 0, Array.IndexOf(buffer, '\0') is var n and >= 0 ? n : buffer.Length);
            var key = AppUserModelIdKey;
            Marshal.ThrowExceptionForHR(((IPropertyStore)link).GetValue(ref key, out var value));
            try
            {
                return (args, value.AsString());
            }
            finally
            {
                value.Clear();
            }
        }
        finally
        {
            Marshal.ReleaseComObject(link);
        }
    }

    private static PropertyKey AppUserModelIdKey => new(new Guid("9F4C2855-9F79-4B39-A8D0-E1D42DE1D5F3"), 5);

    [ComImport]
    [Guid("00021401-0000-0000-C000-000000000046")]
    private class ShellLink
    {
    }

    [ComImport]
    [InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    [Guid("000214F9-0000-0000-C000-000000000046")]
    private interface IShellLinkW
    {
        void GetPath([Out, MarshalAs(UnmanagedType.LPArray, SizeParamIndex = 1)] char[] file, int max, IntPtr findData, uint flags);

        void GetIDList(out IntPtr idList);

        void SetIDList(IntPtr idList);

        void GetDescription([Out, MarshalAs(UnmanagedType.LPArray, SizeParamIndex = 1)] char[] name, int max);

        void SetDescription([MarshalAs(UnmanagedType.LPWStr)] string name);

        void GetWorkingDirectory([Out, MarshalAs(UnmanagedType.LPArray, SizeParamIndex = 1)] char[] dir, int max);

        void SetWorkingDirectory([MarshalAs(UnmanagedType.LPWStr)] string dir);

        void GetArguments([Out, MarshalAs(UnmanagedType.LPArray, SizeParamIndex = 1)] char[] args, int max);

        void SetArguments([MarshalAs(UnmanagedType.LPWStr)] string args);

        void GetHotkey(out short hotkey);

        void SetHotkey(short hotkey);

        void GetShowCmd(out int showCmd);

        void SetShowCmd(int showCmd);

        void GetIconLocation([Out, MarshalAs(UnmanagedType.LPArray, SizeParamIndex = 1)] char[] iconPath, int max, out int icon);

        void SetIconLocation([MarshalAs(UnmanagedType.LPWStr)] string iconPath, int icon);

        void SetRelativePath([MarshalAs(UnmanagedType.LPWStr)] string path, uint reserved);

        void Resolve(IntPtr hwnd, uint flags);

        void SetPath([MarshalAs(UnmanagedType.LPWStr)] string file);
    }

    [ComImport]
    [InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    [Guid("886D8EEB-8CF2-4446-8D02-CDBA1DBDCF99")]
    private interface IPropertyStore
    {
        [PreserveSig]
        int GetCount(out uint count);

        [PreserveSig]
        int GetAt(uint index, out PropertyKey key);

        [PreserveSig]
        int GetValue(ref PropertyKey key, out PropVariant value);

        [PreserveSig]
        int SetValue(ref PropertyKey key, ref PropVariant value);

        [PreserveSig]
        int Commit();
    }

    [StructLayout(LayoutKind.Sequential, Pack = 4)]
    private readonly struct PropertyKey(Guid formatId, uint propertyId)
    {
        public readonly Guid FormatId = formatId;
        public readonly uint PropertyId = propertyId;
    }

    /// <summary>A PROPVARIANT holding VT_LPWSTR or nothing.</summary>
    [StructLayout(LayoutKind.Sequential)]
    private struct PropVariant
    {
        private const ushort VtLpwstr = 31;

        private ushort _vt;
        private ushort _reserved1;
        private ushort _reserved2;
        private ushort _reserved3;
        private IntPtr _pointer;
        private IntPtr _padding;

        public static PropVariant FromString(string value) => new() { _vt = VtLpwstr, _pointer = Marshal.StringToCoTaskMemUni(value) };

        public readonly string? AsString() => _vt == VtLpwstr ? Marshal.PtrToStringUni(_pointer) : null;

        public void Clear()
        {
            PropVariantClear(ref this);
        }

        [DllImport("ole32.dll")]
        private static extern int PropVariantClear(ref PropVariant variant);
    }
}
