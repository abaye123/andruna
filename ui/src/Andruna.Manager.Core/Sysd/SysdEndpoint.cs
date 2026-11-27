using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Security.Principal;
using Microsoft.Win32.SafeHandles;

namespace Andruna.Manager.Core.Sysd;

/// <summary>
/// The per-user, per-session sysd pipe:
/// <c>\\.\pipe\Andruna.sysd.&lt;user SID&gt;.&lt;session id&gt;.&lt;instance&gt;</c>
/// (crates/andruna-ipc/README.md, "Endpoint").
/// </summary>
public static class SysdEndpoint
{
    /// <summary>Instance used by the shipped product.</summary>
    public const string DefaultInstance = "default";

    /// <summary>The pipe name as .NET takes it (without <c>\\.\pipe\</c>).</summary>
    public static string PipeName(string userSid, uint sessionId, string instance = DefaultInstance) =>
        $"Andruna.sysd.{userSid}.{sessionId}.{instance}";

    /// <summary>The pipe name of the current user and Windows session.</summary>
    public static string PipeNameForCurrentUser(string instance = DefaultInstance) =>
        PipeName(CurrentUserSid().Value, CurrentSessionId(), instance);

    public static SecurityIdentifier CurrentUserSid()
    {
        using var identity = WindowsIdentity.GetCurrent();
        return identity.User ?? throw new InvalidOperationException("the current token has no user SID");
    }

    /// <summary>
    /// The owner the current token gives new objects: the user, or
    /// BUILTIN\Administrators in an elevated process.
    /// </summary>
    public static SecurityIdentifier? CurrentDefaultOwner()
    {
        using var identity = WindowsIdentity.GetCurrent();
        return identity.Owner;
    }

    public static uint CurrentSessionId()
    {
        if (!Native.ProcessIdToSessionId(Native.GetCurrentProcessId(), out var session))
        {
            throw new Win32Exception(Marshal.GetLastWin32Error());
        }

        return session;
    }

    /// <summary>Process id of the server end of a connected pipe.</summary>
    public static uint ServerProcessId(SafePipeHandle pipe)
    {
        if (!Native.GetNamedPipeServerProcessId(pipe, out var pid))
        {
            throw new Win32Exception(Marshal.GetLastWin32Error());
        }

        return pid;
    }

    /// <summary>The user a process runs as, or null when it cannot be read.</summary>
    public static SecurityIdentifier? ProcessUser(uint pid)
    {
        using var process = Native.OpenProcess(Native.ProcessQueryLimitedInformation, false, pid);
        if (process.IsInvalid)
        {
            return null;
        }

        if (!Native.OpenProcessToken(process, Native.TokenQuery, out var token))
        {
            return null;
        }

        using (token)
        {
            Native.GetTokenInformation(token, Native.TokenUser, IntPtr.Zero, 0, out var needed);
            if (needed <= 0)
            {
                return null;
            }

            var buffer = Marshal.AllocHGlobal(needed);
            try
            {
                if (!Native.GetTokenInformation(token, Native.TokenUser, buffer, needed, out _))
                {
                    return null;
                }

                // TOKEN_USER starts with SID_AND_ATTRIBUTES, whose first field is the PSID.
                var sid = Marshal.ReadIntPtr(buffer);
                return new SecurityIdentifier(sid);
            }
            finally
            {
                Marshal.FreeHGlobal(buffer);
            }
        }
    }

    private static class Native
    {
        public const uint ProcessQueryLimitedInformation = 0x1000;
        public const uint TokenQuery = 0x0008;
        public const int TokenUser = 1;

        [DllImport("kernel32.dll")]
        public static extern uint GetCurrentProcessId();

        [DllImport("kernel32.dll", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        public static extern bool ProcessIdToSessionId(uint processId, out uint sessionId);

        [DllImport("kernel32.dll", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        public static extern bool GetNamedPipeServerProcessId(SafePipeHandle pipe, out uint serverProcessId);

        [DllImport("kernel32.dll", SetLastError = true)]
        public static extern SafeProcessHandle OpenProcess(uint access, [MarshalAs(UnmanagedType.Bool)] bool inherit, uint processId);

        [DllImport("advapi32.dll", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        public static extern bool OpenProcessToken(SafeProcessHandle process, uint access, out SafeAccessTokenHandle token);

        [DllImport("advapi32.dll", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        public static extern bool GetTokenInformation(SafeAccessTokenHandle token, int infoClass, IntPtr info, int length, out int returnLength);
    }
}
