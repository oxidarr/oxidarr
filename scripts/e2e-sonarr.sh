#!/usr/bin/env bash
# End-to-end acceptance check against a real, unmodified Sonarr: start a
# throwaway Sonarr container, start oxidarr-prowl against a temp data dir,
# wire Sonarr up as an application, push one Cardigann indexer, and confirm
# both sides agree it works — Sonarr's own GET /api/v3/indexer lists it, and
# Sonarr's own POST /api/v3/indexer/test passes against our Torznab endpoint.
#
# This is NOT run in CI. It needs a working `docker` and network access (to
# pull the Sonarr image, and for the Cardigann indexer's own search to reach
# its real tracker). Run it by hand:
#
#   ./scripts/e2e-sonarr.sh
#   ./scripts/e2e-sonarr.sh --keep          # leave everything running to poke at
#   ./scripts/e2e-sonarr.sh --skip-docker   # use an already-running Sonarr
#
# Environment variables (all optional):
#   INDEXER_DEF        Cardigann definition id to add (default: eztv — a
#                       public tracker with no required settings; any other
#                       credential-free public definition works just as
#                       well, this is only a sane default, not a
#                       recommendation of that tracker over another)
#   SONARR_IMAGE        Sonarr image to run (default: lscr.io/linuxserver/sonarr:latest)
#   SONARR_PORT         host port Sonarr's container publishes on (default: 8989)
#   OXIDARR_PORT        port oxidarr-prowl binds to (default: 19696)
#   OXIDARR_PROFILE     cargo build profile to use (default: debug)
#   OXIDARR_EXTERNAL_HOST
#                       hostname Sonarr's container uses to reach oxidarr-prowl
#                       on the host (default: host.docker.internal)
#   SONARR_URL          only with --skip-docker: base URL of an already-running
#                       Sonarr (e.g. http://127.0.0.1:8989)
#   SONARR_API_KEY      only with --skip-docker: that Sonarr's API key
#
# Flags:
#   --keep          skip cleanup: leave the Sonarr container, oxidarr-prowl
#                   process, and temp data dir running/in place for
#                   inspection. A --keep'd oxidarr-prowl process is left
#                   listening on $OXIDARR_PORT — a later run without --keep
#                   will fail to bind that port until it's killed by hand
#                   (the Sonarr container is cleaned up automatically on the
#                   next run either way, via a stale-container removal at
#                   startup).
#   --skip-docker   assume a real Sonarr is already reachable at $SONARR_URL
#                   with API key $SONARR_API_KEY; never touches docker
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

KEEP=0
SKIP_DOCKER=0

usage() {
  sed -n '2,43p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --keep)
      KEEP=1
      shift
      ;;
    --skip-docker)
      SKIP_DOCKER=1
      shift
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage
      exit 1
      ;;
  esac
done

INDEXER_DEF="${INDEXER_DEF:-eztv}"
SONARR_IMAGE="${SONARR_IMAGE:-lscr.io/linuxserver/sonarr:latest}"
SONARR_PORT="${SONARR_PORT:-8989}"
OXIDARR_PORT="${OXIDARR_PORT:-19696}"
OXIDARR_PROFILE="${OXIDARR_PROFILE:-debug}"
OXIDARR_EXTERNAL_HOST="${OXIDARR_EXTERNAL_HOST:-host.docker.internal}"
CONTAINER_NAME="oxidarr-e2e-sonarr"

# oxidarr-prowl binds locally on 127.0.0.1: Docker Desktop's
# host.docker.internal is specifically designed to reach services bound to
# the host's own loopback interface (this is documented Docker Desktop
# behavior on macOS/Windows, not incidental). Plain Linux docker's
# host.docker.internal (via --add-host=host.docker.internal:host-gateway,
# passed below) instead resolves to the host's bridge-gateway address, which
# a service bound to 127.0.0.1 only is NOT reachable through — so on Linux
# this script binds oxidarr-prowl to 0.0.0.0 instead. Either way,
# OXIDARR_EXTERNAL_HOST is what a client *inside* the Sonarr container uses;
# the script itself always talks to oxidarr-prowl over 127.0.0.1, which
# always works regardless of which interface it bound.
if [[ "$(uname -s)" == "Darwin" ]]; then
  OXIDARR_BIND_HOST="127.0.0.1"
else
  OXIDARR_BIND_HOST="0.0.0.0"
fi

OXIDARR_URL="http://127.0.0.1:${OXIDARR_PORT}"
OXIDARR_EXTERNAL_URL="http://${OXIDARR_EXTERNAL_HOST}:${OXIDARR_PORT}"

