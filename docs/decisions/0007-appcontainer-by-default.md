# 0007 - AppContainer isolation by default

**Status:** accepted, 2026-11-26

## Decision
Every package runs in its own AppContainer from the first public preview.
Camera, microphone, location and user files are brokered by sysd. Details:
docs/spec/04-security.md.

## Consequences
- Warm processes must be created per package (spike S7).
- GPU and composition must be verified inside the container.
