# 0035 - App process start: created suspended in the package's container

**Status:** proposed, 2026-11-27

## Context
A process cannot change its AppContainer after it starts (ADR 0007), so a
generic warm pool is not possible. [Security](../spec/04-security.md#warm-processes-and-appcontainer)
lists two options: (1) keep one host process warm in the container of the
package most likely to open next, or (2) create the process suspended in
the right container at launch and map the boot image section into it.
Spike S7 ([results](../plan/spike-results/S7.md)) measured both with
andruna-app running Hello World on ART (x64, boot image, JIT, 15
interleaved runs, medians):

| Start | Spawn to `Hello` |
| --- | ---: |
| Cold, no container | 140.2 ms |
| Cold, in the package's container | 142.0 ms |
| Suspended in the container, boot image mapped by the parent, resumed (option 2) | 141.3 ms |
| Warm host in the container, assignment to `Hello` (option 1) | 0.5 ms (after 160 ms of warm-up) |

The container itself costs nothing measurable; mapping the 36 MiB boot
image set into the suspended process takes 0.03 ms. An idle warm host
holds 106 MiB of working set and 1.09 GiB of private commit.

## Decision
- sysd starts every app process with option 2: `CreateProcess` suspended
  inside the package's AppContainer and job, sysd maps the read-only boot
  image section (S2), then resumes it. This is the only path in the first
  preview.
- Option 1 is not built now. It is reconsidered when the framework runs
  (phase 2) and the start time of a real activity is measured: if
  framework initialization makes the cold start exceed the start-time
  target, sysd may keep at most one warm host, for the most recently used
  package, only on mains power and with enough free memory, and drops it
  under memory pressure.

## Consequences
- No process ever runs outside a container waiting for an assignment.
- Start time is process creation plus runtime init; boot image loading is
  shared through the section, not repeated.
- andruna-app must accept a boot image already mapped by sysd (lx records
  it as a mapping it does not own); until then it maps the image itself.
- A warm host would cost about 1 GiB of commit per process with today's
  ART heap reservations; reducing that is a prerequisite for option 1.
