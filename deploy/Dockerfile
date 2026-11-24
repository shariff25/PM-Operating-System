# syntax=docker/dockerfile:1
# PM OS services: every Rust binary in one image; compose picks the binary per service.
# Built on the host, for its own architecture, with `deploy/pmos-admin build`.
#
# Behind a proxy that re-signs TLS, `pmos-admin build` passes HTTPS_PROXY as a build argument
# and the proxy's CA certificate as the build secret `ca` (PMOS_BUILD_CA in deploy/.env).

# The web app's static files (web/dist), built once here and baked into the final image for
# pmos-web to serve (docs/web-app-running.md). Same proxy and CA handling as the worker image.
FROM node:22-bookworm-slim AS webbuild
ARG HTTPS_PROXY
ARG NO_PROXY
WORKDIR /src
COPY package.json pnpm-lock.yaml pnpm-workspace.yaml ./
COPY harness/package.json harness/
COPY web/package.json web/
RUN --mount=type=secret,id=ca,required=false \
    if [ -s /run/secrets/ca ]; then export NODE_EXTRA_CA_CERTS=/run/secrets/ca; fi \
 && npm_config_https_proxy="$HTTPS_PROXY" npm install -g pnpm@10.28.0 \
 && pnpm config set https-proxy "${HTTPS_PROXY:-}" \
 && pnpm install --frozen-lockfile --filter @pmos/web...
COPY web/ web/
RUN pnpm -C web build

FROM rust:1.100-bookworm AS build
ARG HTTPS_PROXY
ARG NO_PROXY
# Slice H12 (docs/openshell-any-host-spec.md §3.6, "Built only from committed code", X5):
# `pmos-admin build` passes the checkout's commit, the merged branch's commit and, when built
# offline, when that branch was last fetched; pmos-gateway compiles them in and reports them in
# /readyz. A gateway built without them refuses PMOS_MODEL_SOURCE=hosted.
ARG PMOS_BUILD_COMMIT
ARG PMOS_BUILD_MERGED_COMMIT
ARG PMOS_BUILD_LIST_CHECKED_AT
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    --mount=type=secret,id=ca,required=false \
    if [ -s /run/secrets/ca ]; then \
      cat /etc/ssl/certs/ca-certificates.crt /run/secrets/ca > /tmp/ca.crt; \
      export CARGO_HTTP_CAINFO=/tmp/ca.crt SSL_CERT_FILE=/tmp/ca.crt; \
    fi \
 && cargo build --release --locked --bins \
 && mkdir -p /out \
 && for b in pmos pmos-toolserver pmos-docs pmos-gateway pmos-bots pmos-slack pmos-console pmos-web pmos-evals pmos-sandbox-audit; do \
      cp "target/release/$b" /out/; \
    done

# curl for health checks, git and ssh for the tool server's content sync, CA certificates:
# all already in this base, so the runtime image needs no package installs.
FROM buildpack-deps:bookworm-scm
RUN useradd --uid 10001 --create-home --shell /usr/sbin/nologin pmos
# The sandbox-audit collector's own uid and its spool directory (docs/openshell-prover-and-
# sandbox-audit-spec.md §B.2), so its named volume starts owned by it and by nobody else.
RUN useradd --uid 10002 --no-create-home --shell /usr/sbin/nologin pmos-sandbox-audit \
 && mkdir -p /var/lib/pmos-sandbox-audit \
 && chown 10002:10001 /var/lib/pmos-sandbox-audit \
 && chmod 750 /var/lib/pmos-sandbox-audit
COPY --from=build /out/ /usr/local/bin/
COPY config/ /etc/pmos/config/
COPY --from=webbuild /src/web/dist/ /usr/share/pmos/web/
USER pmos
WORKDIR /home/pmos
CMD ["pmos", "--help"]
