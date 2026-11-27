# Compatibility policy

Andruna's job is to give the user the experience each application was built
to give. How we get there may differ from Android as much as Windows
requires: Windows APIs, the PC's real hardware, desktop windows and desktop
defaults are the expected way. Being different from AOSP internally is never
a defect by itself.

What must stay the same is what the user and the application actually do.

## The rules

1. **The operation itself.** When an application asks for an operation (take
   a photo, record audio, read the location, pick a file, sign in), the user
   performs that operation through the PC's real equivalent. We do not
   replace it with a different operation because the real one is hard.
2. **Honest results.** We never report that something happened when it did
   not: a capture, a payment, an autofocus, a permission the user did not
   grant. When something is unavailable, the application gets Android's own
   "unavailable" or "unsupported" behavior.
3. **Honest capabilities.** Features, hardware and characteristics describe
   what the PC and Andruna really provide. No sensor, camera facing, network
   or certification is invented.
4. **The user's protections.** Permissions, consent, security and the
   application lifecycle work as on Android.
5. **No app-specific code.** No package-name special cases in shipped code.
   Tests exercise production behavior.

## Desktop adaptations

Adaptations to a desktop are in scope when they are documented, named as
Andruna's where applications can see them, and give way to real hardware.
A default may describe the PC's real state (it stands still, its camera faces
the user); it never substitutes content.

Every adaptation that applications can see is a maintainer decision, recorded
as an ADR and listed here.

| Adaptation | Status |
| --- | --- |
| Freeform resizable windows with real configuration changes | Decided (spec) |
| A PC camera not marked as rear is reported as front-facing | Proposed |
| Camera orientation relative to the display, since Windows delivers upright frames | Proposed |
| Device identity: the PC's real manufacturer and model; Andruna's own brand, product, device and fingerprint | Proposed |
| microG presented with Google's public signing certificate, only when signed by microG's key | Proposed ([Google services](07-google-services.md)) |
| Windows accent color as Android's dynamic color source | Proposed |
| System property defaults (SDK level, ABI, Andruna's brand, product and device) | Proposed ([ADR 0019](../decisions/0019-system-property-defaults.md)) |

## Review

A change that replaces an operation, reports a false result, invents a
capability or adds a package-specific exception is a merge blocker until it
is removed or redesigned. Reviewers check this before correctness.