WORKDIR="$(mktemp -d "${TMPDIR:-/tmp}/oxidarr-e2e-sonarr.XXXXXX")"
DATA_DIR="$WORKDIR/data"
# Named distinctly from the OXIDARR_LOG *environment variable* oxidarr-prowl
# itself reads (see config.rs's log-level field) — start_oxidarr below
# deliberately unsets that env var before launching, which would otherwise
# silently wipe out this script variable too if they shared a name.
PROWL_LOG="$WORKDIR/oxidarr.log"
BODY_FILE="$WORKDIR/response.json"
mkdir -p "$DATA_DIR"

OXIDARR_PID=""
STAGING_DEFS_DIR=""

log() {
  echo "==> $1" >&2
}

# Prints `$1` as a failure, optionally tailing `$2` (a log/body file) for
# context, then exits non-zero. Every hard-failure path in this script goes
# through this, per this task's own "every failure path prints WHAT failed
# and the relevant log tail" requirement.
die() {
  echo "FAIL: $1" >&2
  if [[ -n "${2:-}" && -f "$2" ]]; then
    echo "--- tail of $2 ---" >&2
    tail -n 40 "$2" >&2 || true
  fi
  exit 1
}

cleanup() {
  local exit_code=$?
  if [[ -n "$OXIDARR_PID" ]] && kill -0 "$OXIDARR_PID" 2>/dev/null; then
    if [[ "$KEEP" == "1" ]]; then
      log "leaving oxidarr-prowl running (pid $OXIDARR_PID, data dir $DATA_DIR) — --keep was passed"
    else
      kill "$OXIDARR_PID" 2>/dev/null || true
      wait "$OXIDARR_PID" 2>/dev/null || true
    fi
  fi
  if [[ "$SKIP_DOCKER" != "1" ]]; then
    if [[ "$KEEP" == "1" ]]; then
      log "leaving Sonarr container '$CONTAINER_NAME' running — --keep was passed"
    else
      docker rm -f "$CONTAINER_NAME" >/dev/null 2>&1 || true
    fi
  fi
  if [[ "$KEEP" != "1" ]]; then
    rm -rf "$WORKDIR"
    if [[ -n "$STAGING_DEFS_DIR" ]]; then
      rm -rf "$STAGING_DEFS_DIR"
    fi
  else
    log "leaving working directory in place: $WORKDIR"
  fi
  exit "$exit_code"
}
trap cleanup EXIT

