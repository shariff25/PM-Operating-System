# shellcheck shell=bash disable=SC2034
# PM OS: the one `openshell sandbox create` command, shared by `deploy/pmos-admin sandbox` and the
# preflight's canaries (deploy/preflight.sh, O-CANARY and O-REACH), so the two can't drift apart
# (docs/openshell-any-host-spec.md §3.7). Sourced, never run; it defines one function and runs
# nothing.
#
# `--from` takes "an explicit OCI image reference" (OpenShell's sandbox page). Verify on a real
# host (the acceptance run, §6 step 2): that it takes the local tag pmos-worker:<version> without
# a registry.

# pmos_sandbox_create_cmd <name> <version> <policy file> [more arguments: --provider, --upload …]
# Sets SANDBOX_CREATE to the arguments for `openshell`.
pmos_sandbox_create_cmd() {
  SANDBOX_CREATE=(sandbox create --name "$1" --from "pmos-worker:$2" --policy "$3" "${@:4}")
}
