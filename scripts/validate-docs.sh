#!/usr/bin/env bash
set -e

echo "🔍 Validating RTK documentation consistency..."

# 1. Source file count sanity check
SRC_FILES=$(find src -name "*.rs" ! -name "mod.rs" ! -name "main.rs" | wc -l | tr -d ' ')
echo "📊 Rust source files in src/: $SRC_FILES"

# 3. Commandes Python/Go présentes partout
PYTHON_GO_CMDS=("ruff" "pytest" "pip" "go" "golangci")
echo "🐍 Checking Python/Go commands documentation..."

for cmd in "${PYTHON_GO_CMDS[@]}"; do
  if [ ! -f "README.md" ]; then
    echo "⚠️  README.md not found, skipping"
    break
  fi
  if ! grep -q "$cmd" "README.md"; then
    echo "❌ README.md ne mentionne pas commande $cmd"
    exit 1
  fi
done
echo "✅ Python/Go commands: documented in README.md"

# 4. Native dispatcher owns hook routing; legacy shell payloads are forbidden.
if [ -e ".claude/hooks/rtk-rewrite.sh" ]; then
  echo "❌ Retired Claude shell hook is present"
  exit 1
fi
grep -q 'rtk hook claude' docs/contributing/TECHNICAL.md
echo "✅ Native RTK dispatcher documented; no legacy shell hook"

echo ""
echo "✅ Documentation validation passed"
