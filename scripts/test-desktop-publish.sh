#!/usr/bin/env bash
set -euo pipefail

script="$(cd "$(dirname "$0")" && pwd)/publish-desktop-release.sh"
fixture="$(mktemp -d)"
trap 'rm -r -- "$fixture"' EXIT
mkdir "$fixture/bin" "$fixture/assets" "$fixture/draft" "$fixture/docs" "$fixture/docs/release-notes"
cd "$fixture"
export PATH="$fixture/bin:$PATH"
export RELEASE_TAG=v0.1.4 RELEASE_TAG_OBJECT=annotated APP_DEPOT_SOURCE_REVISION=revision
export GITHUB_REPOSITORY=example/fixture RESUME_DRAFT=false

cat >bin/git <<'EOF'
#!/usr/bin/env bash
case "$1" in
  ls-remote) printf 'annotated\trefs/tags/v0.1.4\n' ;;
  rev-parse) printf 'revision\n' ;;
  *) exit 2 ;;
esac
EOF

cat >bin/gh <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
if test "$1" = api; then
  if test -e published; then echo v0.1.4; else echo v0.1.3; fi
  exit
fi
test "$1" = release
test "$3" = v0.1.4
case "$2" in
  create) test ! -e created; touch created ;;
  upload) test ! -e "draft/$(basename "$4")"; cp "$4" draft/ ;;
  download) test "$4" = --pattern; test "$6" = --dir; cp "draft/$5" "$7/" ;;
  edit) touch published; exit 1 ;;
  view)
    test -e created
    query="${!#}"
    case "$query" in
      '.assets[].name') find draft -type f -printf '%f\n' ;;
      *'.digest')
        name="$(printf '%s\n' "$query" | cut -d '"' -f2)"
        printf 'sha256:%s\n' "$(sha256sum "draft/$name" | cut -d ' ' -f1)" ;;
      *) echo true ;;
    esac ;;
  *) exit 2 ;;
esac
EOF

chmod +x bin/git bin/gh
touch docs/release-notes/v0.1.4.md
image=AppDepot-0.1.4-x86_64.AppImage
setup=AppDepot-Desktop-Windows-x64-Setup.exe
printf 'Linux fixture' > "assets/$image"
printf 'zsync fixture' > "assets/$image.zsync"
(cd assets && sha256sum "$image" > "$image.sha256")
if bash "$script" assets; then
  echo 'Publication accepted a missing Windows package' >&2
  exit 1
fi
test ! -e created
printf 'Windows fixture' > "assets/$setup"
(cd assets && sha256sum "$setup" > SHA256SUMS)

# Resume a draft whose one existing upload matches the artifacts.
touch created
export RESUME_DRAFT=true
printf 'foreign bytes' > "draft/$image"
if bash "$script" assets; then
  echo 'Publication accepted a conflicting draft asset' >&2
  exit 1
fi
test ! -e published
test "$(find draft -type f | wc -l)" -eq 1
cp "assets/$image" "draft/$image"
bash "$script" assets
test ! -e published
test "$(find draft -type f | wc -l)" -eq 5

# A first run creates a new draft only after the same complete asset checks.
mv draft resumed-draft
mv created resumed-created
mkdir draft
export RESUME_DRAFT=false
bash "$script" assets
test -e created
test ! -e published
test "$(find draft -type f | wc -l)" -eq 5
echo 'Desktop release draft fixtures passed'
