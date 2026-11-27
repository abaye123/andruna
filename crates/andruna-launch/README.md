# andruna-launch

The tiny executable behind Start menu shortcuts:

```
andruna-launch.exe [--quiet] [--instance <name>] [--root <dir>] <package>
```

1. Connects to the user's andruna-sysd pipe.
2. If no sysd runs, starts `andruna-sysd.exe` from its own directory
   (outside the caller's job where allowed), passing an inheritable event,
   and waits up to 10 s for sysd to signal it or exit. A sysd that exits
   with "already running" lost a race to another launcher; the launcher
   then connects to the winner.
3. Passes its foreground right to sysd and sends `StartPackage`. If the
   connection closes before the answer (sysd was just exiting on idle), it
   tries once more.

It is a windows-subsystem program (no console). Errors are shown in a task
dialog in the user's Windows display language; the strings are in
`res/strings.<lang>.txt` (English, Hebrew). `--quiet` writes the English
message to stderr without a dialog. Exit codes: 0 started or focused,
1 bad command line, 2 sysd unavailable, 3 request refused (for example,
package not installed).

`res/andruna-launch.rc` embeds the manifest (Common Controls 6 for the task
dialog, per-monitor DPI, UTF-8) and version information; `build.rs`
compiles it with llvm-mingw's windres.
