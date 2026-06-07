#!/bin/bash
# Bump version across all config files.
# Usage: ./scripts/bump-version.sh 0.1.13

set -e

if [ -z "$1" ]; then
    echo "Usage: $0 <new-version>"
    echo "Example: $0 0.1.13"
    exit 1
fi

NEW_VERSION="$1"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"

echo "Bumping version to $NEW_VERSION..."

# 1. package.json (single source of truth)
cd "$ROOT"
sed -i '' "s/\"version\": \".*\"/\"version\": \"$NEW_VERSION\"/" package.json

# 2. rust-core/Cargo.toml
sed -i '' "s/^version = \".*\"/version = \"$NEW_VERSION\"/" rust-core/Cargo.toml

# 3. Formula/codeseek.rb
sed -i '' "s/version \".*\"/version \"$NEW_VERSION\"/" Formula/codeseek.rb
sed -i '' "s|download/v[0-9.]*/codeseek-|download/v$NEW_VERSION/codeseek-|g" Formula/codeseek.rb

# 4. packagelock.json
cd "$ROOT"
npm install --package-lock-only 2>/dev/null || true

echo ""
echo "Done! Version bumped to $NEW_VERSION in:"
echo "  package.json          → $NEW_VERSION  (↑ source of truth)"
echo "  rust-core/Cargo.toml  → $NEW_VERSION"
echo "  Formula/codeseek.rb   → $NEW_VERSION"
echo ""
echo "Next steps:"
echo "  git add package.json package-lock.json rust-core/Cargo.toml Formula/codeseek.rb"
echo "  git commit -m 'chore: bump version to $NEW_VERSION'"
echo "  git tag v$NEW_VERSION && git push origin v$NEW_VERSION"
