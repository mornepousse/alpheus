## What and why

<!-- One logical change. Say why, not only what. -->

## Checklist

- [ ] A test was written first and I saw it fail for the right reason.
- [ ] No test starts a real session tool; everything goes through `FakeExecutor`, fakes, private buses or the headless harness.
- [ ] The guardrail tests are untouched or stricter.
- [ ] Anything that becomes resident (thread, timer, connection, cache) was measured with the release memory suite; the deltas are below.
- [ ] Docs, the configuration reference and the changelog are updated where the change shows.
- [ ] Fixtures are anonymised.
- [ ] AI assistance, if any, is disclosed below.

## Measurements

<!-- Memory deltas, wake-ups at rest; or "nothing resident". -->

## AI assistance

<!-- None, or which tool and for what. -->
