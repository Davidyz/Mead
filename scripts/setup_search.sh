#!/usr/bin/env bash
# Activate the search oracle and let every devnet node self-register.
#
# The Lyquid constructor only *stages* the search oracle committee. This script
# performs the node-side follow-up that the constructor cannot do:
#   1. resolve the deployed Lyquid contract and its LVM oracle target,
#   2. advance and finalize the first search-oracle epoch,
#   3. POST /nodes/register on each node so it submits a certified
#      register_node(node_id) call for its own identity.
#
# It deliberately performs no bartender ed25519/qx/qy mappings and does not
# hardcode node keys or coordinates. It is safe to run repeatedly: oracle epoch
# operations are no-ops once active and node registration is idempotent.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

NODES_JSON="${NODES_JSON:-tests/fixture/nodes.json}"
RPC="${RPC:-http://127.0.0.1:10087/api}"
REGISTER_TIMEOUT="${REGISTER_TIMEOUT:-60}"

if [[ ! -f "$NODES_JSON" ]]; then
  echo "missing $NODES_JSON; run scripts/prep_nodes.py first" >&2
  exit 1
fi

for command in jq curl cast shaker; do
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

Contract="$(
  shaker list -o json |
    jq -r --arg id "$Lyquid" '.lyquids[] | select(.lyquid_id == $id) | .contract' |
    head -n 1
)"
if [[ -z "$Contract" || "$Contract" == "null" ]]; then
  echo "could not resolve the sequencing contract for $Lyquid" >&2
  exit 1
fi

Target="$(shaker to-hex "$Lyquid")"
echo "Lyquid:  $Lyquid"
echo "Contract: $Contract"
echo "Target:  $Target"

# These are instance/eth-exported oracle helpers. They submit the staged
# committee to the target and then finalize it on the source, which activates
# the first search oracle epoch.
echo "Advancing search oracle epoch..."
cast call --rpc-url "$RPC" "$Contract" \
  '__lyquor_oracle_advance_epoch(string,address,bool)' search "$Target" false >/dev/null
echo "Finalizing search oracle epoch..."
cast call --rpc-url "$RPC" "$Contract" \
  '__lyquor_oracle_finalize_epoch(string,address,bool)' search "$Target" false >/dev/null

# Give the nodes a moment to observe the finalized epoch before self-registering.
sleep "${EPOCH_SETTLE_SECONDS:-2}"

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
first_host="$(jq -r --arg label "$LyquidLabel" '.nodes[0] | "\($label).\(.node_id | sub("^Node-"; ""))"' "$NODES_JSON")"
first_port="$(jq -r '.nodes[0].port' "$NODES_JSON")"
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
