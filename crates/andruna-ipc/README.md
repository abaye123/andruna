# andruna-ipc

The named-pipe protocol between clients (`andruna-launch`, later the
manager) and `andruna-sysd`. Architecture: [02-architecture.md, IPC](../../docs/spec/02-architecture.md#ipc).

## Endpoint

| Object | Name |
| --- | --- |
| Pipe | `\\.\pipe\Andruna.sysd.<user SID>.<session id>.<instance>` |
| Single-instance mutex | `Local\Andruna.sysd.<user SID>.<instance>` |

The instance is `default` in the product. Tests and development builds use
other instance names (and `--root` data directories) so they never touch a
running product sysd.

Security:

- The pipe's DACL is protected and grants access to the current user only
  (`O:<user>D:P(A;;GA;;;<user>)`). `PipeSecurity::allow_app_container` is
  the documented hook for AppContainer package SIDs; it also lowers the
  mandatory label so low-integrity container processes can connect. It is
  not used yet.
- The server creates its first instance with `FILE_FLAG_FIRST_PIPE_INSTANCE`
  and rejects remote clients.
- The client opens the pipe with `SECURITY_IDENTIFICATION` (the server
  cannot impersonate it) and checks that the server process runs as the same
  user before sending anything.

## Frames

```
u32 LE  payload length, 8 ..= 1 MiB
u16 LE  protocol version (1)
u16 LE  kind
u32 LE  request id (echoed in the response)
...     body
```

Integers are little-endian, booleans one byte (0 or 1), strings a `u16`
byte length and UTF-8. Trailing bytes are an error. A request in another
version gets `Error(UnsupportedVersion)` in version 1, then the connection
closes; a malformed frame gets `Error(Malformed)` and the connection
closes; an unknown kind gets `Error(UnknownRequest)` and the connection
stays open.

| Request | Kind | Body | Response |
| --- | --- | --- | --- |
| Ping | 0x0001 | - | Pong (0x8001): `u16` protocol version |
| Status | 0x0002 | - | Status (0x8002): `u32` pid, `u64` uptime ms, `u32` connections, `u32` hosts, `u64` idle timeout ms, `str` version |
| StartPackage | 0x0003 | `str` package | Started (0x8003): `u32` host pid, `bool` already running |
| FocusWindow | 0x0004 | `str` package | Focused (0x8004) |
| ListPackages | 0x0005 | - | Packages (0x8005): `u32` count, `str` each, sorted |
| InspectPackage | 0x0006 | `strs` APK paths, `str` locale | PackageDetails (0x8006), below |
| InstallPackage | 0x0007 | `strs` APK paths | Installed (0x8007), below |
| UninstallPackage | 0x0008 | `str` package, `bool` keep data | Uninstalled (0x8008) |
| ListPackageInfo | 0x0009 | `str` locale | PackageInfos (0x8009): `u32` count, PackageInfo each, sorted by package |
| StartActivity | 0x000A | `str` package, `str` activity | Started (0x8003), as StartPackage |
| WaitEvents | 0x0010 | `u64` epoch, `u64` after, `u32` timeout ms | Events (0x8010): `u64` epoch, `u64` next, `bool` missed, `u32` count, events |
| GetMissingFunctions | 0x0011 | `str` package | MissingFunctions (0x8011): `u64` updated ms, `u32` count, (`str` name, `u64` calls) each, most-called first |
| ReportMissingFunctions | 0x0012 | `u32` count, (`str` name, `u64` calls) each | Reported (0x8012) |
| (any) | | | Error (0x80FF): `u16` code, `str` message |

`strs` is a `u32` count followed by that many strings. A locale is a tag
such as `he-IL` or `en`; empty means sysd's UI language.

- PackageDetails: `str` package, `u64` version code, `str` version name,
  `str` label, `u32` min SDK, `u32` target SDK, `str` signer SHA-256 (hex),
  `u8` signature scheme (1, 2, 3, 31), `strs` permissions, `strs` native
  ABIs, `str` selected ABI, `u64` installed version code (0 if none), `str`
  icon PNG path.
- Installed: `str` package, `u64` version code, `str` version name, `bool`
  updated, `u8` compile status (0 compiled, 1 no code, 2 compiler
  unavailable, 3 compile failed; the package is installed in all cases),
  `str` compile message, `u32` shortcuts created.
- PackageInfo: `str` package, `str` label, `u64` version code, `str`
  version name, `str` icon `.ico` path, `str` icon PNG path, `strs`
  launcher activities, `u64` install time (ms since the Unix epoch), `u64`
  size in bytes, `str` AppUserModelID.

Error codes: 1 UnsupportedVersion, 2 Malformed, 3 UnknownRequest,
4 InvalidPackageName, 5 NotInstalled, 6 NotRunning, 7 NoWindow,
8 HostStartFailed, 9 Busy, 10 Internal, 11 InvalidPackage,
12 InvalidSignature, 13 SignerMismatch, 14 IncompatibleAbi, 15 SdkTooNew,
16 Downgrade, 17 InvalidSplits, 18 InstallFailed, 32 NotPermitted. Clients
treat unknown codes as failures.

The package manager messages (0x0006 to 0x000A) have their golden payloads
in `tests/golden/v1-pkg.txt`. Install and inspect run synchronously on the
connection's thread and can take seconds (dex2oat); clients use a generous
timeout.

### Number ranges

New kinds are added without a version change: an older server answers
them with `UnknownRequest`, which clients show as "not available yet".
To keep parallel work from colliding:

| Range | Owner |
| --- | --- |
| 0x0001..=0x000F | sysd basics and the package manager (install, uninstall, package details) |
| 0x0010..=0x001F | events, crash reports, diagnostics (manager and launcher) |

Responses use the same number with the high bit set. Error codes 11..=31
belong to the package manager, 32..=47 to the 0x0010 range. StartActivity
(0x000A) is defined with the package manager messages; sysd implements it
with the launcher.

### Events (0x0010)

A long poll. The client sends the `epoch` and `next` of its last answer
(first call: epoch 0, after `u64::MAX`, which returns the current position
at once). sysd answers as soon as an event newer than `after` exists, or
with none after the timeout (at most 30 s). sysd keeps the last 256 events;
`missed` says some were dropped, or that the epoch (a new sysd instance)
changed. A connection that sends WaitEvents stops keeping sysd alive, so a
background notifier does not hold sysd running.

Each event: `u64` seq, `u64` time (ms since the Unix epoch), `u16` type,
`u32` body length, body. Unknown types are skipped.

| Type | Event | Body |
| --- | --- | --- |
| 1 | NotificationPosted | `str` package, `i32` id, `str` tag, `str` channel, `str` title, `str` text |
| 2 | NotificationRemoved | `str` package, `i32` id, `str` tag |
| 3 | AppCrashed | `str` package, `u32` pid, `u32` exit code, `str` reason |

sysd publishes AppCrashed when a host process ends on its own with a
non-zero exit code. Nothing posts notifications yet: the notification
service arrives with the framework, and publishes through
`andruna_sysd::events::Events::publish`.

### Missing functions (0x0011, 0x0012)

Names are `<area>:<entry point>`, for example `lx:epoll_pwait2` or
`service:package.getInstalledModules`. Only a host process sysd started
may report (sysd takes the package from the caller's process id, never from
the message; anyone else gets NotPermitted). sysd adds reports to
`diagnostics\<package>.missing`.

`tests/golden/v1-events.txt` (checked by `tests/golden_events.rs`) holds
the golden payloads of this range.

Launcher and sysd speak this pipe protocol; app processes and the manager
will use Binder once it exists, as the architecture says.

## Host channel

`host.rs` is a separate protocol on a separate channel: sysd and one app
host process (`andruna-app --host`), over two inherited anonymous pipes
whose handle values are in `ANDRUNA_HOST_CHANNEL` (ADRs 0036, 0037). Frames
have the layout above with their own version (`HOST_PROTOCOL_VERSION`, 1)
and an id of 0.

| Message | Kind | Direction | Body |
| --- | --- | --- | --- |
| Attach | 0x0101 | sysd to host | package, data dir, ART root, entry (`u8` 1: class path list, class, args; 2: `bool` + activity), mode, runtime options, `bool` + boot image (`u64` base, `u64` size, `u16` count, extents: path, `u64` offset, addr, len) |
| Stop | 0x0102 | sysd to host | - |
| Hello | 0x0181 | host to sysd | `u32` pid, version |
| Running | 0x0182 | host to sysd | - |
| Refused | 0x0183 | host to sysd | message |
| MissingFunctions | 0x0184 | host to sysd | `u16` count, (name, `u64` calls) |
| Exit | 0x0185 | host to sysd | `u32` status, `u8` crash kind (0 none, 1 uncaught exception, 2 native fault, 3 abort, 4 other) and summary when not 0 |

Lists are a `u16` count and their items.
