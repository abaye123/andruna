# andruna-binder

Binder without a kernel driver ([ADR 0025](../../docs/decisions/0025-binder-broker.md)):

- `broker`: the logic of the kernel binder driver, run in sysd for all
  guest processes; independent of the transport and unit tested with
  simulated processes.
- `transport`, `host`, `sys`: the control channel and the shared-memory
  lanes between guest threads and their broker threads, and the server
  sysd runs.
- `pack`: the guest side of the write and read streams.

The `/dev/binder` device of guest processes is
[`andruna-binder-device`](../andruna-binder-device), kept apart so that
sysd does not link andruna-lx. Status and measurements:
[docs/plan/phase2/binder.md](../../docs/plan/phase2/binder.md).

```
cargo test -p andruna-binder
```
