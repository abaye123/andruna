# 0025 - Binder: a broker in sysd behind a /dev/binder device

**Status:** proposed, 2026-11-27

## Context
Android code talks to Binder through `/dev/binder`: `open`, `mmap` of a
receive buffer, and `ioctl` (`BINDER_WRITE_READ` with the `BC_*` / `BR_*`
protocol). libbinder, the Java Binder and the Rust `binder` crate keep
their kernel IPC code (03-components). Windows has no binder driver, and
guest processes are separate Windows processes. Spike S3 measured the
transport: shared-memory rings with a flag-plus-event wake, under 20 us
per round trip, with `SignalObjectAndWait` and bounded spinning.

## Decision
- **Broker in sysd.** The logic of the kernel driver runs in one place,
  the crate `andruna-binder` (module `broker`), hosted by sysd
  (`andruna-sysd::binder`): processes, threads, nodes, references,
  transactions, receive-buffer allocation, death notifications. It follows
  the driver's rules: thread selection (a waiting looper thread, nested
  calls to the thread waiting in the caller's chain), synchronous replies
  with `BR_TRANSACTION_COMPLETE` deferred to the reply, one-way calls
  serialized per node, the owner's `BR_INCREFS` / `BR_ACQUIRE` /
  `BR_RELEASE` / `BR_DECREFS` protocol, `BR_SPAWN_LOOPER`, `BR_DEAD_REPLY`
  and `BR_FAILED_REPLY`, the context manager and handle 0.
- **Identity from sysd.** The calling pid and uid of every transaction are
  the ones sysd recorded for the process (its Windows process id and the
  package uid), never values from the sender. A process sysd did not
  start or allow is refused.
- **Device in the guest.** `/dev/binder` is an andruna-lx device
  registered by the host process (`andruna-binder-device`, used by
  andruna-elf and andruna-app) through `fs::dev::register`, with
  `FileOps::ioctl` and `FileOps::device_mmap`. It implements
  `BINDER_WRITE_READ`, `BINDER_VERSION`, `BINDER_SET_MAX_THREADS`,
  `BINDER_SET_CONTEXT_MGR(_EXT)`, `BINDER_THREAD_EXIT`,
  `BINDER_ENABLE_ONEWAY_SPAM_DETECTION` (accepted), `BINDER_GET_EXTENDED_ERROR`
  (no error), `mmap` and `poll`; other requests give `EINVAL`.
- **Transport (S3).** A control channel per process (a local message-mode
  pipe, or a private channel inherited as a handle for AppContainer
  processes, ADR 0036) for set-up and rare requests. Each guest thread that
  uses binder gets a *lane*: a section shared with one broker thread in
  sysd (its "kernel half"), with a sequence number, a sleeping flag and an
  event per direction. The waiting side spins up to 30 us while the lane is
  busy (a message in the last 2 ms) on machines with at least four logical
  processors, then sleeps; without spinning, waking the peer and sleeping
  is one `SignalObjectAndWait`. Broker threads waiting for work spin the
  same way before their condition variable.
- **Copies.** The device copies the write stream, and the data and offsets
  of each transaction, into the lane (the kernel's `copy_from_user`). The
  broker copies the data once from the lane into the receiver's buffer, a
  section mapped by sysd and by the receiver. Offsets and objects are read
  once into broker memory, validated and translated there, and written
  into the receiver's buffer; the broker never reads a receive buffer back
  and records the references each buffer holds.
- **File descriptors.** The device passes the Windows handle of a regular
  file; the broker duplicates it into the receiver (`DuplicateHandle`), and
  the receiver's device installs it as a descriptor and writes the number
  into the received object.

## Consequences
- Two copies per transaction instead of the kernel's one (lane, then
  receiver buffer). Measured cost is small next to the wakes.
- The receive buffer is writable in the guest (the device writes received
  descriptors into it); a process can corrupt only what it received.
- Round trip between two guest processes through bionic and andruna-elf
  (`binder_raw`): p50 about 7 us with spinning, about 34 us without, on a
  loaded laptop (docs/plan/phase2/binder.md). The target (20 us) holds with
  spinning; without it, it does not on this machine.
- Not supported yet: scatter-gather buffers (`BINDER_TYPE_PTR`, `FDA`;
  libbinder does not send them, HIDL does), passing descriptors that are
  not regular files (pipes, sockets, memfd), interrupting a blocked read
  with a signal, freezing, priority inheritance, security contexts
  (`BR_TRANSACTION_SEC_CTX` carries none).
- sysd is a single point: when it dies, guest binder calls fail with `EIO`.
