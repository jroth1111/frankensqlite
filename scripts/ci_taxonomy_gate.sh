#!/usr/bin/env bash
set -euo pipefail

# bd-zywqc.13: CI taxonomy gate — verifies fix beads under bd-zywqc
# include all 9 test taxonomy categories (T1..T9) in their close reason.
#
# Usage:
#   ./scripts/ci_taxonomy_gate.sh [--verbose]
#
# Exit codes:
#   0  All closed fix beads pass the gate
#   1  At least one bead is missing taxonomy references
#   2  Script error

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
VERBOSE=false

for arg in "$@"; do
    case "$arg" in
        --verbose) VERBOSE=true ;;
        *) echo "Unknown option: $arg" >&2; exit 2 ;;
    esac
done

BEADS_FILE="$REPO_ROOT/.beads/issues.jsonl"
if [[ ! -f "$BEADS_FILE" ]]; then
    echo "[taxonomy-gate] No .beads/issues.jsonl found — nothing to check."
    exit 0
fi

INFRA_BEADS="bd-zywqc.1,bd-zywqc.6,bd-zywqc.13,bd-073kf,bd-bpnnx"

python3 - "$BEADS_FILE" "$VERBOSE" "$INFRA_BEADS" <<'PYEOF'
import json, sys, re

beads_file = sys.argv[1]
verbose = sys.argv[2] == "True"
infra_set = set(sys.argv[3].split(","))

REQUIRED_TAGS = ["T1", "T2", "T3", "T4", "T5", "T6", "T7", "T8", "T9"]
checked = passed = failed = grandfathered = 0
failures = []

with open(beads_file) as f:
    for line in f:
        line = line.strip()
        if not line:
            continue
        try:
            d = json.loads(line)
        except json.JSONDecodeError:
            continue

        bead_id = d.get("id", "")
        status = d.get("status", "")
        bead_type = d.get("type", "")
        close_reason = d.get("close_reason", "")
        deps = d.get("deps", [])
        parents = [x.get("target", "") for x in deps if x.get("kind") == "parent-child"]
        parent = parents[0] if parents else ""

        if status != "closed":
            continue
        if bead_type not in ("task", "bug"):
            continue
        if parent != "bd-zywqc":
            continue
        if bead_id in infra_set:
            if verbose:
                print(f"[taxonomy-gate] SKIP {bead_id} (infrastructure)")
            continue

        checked += 1
        missing = []
        for tag in REQUIRED_TAGS:
            pattern = rf"(^|[^A-Z0-9]){tag}:"
            exempt_pattern = rf"{tag}:.*EXEMPT"
            if re.search(pattern, close_reason):
                continue
            elif re.search(exempt_pattern, close_reason, re.IGNORECASE):
                continue
            else:
                missing.append(tag)

        if not missing:
            passed += 1
            if verbose:
                print(f"[taxonomy-gate] PASS {bead_id}")
        elif "grandfathered" in close_reason.lower():
            grandfathered += 1
            if verbose:
                print(f"[taxonomy-gate] GRANDFATHERED {bead_id}")
        else:
            failed += 1
            failures.append((bead_id, missing))
            print(f"[taxonomy-gate] FAIL {bead_id} — missing: {' '.join(missing)}")
            if verbose:
                print(f"  close_reason: {close_reason[:200]}")

print()
print(f"[taxonomy-gate] Summary: checked={checked} passed={passed} failed={failed} grandfathered={grandfathered}")

if failed > 0:
    print(f"[taxonomy-gate] FAILED — {failed} bead(s) missing taxonomy references")
    sys.exit(1)
else:
    print("[taxonomy-gate] PASSED")
    sys.exit(0)
PYEOF
