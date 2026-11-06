#!/usr/bin/env bash
# P2 live environment: PostgreSQL + MySQL containers with TLS enabled and seeded data.
#
# Usage:
#   scripts/setup-live-p2.sh pg       # PostgreSQL only
#   scripts/setup-live-p2.sh mysql    # MySQL only
#   scripts/setup-live-p2.sh all      # both (default)
#
# Then:
#   RUSTDB_LIVE_TESTS=1 cargo test -p rustdb-db-postgres --test live -- --nocapture
#   RUSTDB_LIVE_TESTS=1 cargo test -p rustdb-db-mysql   --test live -- --nocapture
#
# Ports / credentials match db-postgres/tests/live.rs and db-mysql/tests/live.rs.
# Self-signed server certificates live in .tmp/certs (not version controlled).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

PG="rustdb-pg"
PG_PORT="55432"
PG_PASS="rustdb"
MYSQL="rustdb-mysql"
MYSQL_PORT="53306"
MYSQL_PASS="rustdb"
CERTS=".tmp/certs"
SSL_SNIPPET=".tmp/pg-ssl.conf"

log() { echo "[setup-live-p2] $*"; }

wait_pg() {
  log "waiting for postgres..."
  for _ in $(seq 1 60); do
    if MSYS_NO_PATHCONV=1 docker exec "$PG" pg_isready -U postgres -d rustdb_test >/dev/null 2>&1; then
      log "postgres ready"
      return 0
    fi
    sleep 2
  done
  log "postgres timeout" >&2
  return 1
}

wait_mysql() {
  log "waiting for mysql..."
  for _ in $(seq 1 60); do
    # mysqladmin ping returns 0 even on "Access denied"; use a real query instead.
    if MSYS_NO_PATHCONV=1 docker exec "$MYSQL" mysql -uroot -p"$MYSQL_PASS" -e "SELECT 1" >/dev/null 2>&1; then
      log "mysql ready"
      return 0
    fi
    sleep 2
  done
  log "mysql timeout" >&2
  return 1
}

ensure_running() {
  local name="$1"
  if docker ps --format '{{.Names}}' | grep -qx "$name"; then
    return 0
  fi
  if docker ps -a --format '{{.Names}}' | grep -qx "$name"; then
    # A stopped test container (e.g. killed by a bad ssl config) is rebuilt from
    # scratch; these containers hold disposable seeded data only.
    log "container $name is stopped; recreating it"
    docker rm -f "$name" >/dev/null
  fi
  return 1
}

gen_certs() {
  mkdir -p "$CERTS"
  if [[ -f "$CERTS/server.crt" && -f "$CERTS/server.key" ]]; then
    log "reusing existing self-signed certificate"
    return
  fi
  log "generating self-signed CA + server certificate..."
  openssl req -new -x509 -days 3650 -nodes \
    -out "$CERTS/ca.crt" -keyout "$CERTS/ca.key" \
    -subj "/CN=RustDb Live Test CA" >/dev/null 2>&1
  openssl req -new -nodes \
    -out "$CERTS/server.csr" -keyout "$CERTS/server.key" \
    -subj "/CN=localhost" >/dev/null 2>&1
  printf 'subjectAltName=DNS:localhost,IP:127.0.0.1\nextendedKeyUsage=serverAuth\n' > "$CERTS/ext.cnf"
  openssl x509 -req -days 3650 \
    -in "$CERTS/server.csr" -CA "$CERTS/ca.crt" -CAkey "$CERTS/ca.key" \
    -CAcreateserial -out "$CERTS/server.crt" -extfile "$CERTS/ext.cnf" >/dev/null 2>&1
}

