#!/bin/bash
# set-host-abe.sh <host> <abe-url>: point a host record's abe at <abe-url>
# on the leader (GET, edit, PUT). Needs XDEBBIE_URL/TOKEN/CA
# (source /etc/xdebbie/xdebbie_env). Until bootstrap --join carries abe,
# this is how a box's abe gets into the leader's store.
set -euo pipefail
host=${1:?host name}; url=${2:?abe url}
: "${XDEBBIE_URL:?}" "${XDEBBIE_TOKEN:?}" "${XDEBBIE_CA:?}"
h="$XDEBBIE_URL/v1/hosts/$host"
auth="Authorization: Bearer $XDEBBIE_TOKEN"
curl -sf --cacert "$XDEBBIE_CA" -H "$auth" "$h" \
  | jq --arg u "$url" '.abe = $u' \
  | curl -sf --cacert "$XDEBBIE_CA" -H "$auth" -H 'Content-Type: application/json' -X PUT "$h" -d @- \
  | jq -r '"\(.name): abe = \(.abe)"'
