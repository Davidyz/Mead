#!/usr/bin/env bash
# Activate the search oracle and let every devnet node self-register.
#
# The Lyquid constructor only *stages* the search oracle committee. This script
# performs the node-side follow-up that the constructor cannot do:
#   1. activate the first search-oracle epoch through `POST /oracle/activate`,
#   2. POST /nodes/register on each node so it submits a certified
#      register_node(node_id) call for its own identity.
#
# Activation is idempotent, and `make prepare-nodes` already calls the same
# endpoint. This script is safe to run repeatedly and works on an existing
# deployment.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

NODES_JSON="${NODES_JSON:-tests/fixture/nodes.json}"
REGISTER_TIMEOUT="${REGISTER_TIMEOUT:-60}"
ACTIVATION_TIMEOUT="${ACTIVATION_TIMEOUT:-60}"

if [[ ! -f "$NODES_JSON" ]]; then
  echo "missing $NODES_JSON; run scripts/prep_nodes.py first" >&2
  exit 1
fi

for command in jq curl; do
  if ! command -v "$command" >/dev/null 2>&1; then
    echo "required command not found: $command" >&2
    exit 1
  fi
done

Lyquid="$(jq -r '.lyquid_id' "$NODES_JSON")"
if [[ -z "$Lyquid" || "$Lyquid" == "null" ]]; then
  echo "could not read lyquid_id from $NODES_JSON" >&2
  exit 1
fi
LyquidLabel="${Lyquid#Lyquid-}"

first_host="$(
  jq -r --arg label "$LyquidLabel" \
    '.nodes[0] | "\($label).\(.node_id | sub("^Node-"; ""))"' "$NODES_JSON"
)"
first_port="$(jq -r '.nodes[0].port' "$NODES_JSON")"
activate_url="http://127.0.0.1:${first_port}/oracle/activate"
status_url="http://127.0.0.1:${first_port}/oracle/status"

echo "Lyquid: $Lyquid"
echo "Activating search oracle..."
deadline=$((SECONDS + ACTIVATION_TIMEOUT))
next_post=$SECONDS
backoff=1
active=0
while (( SECONDS < deadline )); do
  # Submit the needed epoch work, then poll the read-only status. Re-POST only
  # with backoff so polling does not submit a finalize certificate every second.
  if (( SECONDS >= next_post )); then
    activate_body="$(
      curl -sS -X POST -H "Host: ${first_host}" -H 'content-length: 0' \
        "$activate_url" 2>/dev/null || true
    )"
    activate_state="$(jq -r '.status // empty' <<<"${activate_body}" 2>/dev/null || true)"
    if [[ "$activate_state" == "active" ]]; then
      echo "  search oracle active (epoch $(jq -r '.epoch' <<<"${activate_body}"))"
      active=1
      break
    fi
    next_post=$((SECONDS + backoff))
    if (( backoff < 8 )); then
      backoff=$((backoff * 2))
    else
      backoff=8
    fi
  fi

  status_body="$(curl -sS -H "Host: ${first_host}" "$status_url" 2>/dev/null || true)"
  active_now="$(jq -r '.active // false' <<<"${status_body}" 2>/dev/null || true)"
  if [[ "$active_now" == "true" ]]; then
    echo "  search oracle active (epoch $(jq -r '.epoch' <<<"${status_body}"))"
    active=1
    break
  fi
  sleep 1
done
if (( active == 0 )); then
  echo "search oracle did not become active within ${ACTIVATION_TIMEOUT}s" >&2
  exit 1
fi

register_node() {
  local index="$1"
  local node_id="$2"
  local port="$3"
  local node_label="${node_id#Node-}"
  local host="${LyquidLabel}.${node_label}"
  local url="http://127.0.0.1:${port}/nodes/register"
  local body_file
  body_file="$(mktemp)"

  local deadline=$((SECONDS + REGISTER_TIMEOUT))
  local status="000"
  while (( SECONDS < deadline )); do
    status="$(
      curl -sS -o "$body_file" -w '%{http_code}' \
        -X POST -H "Host: ${host}" -H 'content-length: 0' "$url" 2>/dev/null || true
    )"
    if [[ "$status" == "200" || "$status" == "202" ]]; then
      echo "node${index} (${node_id}): registered (HTTP ${status})"
      rm -f "$body_file"
      return 0
    fi
    sleep 1
  done

  echo "node${index} (${node_id}): registration failed (HTTP ${status})" >&2
  if [[ -s "$body_file" ]]; then
    sed 's/^/  /' "$body_file" >&2
  fi
  rm -f "$body_file"
  return 1
}

failed=0
while IFS=$'\t' read -r index node_id port; do
  register_node "$index" "$node_id" "$port" || failed=1
done < <(jq -r '.nodes[] | [.index, .node_id, .port] | @tsv' "$NODES_JSON")

if (( failed != 0 )); then
  echo "one or more nodes failed to self-register" >&2
  exit 1
fi

# Wait for the certified registration calls to be executed and visible in the
# shared NodeRegistry before returning.
expected_nodes="$(jq -r '[.nodes[].node_id] | sort | join(",")' "$NODES_JSON")"
list_url="http://127.0.0.1:${first_port}/nodes/list"
deadline=$((SECONDS + REGISTER_TIMEOUT))
while (( SECONDS < deadline )); do
  actual_nodes="$(
    curl -sS -H "Host: ${first_host}" "$list_url" 2>/dev/null |
      jq -r '(.nodes // []) | sort | join(",")' 2>/dev/null || true
  )"
  if [[ "$actual_nodes" == "$expected_nodes" ]]; then
    echo "NodeRegistry contains all registered nodes."
    echo "Search oracle active and all nodes self-registered."
    exit 0
  fi
  sleep 1
done

echo "NodeRegistry did not contain all nodes before the timeout." >&2
echo "  expected: $expected_nodes" >&2
echo "  actual:   ${actual_nodes:-<none>}" >&2
exit 1
