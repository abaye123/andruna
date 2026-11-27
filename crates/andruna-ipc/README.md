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
| (any) | | | Error (0x80FF): `u16` code, `str` message |

Error codes: 1 UnsupportedVersion, 2 Malformed, 3 UnknownRequest,
4 InvalidPackageName, 5 NotInstalled, 6 NotRunning, 7 NoWindow,
8 HostStartFailed, 9 Busy, 10 Internal. Clients treat unknown codes as
failures.

Launcher and sysd speak this pipe protocol; app processes and the manager
will use Binder once it exists, as the architecture says.