check_prereqs() {
  local missing=()
  local cmd
  for cmd in cargo jq curl sed; do
    command -v "$cmd" >/dev/null 2>&1 || missing+=("$cmd")
  done
  if [[ "$SKIP_DOCKER" != "1" ]]; then
    command -v docker >/dev/null 2>&1 || missing+=("docker")
  fi
  if [[ ${#missing[@]} -gt 0 ]]; then
    die "missing required tool(s): ${missing[*]}"
  fi
}

# Idempotent re-runs: a container from a previous --keep'd (or crashed) run
# must not block a fresh `docker run` under the same fixed name.
remove_stale_container() {
  if [[ "$SKIP_DOCKER" != "1" ]]; then
    docker rm -f "$CONTAINER_NAME" >/dev/null 2>&1 || true
  fi
}

start_sonarr() {
  log "starting Sonarr container '$CONTAINER_NAME' ($SONARR_IMAGE) on port $SONARR_PORT"
  docker run -d --name "$CONTAINER_NAME" \
    --add-host=host.docker.internal:host-gateway \
    -p "127.0.0.1:${SONARR_PORT}:8989" \
    "$SONARR_IMAGE" >/dev/null \
    || die "starting the Sonarr container"
}

# Polls the container's own /config/config.xml (read via `docker exec cat`,
# parsed with the HOST's own `sed` rather than whatever's inside the
# container image) until an <ApiKey> shows up, then polls Sonarr's real HTTP
# API with that key until it actually answers — config.xml can exist slightly
# before the web server is serving requests.
wait_for_sonarr_api_key() {
  log "waiting for Sonarr to write its config and start serving"
  local attempts=90
  local key=""
  while [[ $attempts -gt 0 ]]; do
    local config_xml
    config_xml="$(docker exec "$CONTAINER_NAME" cat /config/config.xml 2>/dev/null || true)"
    if [[ -n "$config_xml" ]]; then
      key="$(printf '%s' "$config_xml" | sed -n 's:.*<ApiKey>\(.*\)</ApiKey>.*:\1:p')"
      if [[ -n "$key" ]]; then
        local status
        status="$(curl -sS -o /dev/null -w '%{http_code}' -H "X-Api-Key: $key" "$SONARR_URL/api/v3/system/status" 2>/dev/null || echo 000)"
        if [[ "$status" == "200" ]]; then
          printf '%s' "$key"
          return 0
        fi
      fi
    fi
    attempts=$((attempts - 1))
    sleep 2
  done
  return 1
}

# Points $DATA_DIR/definitions at a flat directory of <id>.yml files, the
# shape DefinitionStore reads (see crates/oxidarr-prowl/src/definitions.rs).
# `scripts/fetch-definitions.sh <dest>` writes into `<dest>/v11/`, one level
# too deep for that — so this always symlinks the store's own `definitions`
# entry at the `v11` subdirectory, never the fetch destination itself.
#
# When this repo checkout already has `.definitions/v11` populated (the
# usual case for anyone who has run `scripts/fetch-definitions.sh` before,
# including this repo's own test fixtures workflow), that is reused directly
# instead of re-fetching — much faster, and the acceptance run needs no
# network access for this step at all.
prepare_definitions() {
  local repo_defs="$REPO_ROOT/.definitions/v11"
  if [[ -d "$repo_defs" ]]; then
    log "reusing this checkout's .definitions/v11 (no fetch needed)"
    ln -s "$repo_defs" "$DATA_DIR/definitions"
  else
    log "fetching Cardigann definitions (no cached .definitions/v11 found)"
    STAGING_DEFS_DIR="$(mktemp -d "${TMPDIR:-/tmp}/oxidarr-e2e-defs.XXXXXX")"
    "$REPO_ROOT/scripts/fetch-definitions.sh" "$STAGING_DEFS_DIR" \
      || die "fetching Cardigann definitions"
    ln -s "$STAGING_DEFS_DIR/v11" "$DATA_DIR/definitions"
  fi
  if [[ ! -f "$DATA_DIR/definitions/${INDEXER_DEF}.yml" ]]; then
    die "INDEXER_DEF '$INDEXER_DEF' has no ${INDEXER_DEF}.yml in $DATA_DIR/definitions"
  fi
}

start_oxidarr() {
  log "building oxidarr-prowl ($OXIDARR_PROFILE profile)"
  local build_args=(build -p oxidarr-prowl)
  if [[ "$OXIDARR_PROFILE" == "release" ]]; then
    build_args+=(--release)
  fi
  (cd "$REPO_ROOT" && cargo "${build_args[@]}") \
    || die "building oxidarr-prowl"

  local bin="$REPO_ROOT/target/${OXIDARR_PROFILE}/oxidarr-prowl"
  [[ -x "$bin" ]] || die "expected binary not found at $bin"

  log "starting oxidarr-prowl on $OXIDARR_BIND_HOST:$OXIDARR_PORT (external url $OXIDARR_EXTERNAL_URL)"
  # Only the three variables this run cares about are set; anything else
  # OXIDARR_*-shaped in the operator's own shell is deliberately cleared so
  # it can't silently redirect this run at a real config file/proxy/data dir.
  unset OXIDARR_CONFIG OXIDARR_PROXY OXIDARR_LOG 2>/dev/null || true
  export OXIDARR_BIND="${OXIDARR_BIND_HOST}:${OXIDARR_PORT}"
  export OXIDARR_DATA_DIR="$DATA_DIR"
  export OXIDARR_EXTERNAL_URL="$OXIDARR_EXTERNAL_URL"
  "$bin" >"$PROWL_LOG" 2>&1 &
  OXIDARR_PID=$!
}

# Parses the "api key: <key>" line oxidarr-prowl's own startup banner prints
# (see print_startup_banner in crates/oxidarr-prowl/src/main.rs) — the only
# place this instance's API key is ever surfaced.
#
# Reports failure via `return 1` rather than `die`: this function is always
# called through `$(...)` command substitution, which runs it in a
# subshell — `die`'s own `exit` would only end that subshell, silently
# leaving the caller with an empty key instead of stopping the script. The
# caller is responsible for turning a failed return into a `die`.
wait_for_oxidarr_api_key() {
  log "waiting for oxidarr-prowl to print its API key"
  local attempts=60
  while [[ $attempts -gt 0 ]]; do
    if ! kill -0 "$OXIDARR_PID" 2>/dev/null; then
      log "oxidarr-prowl exited before printing its API key"
      return 1
    fi
    local line
    line="$(grep -m1 '^api key: ' "$PROWL_LOG" 2>/dev/null || true)"
    if [[ -n "$line" ]]; then
      printf '%s' "${line#api key: }"
      return 0
    fi
    attempts=$((attempts - 1))
    sleep 1
  done
  log "timed out waiting for oxidarr-prowl's startup banner"
  return 1
}

create_application() {
  log "creating the Sonarr application row in oxidarr"
  local payload
  payload="$(jq -n --arg baseUrl "$SONARR_URL" --arg apiKey "$SONARR_API_KEY" '{
    name: "Sonarr (e2e)",
    implementation: "Sonarr",
    syncLevel: "fullSync",
    fields: [
      {name: "baseUrl", value: $baseUrl},
      {name: "apiKey", value: $apiKey}
    ]
  }')"
  local status
  status="$(curl -sS -o "$BODY_FILE" -w '%{http_code}' -X POST "$OXIDARR_URL/api/v1/applications" \
    -H "X-Api-Key: $OXIDARR_API_KEY" -H 'Content-Type: application/json' \
    -d "$payload")"
  if [[ "$status" != "201" ]]; then
    die "creating the application row (status $status)" "$BODY_FILE"
  fi
}

# Reports failure via `return 1` (after logging specifics to stderr) rather
# than `die` — see wait_for_oxidarr_api_key's own comment for why: this
# function's stdout (the new indexer's local id) is captured through `$(...)`
# by its caller, so a `die`'s `exit` here would only end the subshell.
create_indexer() {
  log "creating the '$INDEXER_DEF' Cardigann indexer in oxidarr"
  local payload
  payload="$(jq -n --arg def "$INDEXER_DEF" '{
    name: ($def + " (e2e)"),
    implementation: "Cardigann",
    definitionName: $def,
    enable: true,
    priority: 25,
    fields: []
  }')"
  local status
  status="$(curl -sS -o "$BODY_FILE" -w '%{http_code}' -X POST "$OXIDARR_URL/api/v1/indexer" \
    -H "X-Api-Key: $OXIDARR_API_KEY" -H 'Content-Type: application/json' \
    -d "$payload")"
  if [[ "$status" != "201" ]]; then
    log "creating the indexer failed (status $status)"
    cat "$BODY_FILE" >&2 || true
    return 1
  fi

  local sync_error
  sync_error="$(jq -r '.syncError // empty' "$BODY_FILE")"
  if [[ -n "$sync_error" ]]; then
    log "indexer create pushed to Sonarr but reported a syncError: $sync_error"
    return 1
  fi

  jq -r '.id' "$BODY_FILE"
}

# A direct sanity check of oxidarr's own Torznab pipeline, independent of
# whatever Sonarr itself later reports — isolates "oxidarr's Cardigann/
# Torznab plumbing is broken" from "Sonarr can't reach oxidarr over the
# network" before spending any time polling Sonarr at all.
check_own_torznab_caps() {
  local indexer_id="$1"
  log "checking oxidarr's own t=caps for indexer $indexer_id"
  local caps_file="$WORKDIR/caps.xml"
  local status
  status="$(curl -sS -o "$caps_file" -w '%{http_code}' \
    "${OXIDARR_URL}/${indexer_id}/api?apikey=${OXIDARR_API_KEY}&t=caps")"
  if [[ "$status" != "200" ]] || ! grep -q '<caps' "$caps_file"; then
    die "oxidarr's own t=caps for indexer $indexer_id did not respond as expected (status $status)" "$caps_file"
  fi
}

# Polls Sonarr's own GET /api/v3/indexer until an entry's `fields[].baseUrl`
# exactly matches "$OXIDARR_EXTERNAL_URL/$1" — the local indexer id sync.rs's
# own `torznab_base_url` builds this from (see crates/oxidarr-prowl/src/sync.rs).
# An exact match, not a substring/port check, so a repeated --skip-docker run
# against a shared, long-lived Sonarr can never pick up a stale entry left
# over from an earlier run (those carry a different indexer id in the same
# path). The synced display name ("<name> (Oxidarr)", see crate::sync's own
# docs) is not used for this at all — it is not guaranteed unique against
# whatever else a shared Sonarr instance might carry.
#
# Prints the matched indexer object (Sonarr's own resource JSON) to stdout.
wait_for_sonarr_indexer() {
  local indexer_id="$1"
  local expected_base_url="${OXIDARR_EXTERNAL_URL}/${indexer_id}"
  log "waiting for indexer $indexer_id (baseUrl $expected_base_url) to appear in Sonarr"
  local attempts=60
  while [[ $attempts -gt 0 ]]; do
    local body
    body="$(curl -sS -H "X-Api-Key: $SONARR_API_KEY" "$SONARR_URL/api/v3/indexer" 2>/dev/null || true)"
    if [[ -n "$body" ]]; then
      local match
      match="$(printf '%s' "$body" | jq -c --arg expected "$expected_base_url" '
        map(select(any(.fields[]?; .name == "baseUrl" and .value == $expected)))
        | .[0] // empty
      ' 2>/dev/null || true)"
      if [[ -n "$match" ]]; then
        printf '%s' "$match"
        return 0
      fi
    fi
    attempts=$((attempts - 1))
    sleep 2
  done
  return 1
}

run_sonarr_indexer_test() {
  local indexer_json="$1"
  log "running Sonarr's own POST /api/v3/indexer/test against it"
  local test_body="$WORKDIR/sonarr-test.json"
  local status
  status="$(curl -sS -o "$test_body" -w '%{http_code}' -X POST "$SONARR_URL/api/v3/indexer/test" \
    -H "X-Api-Key: $SONARR_API_KEY" -H 'Content-Type: application/json' \
    -d "$indexer_json")"
  if [[ "$status" == "200" ]]; then
    log "PASS: Sonarr's own indexer test succeeded"
    return 0
  fi
  echo "FAIL: Sonarr's own indexer test rejected our Torznab endpoint (status $status)" >&2
  echo "--- Sonarr's response ---" >&2
  cat "$test_body" >&2 || true
  return 1
}

print_manual_grab_step() {
  local name="$1"
  cat >&2 <<EOF

Everything oxidarr can prove on its own is done. To finish the acceptance
check by hand:
  1. Open Sonarr at $SONARR_URL and add a monitored series.
  2. Trigger an interactive/manual search for it.
  3. Grab a result served through the "$name" indexer.
  4. Confirm the download client receives it — that is the one step this
     script cannot automate without a download client and a monitored
     series already configured.
EOF
}

main() {
  check_prereqs
  remove_stale_container

  if [[ "$SKIP_DOCKER" == "1" ]]; then
    [[ -n "${SONARR_URL:-}" ]] || die "--skip-docker requires SONARR_URL to be set"
    [[ -n "${SONARR_API_KEY:-}" ]] || die "--skip-docker requires SONARR_API_KEY to be set"
    log "skipping docker: using Sonarr already running at $SONARR_URL"
  else
    SONARR_URL="http://127.0.0.1:${SONARR_PORT}"
    start_sonarr
    SONARR_API_KEY="$(wait_for_sonarr_api_key)" || {
      docker logs --tail 40 "$CONTAINER_NAME" >"$WORKDIR/sonarr-container.log" 2>&1 || true
      die "Sonarr never started serving its API" "$WORKDIR/sonarr-container.log"
    }
  fi
  log "Sonarr API key acquired"

  prepare_definitions
  start_oxidarr
  OXIDARR_API_KEY="$(wait_for_oxidarr_api_key)" \
    || die "oxidarr-prowl never printed its API key" "$PROWL_LOG"
  log "oxidarr API key acquired"

  create_application
  local indexer_id
  indexer_id="$(create_indexer)" || die "creating the indexer" "$BODY_FILE"
  log "indexer created locally as id $indexer_id"
  check_own_torznab_caps "$indexer_id"

  local sonarr_indexer
  sonarr_indexer="$(wait_for_sonarr_indexer "$indexer_id")" || {
    curl -sS -H "X-Api-Key: $SONARR_API_KEY" "$SONARR_URL/api/v3/indexer" \
      >"$WORKDIR/sonarr-indexers.json" 2>&1 || true
    die "our indexer never appeared in Sonarr's GET /api/v3/indexer" "$WORKDIR/sonarr-indexers.json"
  }
  local sonarr_name
  sonarr_name="$(printf '%s' "$sonarr_indexer" | jq -r '.name')"
  log "found in Sonarr as '$sonarr_name'"

  local test_result=0
  run_sonarr_indexer_test "$sonarr_indexer" || test_result=1

  print_manual_grab_step "$sonarr_name"

  if [[ "$test_result" == "0" ]]; then
    log "acceptance check passed"
  else
    log "acceptance check FAILED — see Sonarr's own test response above"
  fi
  # `return`, not `exit`: as the last statement in the script, main's own
  # return status already becomes the script's exit status, and an `exit`
  # here specifically (verified against shellcheck 0.11.0) makes it lose
  # track of `trap cleanup EXIT`'s reference to `cleanup` below, flagging it
  # SC2329 "never invoked" — a false positive, not a real dead function.
  return "$test_result"
}

main
