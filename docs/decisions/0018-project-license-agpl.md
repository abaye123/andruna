# 0018 - Project license: AGPL-3.0-only, contributions under a CLA

**Status:** accepted, 2026-11-27

## Context
ADR 0006 made Andruna open source and left the license open, allowing only
permissive dependencies so that every option stayed available. The
maintainer has now chosen the license. Two further needs come with it: the
management app ships with the Windows App SDK, which is under Microsoft's
proprietary redistributable terms, and the maintainer wants to keep the
right to release the project, including outside contributions, under other
terms (for example a closed edition) in the future.

## Decision
- Andruna's own code is licensed under the GNU Affero General Public License,
  version 3 only (SPDX `AGPL-3.0-only`). The text is in `LICENSE`.
- An additional permission under section 7 of the AGPL allows conveying
  Andruna combined with the Windows App SDK without its source code. The text
  is in `LICENSING.md`.
- Outside contributions are accepted only under the Contributor License
  Agreement in `CLA.md`. It grants the project owner the right to license
  contributions under any terms, including proprietary ones, while the
  contributor keeps their copyright. The project owner promises that
  contributions stay available under the project's open source license.
- The dependency policy of ADR 0006 is unchanged: third-party code stays
  permissive (with the named exceptions recorded there and in ADR 0012).
  Copyleft dependencies would remove the option of other licensing terms.

## Consequences
- The workspace manifest declares `license = "AGPL-3.0-only"`; crates stay
  unpublished (`publish = false`).
- `CLA.md` is a draft until a lawyer has reviewed it and the project owner's
  legal name is filled in. No outside contribution is merged before then.
- A CLA check (a bot or a signed record per contributor) is set up before the
  repository accepts outside pull requests.
- AOSP code (Apache-2.0) and libcore (GPL-2.0 with the Classpath exception,
  ADR 0012) are compatible with distribution under these terms.
