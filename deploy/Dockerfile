# syntax=docker/dockerfile:1
# PM OS services: every Rust binary in one image; compose picks the binary per service.
# Built on the Spark itself (arm64) with `deploy/pmos-admin build`.
#
# Behind a proxy that re-signs TLS, `pmos-admin build` passes HTTPS_PROXY as a build argument
# and the proxy's CA certificate as the build secret `ca` (PMOS_BUILD_CA in deploy/.env).

FROM rust:1.100-bookworm AS build
ARG HTTPS_PROXY
ARG NO_PROXY
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
 && for b in pmos pmos-toolserver pmos-docs pmos-gateway pmos-bots pmos-slack pmos-console pmos-evals; do \
      cp "target/release/$b" /out/; \
    done

# curl for health checks, git and ssh for the tool server's content sync, CA certificates:
# all already in this base, so the runtime image needs no package installs.
FROM buildpack-deps:bookworm-scm
RUN useradd --uid 10001 --create-home --shell /usr/sbin/nologin pmos
COPY --from=build /out/ /usr/local/bin/
COPY config/ /etc/pmos/config/
USER pmos
WORKDIR /home/pmos
CMD ["pmos", "--help"]
