# Spikes

Small, throwaway prototypes that answer the questions most likely to change
the architecture. Each produces a written result (`docs/plan/spike-results/
S<n>.md`) with numbers, and an ADR if it changes a decision. Spike code is not
production code.

## S1: ART on a Rust Linux layer

**Question.** Can ART (with libcore) run on `andruna-lx` written in Rust, on
x64 and ARM64 Windows, with the boundary rules in AGENTS.md?

**Do.** Build ART, libartbase, libdexfile, libcore natives with llvm-mingw;
provide the needed exports from a minimal `andruna-lx`; run Hello World and a
small benchmark with interpreter, JIT and AOT.

**Decide.**
- Thread suspension: signal-style injection vs cooperative checkpoints only.
- Implicit null checks and stack overflow checks via Vectored Exception
  Handlers on both architectures.
- The managed calling convention on x64 (keep ART's System V) and how JNI
  natives built as Windows code are entered.

**Success.** Hello World and the benchmark run under all three modes on both
architectures; a list of every lx export used.

## S2: Shared boot image and garbage collection

**Question.** Can the boot image be mapped from one section into several
processes at the same address, and which garbage collector works well without
`userfaultfd`?

**Do.** Two app processes mapping the same boot image; measure private and
shared working set, start time. Run ART's GC options available without
`userfaultfd`; prototype the alternative on Windows memory primitives if
needed.

**Success.** Numbers for memory and start time per process; a GC decision
with pause-time and throughput measurements.

## S3: Binder over shared memory

**Question.** Does a shared-memory Binder reach a round trip under 20 µs on
Windows, with correct caller identity?

**Do.** A broker plus two processes; libbinder unchanged on top; measure small
and 64 KiB transactions, oneway calls, death notification, fd passing; caller
identity from the AppContainer token.

**Success.** Latency numbers vs the target; libbinder's own tests that apply.

## S4: bionic and thread-local storage

**Question.** Can AOSP's real bionic serve guest code, with its system calls
routed to `andruna-lx`? How is TLS provided (x64 `%fs`, ARM64 `TPIDR_EL0`)?
Is x18 safe on Windows ARM64 with NDK code?

**Do.** Load an NDK library with real bionic, call stdio, locale, malloc,
pthread, C++ exceptions. Try the TLS options (a thread register that Windows
leaves to user mode, instruction patching, compiler flags for our own code).
Survey x18 use in real NDK binaries.

**Success.** A decision between real bionic and a shim, with the TLS
mechanism and its cost.

## S5: arm64 translation on x64

**Question.** Does ahead-of-time translation to LLVM IR give near-native
speed for real arm64 libraries, at acceptable background cost?

**Do.** Translate two real libraries (a game engine, a media library);
measure translation time, memory, code size and run speed vs dynarmic alone.

**Success.** Numbers and a go/no-go for AOT translation in phase 5.

## S6: WinUI 3 manager talking to sysd

**Question.** Can an unpackaged WinUI 3 app use Binder (through a small C#
client) to sysd, and host system dialogs that open quickly enough?

**Success.** A dialog appears within 150 ms of the request; the client
library's shape is agreed.

## S7: AppContainer, GPU and warm processes

**Question.** Do ART, ANGLE on D3D11 and DirectComposition work inside an
AppContainer, and which warm-process strategy keeps start time on target?

**Success.** First frame drawn inside a container; start times for both
strategies in [security](../spec/04-security.md#warm-processes-and-appcontainer).
