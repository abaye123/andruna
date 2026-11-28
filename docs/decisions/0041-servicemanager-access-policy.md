# 0041 - servicemanager: access checks on the broker's uid, no SELinux

**Status:** proposed, 2026-11-28

## Context
servicemanager (frameworks/native/cmds/servicemanager) asks its `Access`
class whether a caller may find, add or list services. On Android the
answer comes from SELinux: the caller's security context (from the binder
driver or `getpidcon`), the service's context from `service_contexts`, and
the policy. Andruna has no SELinux, and the binder broker sends no security
context (ADR 0025). The only identity servicemanager can trust is the
calling uid and pid, which the broker takes from what sysd recorded when it
started the process, never from the sender. The rules are visible to every
application (which services it can find or register), so they are an
application-visible default.

## Decision
- servicemanager is built from the upstream sources with `Access.cpp`
  replaced by `aosp/guest/binder/servicemanager_access.cpp`, which
  implements the same class (`Access.h` unchanged) with rules on the
  caller's app id (`uid % 100000`):

  | Caller | find | add | list |
  | --- | --- | --- | --- |
  | app id below `AID_APP_START` (10000): system processes | yes | yes | yes |
  | apps, SDK sandboxes, isolated processes | yes | no | no |

  A denial is logged with the caller's pid and uid and reaches the caller
  as servicemanager's usual `EX_SECURITY`.
- servicemanager's own rules stay on top: isolated callers only get
  services registered with `allowIsolated`, names are validated, and
  services check their callers themselves (calling uid, permissions), as on
  Android.
- `ServiceManager.cpp` and `main.cpp` are compiled with
  `VENDORSERVICEMANAGER`, the upstream switch that leaves out VINTF
  manifest lookups and perfetto tracing. Andruna has no VINTF manifest (it
  has no HALs) and no perfetto producer, so `isDeclared` is false,
  `getDeclaredInstances` and `updatableViaApex` are empty, as for a device
  whose manifests declare nothing. The switch also skips setting
  `servicemanager.ready`; sysd provides that property instead (below).
- `servicemanager.ready`: libbinder waits for this property before its
  first lookup. Properties are per process and fixed at start (ADR 0019),
  so sysd starts servicemanager first, waits until it is the broker's
  context manager, and defines `servicemanager.ready=true` for every guest
  process it starts afterwards (andruna-elf `--prop`).
- libbinder's `AApexSupport_loadLibrary` (libapexsupport) is Andruna's: it
  fails, since no APEX provides HALs.

## Consequences
- Apps can find every registered service by name, including ones SELinux
  restricts to some system daemons on Android; until a service-type policy
  exists, a service that must not be reachable from apps checks the
  calling uid itself. Apps cannot register services or list them, as on
  Android.
- No AOSP patch: servicemanager's sources are unchanged; the policy lives
  in one Andruna file that can grow a service-type table later.
- Processes started before servicemanager is ready, or by something other
  than sysd, wait for the property as on Android (libbinder logs every
  second).
