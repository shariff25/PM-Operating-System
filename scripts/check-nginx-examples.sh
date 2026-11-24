#!/usr/bin/env bash
# `nginx -t` on the deploy kit's nginx examples (deploy/nginx/, slice H10;
# docs/openshell-any-host-spec.md §3.8): the public host's server blocks with their include, and
# the admin door's TLS terminator, with a throwaway self-signed certificate. Nothing is served or
# installed. `nginx -t` opens its listening sockets, so the copies listen on 127.0.0.1 ports of
# their own (19080 and up) instead of 80, 443 and the examples' addresses: every other line is
# checked as written. Without nginx (or openssl) it says so and exits 0; CI's deploy job runs it
# where the runner has nginx.
set -euo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
NGX="$REPO/deploy/nginx"
if ! command -v nginx >/dev/null 2>&1 || ! command -v openssl >/dev/null 2>&1; then
  echo "nginx examples: not checked (no nginx or openssl on this machine)"
  exit 0
fi
WORK=$(mktemp -d "${TMPDIR:-/tmp}/pmos-nginx-check.XXXXXX")
trap 'rm -rf "$WORK"' EXIT
openssl req -x509 -newkey rsa:2048 -nodes -days 1 -subj /CN=pmos.example.com \
  -keyout "$WORK/key.pem" -out "$WORK/cert.pem" >/dev/null 2>&1
port=19080
for f in pmos-public.conf.example pmos-admin.conf.example pmos-proxy.conf; do
  sed -e "s|/etc/nginx/pmos-proxy.conf|$WORK/pmos-proxy.conf|" \
    -e "s|/etc/ssl/pmos/[a-z-]*fullchain.pem|$WORK/cert.pem|" \
    -e "s|/etc/ssl/pmos/[a-z-]*privkey.pem|$WORK/key.pem|" "$NGX/$f" >"$WORK/$f.in"
  : >"$WORK/$f"
  while IFS= read -r line; do
    if [[ "$line" =~ ^([[:space:]]*)listen[[:space:]]+[^[:space:]\;]+(.*)$ ]]; then
      line="${BASH_REMATCH[1]}listen 127.0.0.1:$port${BASH_REMATCH[2]}"
      port=$((port + 1))
    fi
    printf '%s\n' "$line" >>"$WORK/$f"
  done <"$WORK/$f.in"
done
cat >"$WORK/nginx.conf" <<EOF
pid $WORK/nginx.pid;
error_log stderr;
events {}
http {
    access_log off;
    client_body_temp_path $WORK/body;
    proxy_temp_path $WORK/proxy;
    fastcgi_temp_path $WORK/fastcgi;
    uwsgi_temp_path $WORK/uwsgi;
    scgi_temp_path $WORK/scgi;
    include $WORK/pmos-public.conf.example;
    include $WORK/pmos-admin.conf.example;
}
EOF
nginx -t -q -p "$WORK" -c "$WORK/nginx.conf" -e stderr
echo "nginx examples: nginx -t passed"
