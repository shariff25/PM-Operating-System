# Test record for the stack-test list (not a provider record)

This file stands in for a provider record in `crates/gateway/approved-hosted.stack-test.yaml`,
the test-only list that only a gateway built with the `stack-test-list` feature carries
(`docs/openshell-any-host-spec.md` §3.6, "How H12's stack test runs `hosted`"). It approves
nothing: the entry names only the stack test's local mock (`deploy/test/hosted-stack-test.url`
and `deploy/test/hosted-stack-test.model`), and every PM OS service treats a gateway that
reports `hosted:stack-test` as fake-data mode.
