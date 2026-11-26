# 0009 - One maintainer; AI agents write the code

**Status:** accepted, 2026-11-26

## Decision
The maintainer sets direction, decides scope and reviews. AI agents
implement. Work is split into tasks that each own one module, with explicit
exit criteria and tests. Reports state what was verified and what was not.

## Consequences
- Module boundaries and interfaces are defined before parallel work starts.
- ADRs stay short (one page) and numbered without duplicates.
- Local builds must be fast enough for agents to iterate without waiting on
  CI for every step.
