# 0033 - The manager bridges notifications and crashes, in the background

**Status:** proposed, 2026-11-27

## Context
Android notifications must become Windows notifications (D-14) and a crashed
app must show a Windows dialog (P-23), also when the manager window is
closed. Both come from sysd, which has no window and must stay a headless
service. Something in the user's session has to receive the events, show
the notifications through the Windows App SDK (the documented API for
unpackaged apps) and handle a click by opening the app. Options:

| Option | Cost |
| --- | --- |
| sysd shows toasts itself (Win32 toast COM APIs) | A notification UI stack and COM activation inside the system server, which should stay small and headless |
| A separate small notifier executable | A second self-contained .NET or Windows App SDK app in the installer (tens of MB) or a second UI stack in Rust |
| The manager, with a windowless background mode | One process to run while apps run; it already has the strings, the theme and the Windows App SDK |

The pipe protocol had only request and response. Events need a push path
that does not change the framing.

## Decision
- The manager runs once per user and session (Windows App SDK
  `AppInstance` key `Andruna.Manager`). A second start hands its activation
  to the running instance. `--background` starts it without a window.
- andruna-launch starts `Andruna.Manager.exe --background` after it started
  an app, unless the mutex `Local\Andruna.Manager.<user SID>` shows that the
  manager runs.
- sysd publishes events (notification posted or removed, app crashed) to a
  ring of the last 256. Clients long-poll them with `WaitEvents` (0x0010) on
  a connection of their own. Such a connection is passive: it does not keep
  sysd running, so the background manager never holds sysd alive.
- The manager shows notifications as Windows app notifications with the
  app's label as attribution; a click runs `andruna-launch <package>`. A
  crash opens the crash dialog (open again, report a problem, close).
- Without a window, the manager exits when sysd is gone and no crash dialog
  is open. Closing the window drops its active connection.
- The uninstaller runs `Andruna.Manager.exe --unregister` to remove the
  notification registration.

## Consequences
- Notifications show "Andruna" as the source, with the app as attribution;
  per-app sources, channels in Windows settings, actions and inline reply
  (rest of D-14) need per-package registration later.
- The manager process runs while apps run: measured once in the background
  with no window, a Debug x64 build used 104 MB working set, 50 MB private
  (Release not measured yet). A smaller notifier remains possible later
  without protocol changes.
- No producer of notification events exists yet: the notification service
  arrives with the framework and calls `Events::publish`. Crash events are
  real: a host that ends on its own with a non-zero code.
- The 256-event ring is lost when sysd exits; events are not persisted.
