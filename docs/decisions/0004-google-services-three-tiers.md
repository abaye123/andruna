# 0004 - Google services in three tiers

**Status:** accepted, 2026-11-26

## Decision
Tier 0: none. Tier 1: microG, shipped, on after the user signs in. Tier 2:
Google's own services, obtained by the user, verified as signed by Google,
experimental and off by default. Attestation, certification, Play Billing and
Widevine are never faked. Details: docs/spec/07-google-services.md.

## Consequences
- Signature spoofing for microG only, gated on microG's own certificate; a
  documented desktop adaptation.
- Account master tokens live only in sysd.
