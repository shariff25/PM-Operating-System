# Preflight fixtures

Hosts for `deploy/preflight.sh --root <dir>` (slice H1; `scripts/test-preflight.sh`,
`scripts/test-pmos-admin.sh`). `ubuntu-24.04-amd64` is a whole host that passes every check
(stage `run`, `PMOS_MODEL_SOURCE=local`): a file `<dir>/<path>` stands for a host file, `cmd/<id>`
(and `cmd/<id>.exit`) for a command's output, `bin/openshell` for OpenShell's CLI. The other
directories are overlays: only the files that differ, copied over a copy of the Ubuntu host
(`debian-12-arm64`, `kernel-5.15`, `no-landlock-lsm`, `landlock-abi-2`, `docker-27`, `no-nvidia`).
The tests build the remaining shapes from these by changing one file. Addresses are the pinned
ones of slice H2 (`10.201.77.10` to `.12`, the `ip_range` `10.201.77.128/25`, the managed block).
