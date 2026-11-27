# andruna-binder-device

`/dev/binder` in guest processes: an andruna-lx device (registered by
andruna-elf and andruna-app) that sends the binder ioctls to the broker in
sysd ([ADR 0025](../../docs/decisions/0025-binder-broker.md)). The broker is
found through `ANDRUNA_BINDER_HANDLE` (an inherited private channel) or
`ANDRUNA_BINDER_PIPE` (the broker's named pipe).

```
cargo test -p andruna-binder-device                 # two-process transport test
python aosp/tools/build_guest.py --arch x64 binder-tests
cargo build -p andruna-linker --release
set ANDRUNA_REQUIRE_GUEST=1
cargo test -p andruna-binder-device --release --test guest
```

`ANDRUNA_BINDER_SPIN_US` sets the spin budget of a waiting side (0
disables spinning); `BINDER_ITERS` the number of timed calls of the tests.
