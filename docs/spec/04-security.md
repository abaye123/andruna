# Security

## Threat model

An installed app may be malicious or compromised. It runs our code and its
own native code in its process. Andruna must keep it from:

- reading or changing the user's files outside what the user granted,
- reading another app's data or Andruna's account tokens,
- using camera, microphone or location without both Android and Windows
  consent,
- impersonating another package to sysd or to Google services.

Not in scope: a malicious user, kernel exploits, side channels.

## Isolation: one AppContainer per package

- Every package gets an AppContainer profile at install (SID derived from the
  package name and a per-install salt). The app process runs in it with a
  low-integrity, restricted token.
- Capabilities follow the package's granted Android permissions:

| Android permission | AppContainer capability / mechanism |
| --- | --- |
| `INTERNET` | `internetClient` (and `privateNetworkClientServer` only if Android local network access is granted) |
| `CAMERA` | Brokered by sysd: the app never opens the camera device itself |
| `RECORD_AUDIO` | Brokered by sysd |
| `ACCESS_*_LOCATION` | Brokered by sysd |
| Storage / media | Brokered: Storage Access Framework and MediaStore are served by sysd, which runs a Windows file picker outside the container |

- Brokered access also checks Windows privacy settings for Andruna, so both
  consents apply.
- The app's data directory, its APK directory and the runtime files are the
  only file system locations the container can reach (ACLs set at install).
- GPU access: AppContainer processes can use D3D11; verify ANGLE and DComp
  under the container in spike S7.

### Warm processes and AppContainer

A process cannot change its AppContainer after it starts. A generic warm pool
would therefore run outside any container, which is not acceptable. Options,
decided in spike S7:

1. Warm one host process in the container of the package most likely to be
   opened next (recently used).
2. Create the process suspended in the right container at launch, and map
   the already-built boot image section into it, so the cost is process
   creation plus runtime init, without boot image load.

## Identity

- sysd identifies every Binder peer by its process token (AppContainer SID),
  never by what the process says. `getCallingUid` is derived from that.
- Each package has a stable Android uid assigned at install.

## Secrets

- Google account master tokens and other credentials live only in sysd,
  encrypted with DPAPI. Apps receive scoped tokens through `AccountManager`.
- No secret is passed on a command line or in an environment variable.

## Package trust

- APK signature verification (v1 to v4) before install and before every
  update; an update must match the installed signer lineage.
- Packages that claim to be Google's (Play services, Play Store, Services
  Framework) are installed only if signed by Google's certificate.
- The microG package may be presented with Google's public signing
  certificate only if it is signed by microG's own key
  ([Google services](07-google-services.md)).

## Attack surface in Rust

Every parser of untrusted input is Rust with fuzzing in CI: Binder
transaction parsing, ELF parsing, APK/zip/manifest/resources parsing (where
not done by AOSP code), the adbd protocol, IPC messages to the manager.

## Supply chain and updates

- All shipped binaries are code-signed and timestamped.
- Self-update downloads only from the project's releases, verifies the
  signature and a SHA-256 published with the release.
- Dependencies pinned by version and checksum; `cargo deny` for licenses and
  advisories in CI.

## Honest limits, stated to users

Until AppContainer isolation is complete and verified, the product states
plainly that apps run with the user's rights. The
first public preview ships with isolation on.
