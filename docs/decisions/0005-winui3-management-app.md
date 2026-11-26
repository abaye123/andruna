# 0005 - WinUI 3 for the management app and system dialogs

**Status:** accepted, 2026-11-26

## Decision
The management app and system dialogs (permissions, install, account
chooser, crash) are a WinUI 3 app in C# on the Windows App SDK, unpackaged,
talking to sysd through Binder via a small C# client.

## Consequences
- The most accurate Windows 11 look.
- C# is a third language; it stays in `ui/` only.
