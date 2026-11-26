#!/usr/bin/env bash
set -euo pipefail

assets_dir="$1"
tag="${RELEASE_TAG:?}"
image="AppDepot-${tag#v}-x86_64.AppImage"
setup="AppDepot-Desktop-Windows-x64-Setup.exe"
names=("$image" "$image.sha256" "$image.zsync" "$setup" SHA256SUMS)

verify_tag() {
  test "$(git ls-remote origin "refs/tags/$tag" | cut -f1)" = "${RELEASE_TAG_OBJECT:?}"
  test "$(git rev-parse HEAD)" = "${APP_DEPOT_SOURCE_REVISION:?}"
}

verify_latest_version() {
  local latest
  latest="$(gh api "repos/$GITHUB_REPOSITORY/releases/latest" --jq '.tag_name')"
  if [[ "$latest" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] &&
      test "$(printf '%s\n' "$latest" "$tag" | sort -V | tail -1)" != "$tag"; then
    echo "Latest already points to the newer Desktop release $latest." >&2
    exit 1
  fi
}

verify_tag
verify_latest_version
for name in "${names[@]}"; do
  test -s "$assets_dir/$name"
done
(
  cd "$assets_dir"
  for pair in "$image.sha256:$image" "SHA256SUMS:$setup"; do
    manifest="${pair%%:*}"
    target="${pair#*:}"
    expected="$(sha256sum "$target")"
    test "$(tr -d '\r' < "$manifest")" = "$expected"
  done
)

if gh release view "$tag" --json tagName >/dev/null 2>&1; then
  if test "$RESUME_DRAFT" != true && test "${GITHUB_RUN_ATTEMPT:-1}" -le 1; then
    echo "Release $tag already exists. Inspect it before requesting draft recovery." >&2
    exit 1
  fi
  gh release view "$tag" --json isDraft,isPrerelease,tagName \
    --jq '.isDraft and (.isPrerelease | not) and (.tagName == "'"$tag"'")' | grep -Fx true
else
  test "$RESUME_DRAFT" != true
  gh release create "$tag" --verify-tag --draft --latest=false \
    --title "App Depot Desktop $tag" --notes-file "docs/release-notes/$tag.md"
fi

downloaded="$(mktemp -d)"
trap 'rm -r -- "$downloaded"' EXIT
before="$(gh release view "$tag" --json assets --jq '.assets[].name')"
while IFS= read -r name; do
  test -n "$name" || continue
  printf '%s\n' "${names[@]}" | grep -Fx -- "$name" >/dev/null
  gh release download "$tag" --pattern "$name" --dir "$downloaded"
  if ! cmp -s "$assets_dir/$name" "$downloaded/$name"; then
    echo "Existing draft asset differs from this verified build: $name. Draft was preserved." >&2
    exit 1
  fi
done <<< "$before"

for name in "${names[@]}"; do
  if ! grep -Fxq -- "$name" <<< "$before"; then
    gh release upload "$tag" "$assets_dir/$name"
    gh release download "$tag" --pattern "$name" --dir "$downloaded"
  fi
  cmp "$assets_dir/$name" "$downloaded/$name"
  expected="sha256:$(sha256sum "$assets_dir/$name" | cut -d ' ' -f1)"
  observed="$(gh release view "$tag" --json assets --jq '.assets[] | select(.name == "'"$name"'") | .digest')"
  test "$observed" = "$expected"
done

expected_names="$(printf '%s\n' "${names[@]}" | LC_ALL=C sort)"
actual_names="$(gh release view "$tag" --json assets --jq '.assets[].name' | LC_ALL=C sort)"
test "$actual_names" = "$expected_names"
verify_tag
verify_latest_version
gh release view "$tag" --json isDraft,isPrerelease --jq '.isDraft and (.isPrerelease | not)' | grep -Fx true
gh release edit "$tag" --draft=false --prerelease=false --latest=true
test "$(gh api "repos/$GITHUB_REPOSITORY/releases/latest" --jq '.tag_name')" = "$tag"
gh release view "$tag" --json isDraft,isPrerelease,tagName \
  --jq '(.isDraft | not) and (.isPrerelease | not) and (.tagName == "'"$tag"'")' | grep -Fx true
echo "Published verified complete Desktop release $tag at $APP_DEPOT_SOURCE_REVISION."
