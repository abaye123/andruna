# 0036 - AppContainer file access and the channel to sysd

**Status:** proposed, 2026-11-27

## Context
A process in an AppContainer reaches a file only if its DACL allows the
package SID, `ALL APPLICATION PACKAGES` or a capability SID the token
holds, and it runs at low integrity, so it cannot write objects with the
default medium label. Spike S7 ([results](../plan/spike-results/S7.md))
ran ART through andruna-app in a container with each option and checked
the sysd pipe from inside.

The install directory (`%LOCALAPPDATA%\Programs\Andruna`, ART root, boot
image) is not under Program Files, so it has no `ALL APPLICATION PACKAGES`
entry by default. Granting that group would let every AppContainer on the
machine (store apps, browser sandboxes) read it; granting each package SID
means rewriting the ACL of the whole tree at every install.

## Decision
- **Runtime files:** one inheritable read-and-execute ACE for a capability
  of our own, `andrunaRuntime` (SID from `DeriveCapabilitySidsFromName`),
  on the install directory, set by the installer (and by sysd at start,
  which is idempotent). Every Andruna container gets this capability;
  nothing else on the machine has it. This also works for less privileged
  AppContainers, which do not match `ALL APPLICATION PACKAGES`.
- **Package directories:** `data\<package>` gets a protected DACL (user,
  SYSTEM, package SID: full access) and a low mandatory label;
  `app\<package>` gets a protected DACL with read and execute for the
  package. Directories are created with that descriptor (setting a label
  later needs WRITE_OWNER, which the owner lacks without an ACE).
- **Identity:** the profile name is `Andruna.` and 128 bits of
  SHA-256(package, salt) with a per-install random salt stored by the
  package manager.
- **Channel to sysd:** an app process gets its sysd connection as an
  inherited pipe handle in the launch handle list, never by opening the
  launcher's named pipe. The named pipe stays user-only. (Opening it from
  a container works with `PipeSecurity::allow_app_container`, but
  `Client::connect` then cannot verify the server: the container may not
  open sysd's process to read its token.)

## Consequences
- An app process reads the runtime, its own code and its own data, and
  nothing else of the user's; S7 and `crates/andruna-sysd/tests/appcontainer.rs`
  check this.
- Uninstall deletes the profile; the directories go with the package.
- Less privileged AppContainers remain an option (S7: ART runs; D3D11 also
  needs the `registryRead` capability); not adopted until the rest of the
  runtime (fonts, ICU, networking) is checked under it.
