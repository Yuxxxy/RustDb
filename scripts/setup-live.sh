#!/usr/bin/env bash
# 本地真库环境：起 SQL Server / Oracle 容器并播种测试数据。
#
# 用法：
#   scripts/setup-live.sh mssql      # 只起 SQL Server
#   scripts/setup-live.sh oracle     # 只起 Oracle
#   scripts/setup-live.sh all        # 两个都起
#
# 跑完用：
#   RUSTDB_LIVE_TESTS=1 cargo test -p rustdb-db-mssql --test live
#   RUSTDB_LIVE_TESTS=1 cargo test -p rustdb-db-oracle --test live
#
# 注意：Oracle 镜像约 2.5GB、SQL Server 约 1.5GB，磁盘至少留 6GB 再起。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

MSSQL_CONTAINER="rustdb-mssql"
MSSQL_PORT="51433"
MSSQL_ENV=".tmp/mssql.env"

ORACLE_CONTAINER="rustdb-oracle"
ORACLE_PORT="51521"
ORACLE_PASSWORD="RustdbLocal123"

wait_for_mssql() {
  echo "[mssql] waiting for the engine to accept logins…"
  for _ in $(seq 1 60); do
    if MSYS_NO_PATHCONV=1 docker exec "$MSSQL_CONTAINER" bash -lc \
      '/opt/mssql-tools18/bin/sqlcmd -S localhost -U sa -P "$MSSQL_SA_PASSWORD" -C -l 2 -Q "select 1"' \
      >/dev/null 2>&1; then
      echo "[mssql] ready"
      return 0
    fi
    sleep 3
  done
  echo "[mssql] timeout" >&2
  return 1
}

setup_mssql() {
  if ! docker ps -a --format '{{.Names}}' | grep -qx "$MSSQL_CONTAINER"; then
    echo "[mssql] creating container…"
    docker run -d --name "$MSSQL_CONTAINER" \
      --env-file "$MSSQL_ENV" -p "${MSSQL_PORT}:1433" \
      mcr.microsoft.com/mssql/server:2022-latest >/dev/null
  fi
  wait_for_mssql
  echo "[mssql] seeding schema + rows…"
  docker cp .tmp/seed-mssql-all.sql "$MSSQL_CONTAINER:/tmp/seed.sql"
  MSYS_NO_PATHCONV=1 docker exec "$MSSQL_CONTAINER" bash -lc \
    '/opt/mssql-tools18/bin/sqlcmd -S localhost -U sa -P "$MSSQL_SA_PASSWORD" -C -b -i /tmp/seed.sql' >/dev/null
  echo "[mssql] done (127.0.0.1:${MSSQL_PORT})"
}

setup_oracle() {
  if ! docker ps -a --format '{{.Names}}' | grep -qx "$ORACLE_CONTAINER"; then
    echo "[oracle] creating container (first boot takes ~1-2 min)…"
    docker run -d --name "$ORACLE_CONTAINER" \
      -e "ORACLE_PASSWORD=${ORACLE_PASSWORD}" \
      -e "APP_USER=rustdb" -e "APP_USER_PASSWORD=${ORACLE_PASSWORD}" \
      -p "${ORACLE_PORT}:1521" gvenzl/oracle-free:23-slim >/dev/null
  fi
  echo "[oracle] waiting for the database to be open…"
  for _ in $(seq 1 80); do
    if MSYS_NO_PATHCONV=1 docker exec "$ORACLE_CONTAINER" bash -lc \
      'echo "select 1 from dual" | sql -s system/"$ORACLE_PASSWORD"@localhost:1521/FREEPDB1' \
      >/dev/null 2>&1; then
      echo "[oracle] ready"
      break
    fi
    sleep 5
  done
  echo "[oracle] seeding schema + rows…"
  MSYS_NO_PATHCONV=1 docker exec -i "$ORACLE_CONTAINER" bash -lc \
    "sql -s system/${ORACLE_PASSWORD}@localhost:1521/FREEPDB1" < .tmp/seed-oracle.sql >/dev/null
  echo "[oracle] done (127.0.0.1:${ORACLE_PORT})"
}

target="${1:-all}"
case "$target" in
  mssql) setup_mssql ;;
  oracle) setup_oracle ;;
  all) setup_mssql; setup_oracle ;;
  *) echo "usage: $0 [mssql|oracle|all]" >&2; exit 2 ;;
esac
