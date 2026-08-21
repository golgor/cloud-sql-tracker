#!/usr/bin/env bash
# Layer 1 of issue #23: golden examples vs JSON Schemas.
# This script is the pair registry. Call it from mise, hk pre-push, and CI.
# Do not add logs.v1.txt (plain text). Do not close #23 on this check alone.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

if ! command -v check-jsonschema >/dev/null 2>&1; then
  echo "check-jsonschema not on PATH. Run: mise install" >&2
  exit 1
fi

check() {
  local schema="$1"
  local instance="$2"
  echo "validate '${instance}' against '${schema}'"
  check-jsonschema --schemafile "${schema}" "${instance}"
}

check schemas/status.v1.json examples/status.v1.json
check schemas/config.v1.json examples/connections.json
check schemas/doctor.v1.json examples/doctor.v1.json
