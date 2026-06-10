#!/bin/bash
# Bump version, commit, tag, and push — one command release.
# Usage: ./scripts/bump-version.sh 0.1.18

set -e

# Detect OS for sed in-place compatibility
if [[ "$OSTYPE" == "darwin"* ]]; then
    SED_INPLACE=(-i '')
else
    SED_INPLACE=(-i)
fi

if [ -z "$1" ]; then
    echo "Usage: $0 <new-version>"
    echo "Example: $0 0.1.18"
    exit 1
fi

NEW_VERSION="$1"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"

echo "==> Bumping version to $NEW_VERSION..."

cd "$ROOT"

# 1. package.json (single source of truth)
sed "${SED_INPLACE[@]}" "s/\"version\": \".*\"/\"version\": \"$NEW_VERSION\"/" package.json

# 2. rust-core/Cargo.toml
sed "${SED_INPLACE[@]}" "s/^version = \".*\"/version = \"$NEW_VERSION\"/" rust-core/Cargo.toml

# 3. Formula/codeseek.rb
sed "${SED_INPLACE[@]}" "s/version \".*\"/version \"$NEW_VERSION\"/" Formula/codeseek.rb
sed "${SED_INPLACE[@]}" "s|download/v[0-9.]*/codeseek-|download/v$NEW_VERSION/codeseek-|g" Formula/codeseek.rb

# 4. package-lock.json
npm install --package-lock-only 2>/dev/null || true

# 5. Commit
git add package.json package-lock.json rust-core/Cargo.toml Formula/codeseek.rb
git commit -m "chore: bump version to $NEW_VERSION" 2>/dev/null || true

# 6. Push & tag
echo ""
echo "==> Pushing and tagging v$NEW_VERSION..."
git push
git tag "v$NEW_VERSION"
git push origin "v$NEW_VERSION"

echo ""
echo "==> Done! v$NEW_VERSION released."
echo "    CI: https://github.com/CodeBendKit/codeseek/actions"
