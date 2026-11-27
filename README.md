# Andruna

Andruna runs Android applications on Windows 11 as ordinary Windows programs:
no emulator, no virtual machine, no Android image to boot.

Each app runs in its own Windows process, with its own window, Start menu
entry, taskbar icon and Windows notifications. Inside that process run
Android's own runtime (ART) and framework, built from the Android Open Source
Project. Everything between them and Windows is Andruna's own code, written in
Rust.

> **Status:** specification. No code yet. Start with
> [docs/spec/00-overview.md](docs/spec/00-overview.md) and
> [docs/plan/roadmap.md](docs/plan/roadmap.md).

## Goals

- **Native speed and low overhead.** A shared boot image, a pool of pre-warmed
  processes, Binder over shared memory, zero-copy graphics and video.
- **Feels like Windows.** Real resizable windows, Mica, Snap Layouts, dark mode
  and accent color from Windows, Windows notifications, share and drag and drop.
- **Safe by default.** Every app runs in its own AppContainer.
- **x64 and ARM64 from day one.** On Windows on ARM, arm64 apps run natively.
- **Honest.** Applications get the operation they asked for, and never a
  result that did not happen.

## Documentation

| Document | Contents |
| --- | --- |
| [Overview](docs/spec/00-overview.md) | Vision, goals, non-goals, decisions |
| [Product](docs/spec/01-product.md) | Users, requirements, user flows |
| [Architecture](docs/spec/02-architecture.md) | Process model, layers, data on disk |
| [Components](docs/spec/03-components.md) | Every component and its interfaces |
| [Security](docs/spec/04-security.md) | Isolation, permissions, trust |
| [Performance](docs/spec/05-performance.md) | Targets and how they are measured |
| [Compatibility policy](docs/spec/06-compatibility-policy.md) | What "behave like Android" means here |
| [Google services](docs/spec/07-google-services.md) | The three tiers |
| [Build, test and CI](docs/spec/08-build-test-ci.md) | Toolchain, test layers, CI |
| [Roadmap](docs/plan/roadmap.md) | Phases and exit criteria |
| [Spikes](docs/plan/spikes.md) | Prototypes that settle the hard questions first |
| [Decisions](docs/decisions/) | Architecture decision records |
| [Open questions](docs/open-questions.md) | What is still undecided |

## License

Andruna is licensed under the GNU Affero General Public License, version 3
only (AGPL-3.0-only), with an additional permission for the Windows App SDK.
See [LICENSING.md](LICENSING.md) and
[ADR 0018](docs/decisions/0018-project-license-agpl.md). Contributions are
accepted under the [Contributor License Agreement](CLA.md). Third-party
dependencies stay permissively licensed
([ADR 0006](docs/decisions/0006-open-source-permissive-dependencies.md)).

Android is a trademark of Google LLC. Andruna is not affiliated with Google or
Microsoft.
