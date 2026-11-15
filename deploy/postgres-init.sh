#!/bin/bash
# Runs once, when the Postgres volume is first created: one database and one role per service,
# so a service can only reach its own tables. Passwords come from deploy/.env.
set -euo pipefail
for svc in toolserver bots docs; do
  var="PMOS_DB_${svc^^}_PASSWORD"
  pass="${!var}"
  psql -v ON_ERROR_STOP=1 --username postgres <<SQL
CREATE ROLE pmos_${svc} LOGIN PASSWORD '${pass}';
CREATE DATABASE pmos_${svc} OWNER pmos_${svc};
REVOKE ALL ON DATABASE pmos_${svc} FROM PUBLIC;
SQL
done