setup_pg() {
  if ! ensure_running "$PG"; then
    log "creating postgres container..."
    docker run -d --name "$PG" \
      -e POSTGRES_PASSWORD="$PG_PASS" -e POSTGRES_DB=rustdb_test \
      -p "${PG_PORT}:5432" postgres:15.2 >/dev/null
  fi
  wait_pg
  gen_certs

  log "installing server certificate and enabling ssl=on..."
  MSYS_NO_PATHCONV=1 docker cp "$CERTS/server.crt" "$PG:/var/lib/postgresql/data/server.crt" >/dev/null
  MSYS_NO_PATHCONV=1 docker cp "$CERTS/server.key" "$PG:/var/lib/postgresql/data/server.key" >/dev/null
  # postgresql.conf requires quoted paths; unquoted absolute paths are a syntax error.
  printf "ssl = on\nssl_cert_file = '/var/lib/postgresql/data/server.crt'\nssl_key_file = '/var/lib/postgresql/data/server.key'\n" > "$SSL_SNIPPET"
  MSYS_NO_PATHCONV=1 docker cp "$SSL_SNIPPET" "$PG:/var/lib/postgresql/data/pg-ssl.conf" >/dev/null
  # Strip any previous ssl snippet first so the script is idempotent.
  MSYS_NO_PATHCONV=1 docker exec -u root "$PG" bash -c \
    'cd /var/lib/postgresql/data && chown postgres:postgres server.crt server.key && chmod 600 server.key && chmod 644 server.crt && grep -v -E "^(ssl|ssl_cert_file|ssl_key_file) =" postgresql.conf > postgresql.conf.tmp && mv postgresql.conf.tmp postgresql.conf && cat pg-ssl.conf >> postgresql.conf && rm -f pg-ssl.conf && chown postgres:postgres postgresql.conf && chmod 600 postgresql.conf'
  docker restart "$PG" >/dev/null
  wait_pg

  log "seeding postgres..."
  MSYS_NO_PATHCONV=1 docker exec -i "$PG" psql -U postgres -d rustdb_test -v ON_ERROR_STOP=1 \
    < scripts/seed-pg.sql >/dev/null
  local ssl_state
  ssl_state=$(MSYS_NO_PATHCONV=1 docker exec "$PG" psql -U postgres -d rustdb_test -Atc "show ssl")
  log "postgres done (127.0.0.1:${PG_PORT}, ssl=${ssl_state}, db=rustdb_test, user=postgres/${PG_PASS})"
}

setup_mysql() {
  if ! ensure_running "$MYSQL"; then
    log "creating mysql container..."
    # MySQL 5.7 ships OpenSSL 1.0.2 whose ECDHE suites make the server reject
    # rustls with HandshakeFailure. MySQL 8 speaks TLS1.2/1.3 + GCM suites.
    # mysql_native_password keeps the test compatible with mysql_async 0.37.
    MSYS_NO_PATHCONV=1 docker run -d --name "$MYSQL" \
      -e MYSQL_ROOT_PASSWORD="$MYSQL_PASS" \
      -p "${MYSQL_PORT}:3306" mysql:8.0 \
      --default-authentication-plugin=mysql_native_password >/dev/null
  fi
  wait_mysql
  log "seeding mysql..."
  MSYS_NO_PATHCONV=1 docker exec -i "$MYSQL" \
    mysql --default-character-set=utf8mb4 -uroot -p"$MYSQL_PASS" \
    < scripts/seed-mysql.sql >/dev/null
  local have_ssl
  have_ssl=$(MSYS_NO_PATHCONV=1 docker exec "$MYSQL" mysql -uroot -p"$MYSQL_PASS" -Nse \
    "SHOW VARIABLES LIKE 'have_ssl'" | awk '{print $2}')
  log "mysql done (127.0.0.1:${MYSQL_PORT}, have_ssl=${have_ssl:-NO}, db=rustdb_test, user=root/${MYSQL_PASS})"
}

target="${1:-all}"
case "$target" in
  pg) setup_pg ;;
  mysql) setup_mysql ;;
  all) setup_pg; setup_mysql ;;
  *) echo "usage: $0 [pg|mysql|all]" >&2; exit 2 ;;
esac
