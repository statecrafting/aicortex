#!/usr/bin/env bash
# Spec: specs/055-release-and-crate-publication/spec.md
#
# The release check, the tag guard, and the publish step of spec 055.
#
#   (no argument)      check the manifests (FR-004), package the publishable
#                      crates and audit what they carry (B-5), then build a
#                      registry-only consumer and run the spec 053 external
#                      host proof against the packages (B-6, FR-002)
#   --guard-tag TAG [--main-ref REF]
#                      refuse a tag that may not publish (B-1, FR-003): a
#                      lightweight tag, a commit not on REF (default
#                      origin/main), a version that differs from the
#                      workspace version at the tag, or a release with no
#                      CHANGELOG.md section (B-8)
#   --publish          publish the crates in dependency order and stop at the
#                      first failure, skipping a version already on
#                      crates.io (B-4); the release workflow runs it only
#                      after --guard-tag passed
#   --self-test        exercise the negative cases of FR-001 and FR-003
#   --allow-dirty      with the check: package uncommitted changes too. Off by
#                      default, so a local pass judges the committed tree CI
#                      judges; cargo refuses a dirty tree without it
#
# Exit 0 ok, 1 finding or refusal, 3 usage, 4 execution failure.

if [ -z "${BASH_VERSION:-}" ]; then
  echo "release-check.sh requires bash; run it through its executable shebang" >&2
  exit 3
fi
set -uo pipefail

# B-2: the publishable set, in dependency order. Publishing walks this list
# front to back, and the manifest check refuses an order a crate's normal
# dependencies contradict. A crate joins only through an amendment of 055.
CRATES=(aicortex-types aicortex-claims aicortex-gate aicortex-store aicortex-index aicortex-embed)

# The external host fixture of spec 053, whose proof the consumer reruns.
FIXTURE=fixtures/external-host
# The one file the fixture's test reads from the workspace by relative path,
# and the spelling it is read with. The consumer copies it beside the test.
REGISTRY_DOC=crates/aicortex-claims/testdata/registries/travel-v1.json
REGISTRY_INCLUDE='"../../../crates/aicortex-claims/testdata/registries/travel-v1.json"'

# I-3: the sentence every release section of CHANGELOG.md carries.
NON_ASSERTION='asserts no deployment, image, reference-deployment qualification, or production readiness'

usage() {
  echo "usage: release-check.sh [--allow-dirty] [--guard-tag TAG [--main-ref REF]] [--publish] [--self-test]" >&2
  exit 3
}
fail() {
  echo "release-check: $*" >&2
  exit 1
}
broke() {
  echo "release-check: $*" >&2
  exit 4
}

root=$(git rev-parse --show-toplevel 2>/dev/null) || broke "not inside a git work tree"
cd "$root" || broke "cannot enter $root"

command -v cargo > /dev/null || broke "cargo is not on PATH"
command -v python3 > /dev/null || broke "python3 is not on PATH"

target_dir() {
  cargo metadata --format-version 1 --no-deps --locked \
    | python3 -I -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])'
}

workspace_version() {
  # $1: a Cargo.toml path.
  python3 -I - "$1" << 'PY'
import sys, tomllib
data = tomllib.load(open(sys.argv[1], "rb"))
version = data.get("workspace", {}).get("package", {}).get("version")
if not isinstance(version, str):
    sys.exit("no [workspace.package] version")
print(version)
PY
}

# FR-004 and B-3: the publishable set is publishable and nothing else is.
check_manifests() {
  cargo metadata --format-version 1 --no-deps --locked > "$work/metadata.json" \
    || broke "cargo metadata failed"
  python3 -I - "$work/metadata.json" "$root/Cargo.toml" "${CRATES[@]}" << 'PY'
import json, os, sys, tomllib

meta = json.load(open(sys.argv[1]))
root_manifest = tomllib.load(open(sys.argv[2], "rb"))
order = sys.argv[3:]
version = root_manifest["workspace"]["package"].get("version")
problems = []
if not isinstance(version, str):
    problems.append("[workspace.package] carries no version (B-3)")

workspace_deps = root_manifest["workspace"].get("dependencies", {})
for name in order:
    spec = workspace_deps.get(name)
    req = spec.get("version") if isinstance(spec, dict) else spec
    if req != f"={version}":
        problems.append(f"[workspace.dependencies] {name} requires {req!r}, not '={version}' (B-3)")

packages = {p["name"]: p for p in meta["packages"]}
for name in order:
    if name not in packages:
        problems.append(f"{name} is not a workspace member")
for name, pkg in sorted(packages.items()):
    if name in order:
        if pkg.get("publish") is not None:
            problems.append(f"{name} must be publishable to crates.io (B-2); publish is {pkg['publish']!r}")
        if pkg["version"] != version:
            problems.append(f"{name} is {pkg['version']}, not the workspace version {version} (B-3)")
        for field in ("description", "license", "repository", "readme"):
            if not pkg.get(field):
                problems.append(f"{name} sets no {field} (FR-004)")
        readme = pkg.get("readme")
        if readme and not os.path.isfile(os.path.join(os.path.dirname(pkg["manifest_path"]), readme)):
            problems.append(f"{name} names a readme that does not exist: {readme}")
        for dep in pkg["dependencies"]:
            if not dep["name"].startswith("aicortex-"):
                continue
            if dep["kind"] == "dev":
                # Stripped at publish only when it carries no version.
                if dep["req"] != "*":
                    problems.append(f"{name} dev-depends on {dep['name']} with a version; a dev edge between workspace crates is path-only, so publication strips it")
            elif dep["name"] not in order or order.index(dep["name"]) >= order.index(name):
                problems.append(f"{name} depends on {dep['name']}, which does not publish before it (B-2)")
            elif dep["req"] != f"={version}":
                problems.append(f"{name} requires {dep['name']} {dep['req']}, not '={version}' (B-3)")
    elif pkg.get("publish") != []:
        problems.append(f"{name} is outside the publishable set and must keep publish = false (I-2)")

for line in problems:
    print(f"release-check: {line}", file=sys.stderr)
sys.exit(1 if problems else 0)
PY
}

# B-5: what a package reads at compile time is inside it, and nothing points
# at fixtures/, data/, or the derived directory. $1 is the workspace root,
# then a list file per crate written by `cargo package --list`.
audit_contents() {
  python3 -I - "$@" << 'PY'
import os, re, sys

root = os.path.realpath(sys.argv[1])
forbidden = [os.path.join(root, d) for d in ("fixtures", "data", os.path.join(".statecraft", "derived"))]
literal = r'"((?:[^"\\]|\\.)*)"'
patterns = [
    re.compile(r"\binclude(?:_str|_bytes)?!\s*\(\s*" + literal),
    re.compile(r"#\s*\[\s*path\s*=\s*" + literal),
]
problems = []

def under(path, base):
    return path == base or path.startswith(base + os.sep)

for pair in sys.argv[2:]:
    crate_dir, listing = pair.split("=", 1)
    crate_dir = os.path.realpath(crate_dir)
    listed = {line.strip() for line in open(listing) if line.strip()}
    crate = os.path.basename(crate_dir)
    for entry in sorted(listed):
        on_disk = os.path.join(crate_dir, entry)
        # Cargo writes some entries itself (Cargo.toml.orig, the VCS info);
        # only a file that exists can point somewhere it should not.
        if not os.path.lexists(on_disk):
            continue
        resolved = os.path.realpath(on_disk)
        if any(under(resolved, f) for f in forbidden) or not under(resolved, crate_dir):
            problems.append(f"{crate} packages {entry}, which resolves outside the crate to {resolved} (B-5)")
    sources = []
    for top in ("src", "build.rs"):
        start = os.path.join(crate_dir, top)
        if os.path.isfile(start):
            sources.append(start)
        for dirpath, _, files in os.walk(start):
            sources.extend(os.path.join(dirpath, f) for f in files if f.endswith(".rs"))
    for source in sources:
        text = open(source, encoding="utf-8").read()
        for pattern in patterns:
            for match in pattern.finditer(text):
                target = os.path.realpath(os.path.join(os.path.dirname(source), match.group(1)))
                where = os.path.relpath(source, crate_dir)
                if any(under(target, f) for f in forbidden):
                    problems.append(f"{crate} {where} reads {match.group(1)}, a path into fixtures/, data/, or the derived directory (B-5)")
                elif not under(target, crate_dir):
                    problems.append(f"{crate} {where} reads {match.group(1)}, which is outside the crate and so outside its package (B-5)")
                elif os.path.relpath(target, crate_dir) not in listed:
                    problems.append(f"{crate} {where} reads {match.group(1)}, which its package omits (B-5)")

for line in problems:
    print(f"release-check: {line}", file=sys.stderr)
sys.exit(1 if problems else 0)
PY
}

# B-5: package every crate (cargo verifies each against the ones before it)
# and audit the lists. $1 is the workspace root to package, then any extra
# `cargo package` flags (`--allow-dirty`).
package_crates() {
  local ws=$1
  shift
  local args=() pairs=() crate
  for crate in "${CRATES[@]}"; do args+=(-p "$crate"); done
  (cd "$ws" && cargo package --locked "$@" "${args[@]}") || return 1
  for crate in "${CRATES[@]}"; do
    (cd "$ws" && cargo package --locked "$@" --list -p "$crate") > "$work/$crate.list" \
      || return 1
    pairs+=("$ws/crates/$crate=$work/$crate.list")
  done
  audit_contents "$ws" "${pairs[@]}"
}

# B-6: a consumer outside the workspace whose only source is a registry. The
# vendored copy of the locked tree stands in for crates.io, the six
# packages are unpacked into it as crates.io would serve them, and the
# consumer names every dependency by version alone.
build_consumer() {
  local version=$1 vendor="$work/vendor" consumer="$work/consumer" crate sum
  cargo vendor --locked --quiet "$vendor" > /dev/null || broke "cargo vendor failed"
  for crate in "${CRATES[@]}"; do
    local packed="$tdir/package/$crate-$version.crate"
    [ -f "$packed" ] || broke "no package at $packed"
    rm -rf "${vendor:?}/$crate" "${vendor:?}/$crate-$version"
    tar -xzf "$packed" -C "$vendor" || broke "cannot unpack $packed"
    mv "$vendor/$crate-$version" "$vendor/$crate" || broke "cannot place $crate"
    sum=$(python3 -I -c 'import hashlib, sys; print(hashlib.sha256(open(sys.argv[1], "rb").read()).hexdigest())' "$packed")
    printf '{"files":{},"package":"%s"}\n' "$sum" > "$vendor/$crate/.cargo-checksum.json"
  done

  rm -rf "$consumer"
  mkdir -p "$consumer/.cargo" || broke "cannot create $consumer"
  cp -R "$FIXTURE/src" "$FIXTURE/tests" "$consumer/" || broke "cannot copy $FIXTURE"
  cp rust-toolchain.toml "$consumer/" || broke "cannot copy the toolchain pin"
  cp "$REGISTRY_DOC" "$consumer/tests/travel-v1.json" || broke "cannot copy $REGISTRY_DOC"
  python3 -I - "$consumer/tests/atomic.rs" "$REGISTRY_INCLUDE" << 'PY' || fail "the fixture no longer reads $REGISTRY_DOC as expected; update REGISTRY_INCLUDE"
import sys
path, needle = sys.argv[1], sys.argv[2]
text = open(path).read()
if text.count(needle) != 1:
    sys.exit(1)
open(path, "w").write(text.replace(needle, '"travel-v1.json"'))
PY
  cat > "$consumer/.cargo/config.toml" << EOF
# Written by scripts/release-check.sh (spec 055 B-6): crates.io is replaced
# by the locked tree plus the packages under test, so nothing resolves from
# a path, a git source, or a patch.
[source.crates-io]
replace-with = "release-overlay"

[source.release-overlay]
directory = "$vendor"
EOF
  python3 -I - "$root/Cargo.toml" "$FIXTURE/Cargo.toml" "$consumer/Cargo.toml" "${CRATES[@]}" << 'PY' || broke "cannot write the consumer manifest"
import json, sys, tomllib

root = tomllib.load(open(sys.argv[1], "rb"))
fixture = tomllib.load(open(sys.argv[2], "rb"))
out = sys.argv[3]
workspace = root["workspace"]
deps = workspace["dependencies"]
package = workspace["package"]

def render(name, local):
    spec = deps.get(name) if isinstance(local, dict) and local.get("workspace") else local
    if isinstance(spec, str):
        spec = {"version": spec}
    spec = dict(spec)
    for key in ("path", "git", "branch", "tag", "rev", "workspace"):
        spec.pop(key, None)
    if isinstance(local, dict):
        extra = local.get("features", [])
        if extra:
            spec["features"] = sorted(set(spec.get("features", [])) | set(extra))
        if "optional" in local:
            spec["optional"] = local["optional"]
    fields = ", ".join(f"{k} = {json.dumps(v)}" for k, v in spec.items())
    return f"{name} = {{ {fields} }}"

lines = [
    "# Written by scripts/release-check.sh (spec 055 B-6). Not a workspace member.",
    "[package]",
    'name = "aicortex-release-consumer"',
    'version = "0.0.0"',
    f'edition = {json.dumps(package["edition"])}',
    f'rust-version = {json.dumps(package["rust-version"])}',
    "publish = false",
    "",
    "[lib]",
    'name = "aicortex_external_host_fixture"',
    "",
    "[workspace]",
]
for table in ("dependencies", "dev-dependencies"):
    lines += ["", f"[{table}]"]
    for name, local in fixture.get(table, {}).items():
        lines.append(render(name, local))
open(out, "w").write("\n".join(lines) + "\n")
PY
  if grep -nE '(^|[ ,{])(path|git)[[:space:]]*=|^\[patch' "$consumer/Cargo.toml"; then
    fail "the consumer manifest names a path, git, or patch source (B-6)"
  fi
  (cd "$consumer" && cargo generate-lockfile --offline) || broke "cannot resolve the consumer against the overlay"
  (cd "$consumer" && cargo test --locked --offline) || fail "the spec 053 external host proof failed against the packaged crates (FR-002)"
}

run_check() {
  tdir=$(target_dir) || broke "cannot read the target directory"
  work="$tdir/release-check"
  mkdir -p "$work" || broke "cannot create $work"
  local version
  version=$(workspace_version "$root/Cargo.toml") || fail "the workspace carries no version"
  echo "release-check: version $version, crates ${CRATES[*]}"
  check_manifests || fail "the manifests are not publishable as declared"
  echo "release-check: manifests ok"
  package_crates "$root" ${dirty[@]+"${dirty[@]}"} || fail "packaging refused"
  echo "release-check: packages ok"
  build_consumer "$version"
  echo "release-check: registry-only consumer ok"
}

# B-1: whether TAG may publish. $1 tag, $2 the ref that stands for main.
guard_tag() {
  local tag=$1 main_ref=$2 kind commit version="" notes
  if ! [[ $tag =~ ^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[0-9A-Za-z.-]+)?$ ]]; then
    echo "guard: $tag is not v<semver>" >&2
    return 1
  fi
  kind=$(git cat-file -t "refs/tags/$tag" 2>/dev/null) || {
    echo "guard: no tag $tag" >&2
    return 1
  }
  if [ "$kind" != tag ]; then
    echo "guard: $tag is a lightweight tag; only an annotated tag publishes" >&2
    return 1
  fi
  commit=$(git rev-parse --verify --quiet "refs/tags/$tag^{commit}") || {
    echo "guard: $tag names no commit" >&2
    return 1
  }
  git rev-parse --verify --quiet "$main_ref^{commit}" > /dev/null || {
    echo "guard: $main_ref does not resolve" >&2
    return 1
  }
  if ! git merge-base --is-ancestor "$commit" "$main_ref"; then
    echo "guard: $tag names $commit, which is not on $main_ref" >&2
    return 1
  fi
  local at
  at=$(mktemp) || return 4
  git show "$commit:Cargo.toml" > "$at" 2> /dev/null && version=$(workspace_version "$at")
  local read=$?
  rm -f "$at"
  [ "$read" -eq 0 ] || {
    echo "guard: the workspace at $commit carries no version" >&2
    return 1
  }
  if [ "v$version" != "$tag" ]; then
    echo "guard: $tag differs from the workspace version $version at $commit" >&2
    return 1
  fi
  notes=$(git show "$commit:CHANGELOG.md" 2> /dev/null | awk -v v="$version" '
    $0 ~ "^## " { inside = ($0 ~ "^## \\[?" v "\\]?([ ]|$)") ; next }
    inside { print }')
  if [ -z "$notes" ]; then
    echo "guard: CHANGELOG.md at $commit has no section for $version (B-8)" >&2
    return 1
  fi
  if ! printf '%s\n' "$notes" | tr '\n' ' ' | tr -s ' ' | grep -qF "$NON_ASSERTION"; then
    echo "guard: the $version section of CHANGELOG.md does not say it $NON_ASSERTION (I-3)" >&2
    return 1
  fi
  echo "guard: $tag is annotated, on $main_ref at $commit, and matches $version"
}

# B-4: publish front to back; a version already on crates.io is skipped, so
# a partial release is finished by running this again at the same tag.
published_on_crates_io() {
  local name=$1 version=$2 path body
  path="${name:0:2}/${name:2:2}/$name"
  body=$(curl -fsS --retry 3 "https://index.crates.io/$path" 2> /dev/null) || return 1
  printf '%s\n' "$body" | python3 -I -c '
import json, sys
want = sys.argv[1]
sys.exit(0 if any(json.loads(l).get("vers") == want for l in sys.stdin if l.strip()) else 1)' "$version"
}

run_publish() {
  local version crate done_list=() skipped=()
  version=$(workspace_version "$root/Cargo.toml") || fail "the workspace carries no version"
  [ -n "${CARGO_REGISTRY_TOKEN:-}" ] || fail "CARGO_REGISTRY_TOKEN is not set"
  for crate in "${CRATES[@]}"; do
    if published_on_crates_io "$crate" "$version"; then
      echo "publish: $crate $version is already on crates.io; skipped"
      skipped+=("$crate")
      continue
    fi
    if ! cargo publish --locked -p "$crate"; then
      echo "publish: $crate $version failed" >&2
      echo "publish: published in this run: ${done_list[*]:-none}; already present: ${skipped[*]:-none}" >&2
      echo "publish: not published: $crate and every crate after it in: ${CRATES[*]}" >&2
      echo "publish: recover by rerunning at this tag, or yank and release the next version; never retag (B-4)" >&2
      exit 1
    fi
    done_list+=("$crate")
  done
  echo "publish: $version published: ${done_list[*]:-none}; already present: ${skipped[*]:-none}"
}

# FR-001 and FR-003, negative cases.
self_test() {
  tdir=$(target_dir) || broke "cannot read the target directory"
  work="$tdir/release-check/self-test"
  rm -rf "$work"
  mkdir -p "$work" || broke "cannot create $work"
  local failures=0

  expect() {
    # $1 expected exit (0, or 1 for any refusal), $2 the text a refusal
    # must print (empty for a pass), $3 label, then the command. A refusal
    # for another reason than the one under test is a failure.
    local want=$1 needle=$2 label=$3 got
    shift 3
    "$@" > "$work/out.txt" 2>&1
    got=$?
    if { [ "$want" = 0 ] && [ "$got" = 0 ]; } \
      || { [ "$want" = 1 ] && [ "$got" = 1 ] && grep -qF -- "$needle" "$work/out.txt"; }; then
      echo "self-test: ok: $label"
    else
      echo "self-test: FAILED: $label (exit $got, wanted $want)" >&2
      sed 's/^/  | /' "$work/out.txt" >&2
      failures=$((failures + 1))
    fi
  }

  # FR-003: the tag guard against fixture refs.
  local repo="$work/tags"
  mkdir -p "$repo"
  g() { git -C "$repo" -c user.name=release-check -c user.email=release-check@example.invalid \
    -c commit.gpgsign=false -c tag.gpgsign=false "$@" > /dev/null 2>&1; }
  manifest() { printf '[workspace.package]\nversion = "%s"\n' "$1" > "$repo/Cargo.toml"; }
  notes() {
    printf '# Changelog\n\n## %s\n\nThis release is a publication of library crates and %s.\n' \
      "$1" "$NON_ASSERTION" > "$repo/CHANGELOG.md"
  }
  g init -q -b main
  manifest 0.1.0
  notes 0.1.0
  g add -A
  g commit -q -m "release 0.1.0"
  g tag -a v0.1.0 -m "v0.1.0"
  inrepo() { (cd "$repo" && guard_tag "$1" main); }
  expect 0 "" "an annotated tag on main matching the version passes" inrepo v0.1.0
  g tag -d v0.1.0
  g tag v0.1.0
  expect 1 "lightweight tag" "a lightweight tag is refused" inrepo v0.1.0
  g tag -d v0.1.0
  g switch -q -c side
  printf 'side\n' > "$repo/side.txt"
  g add -A
  g commit -q -m "off main"
  g tag -a v0.1.0 -m "v0.1.0"
  g switch -q main
  expect 1 "which is not on main" "a tag on a commit not on main is refused" inrepo v0.1.0
  g tag -d v0.1.0
  g tag -a v0.2.0 -m "v0.2.0"
  expect 1 "differs from the workspace version" "a tag whose version differs from the workspace is refused" inrepo v0.2.0
  manifest 0.3.0
  g add -A
  g commit -q -m "0.3.0 without notes"
  g tag -a v0.3.0 -m "v0.3.0"
  expect 1 "has no section for 0.3.0" "a release with no CHANGELOG.md section is refused" inrepo v0.3.0
  expect 1 "is not v<semver>" "a tag that is not v<semver> is refused" inrepo release-0.3.0

  # B-5: the static audit refuses a read into fixtures/ and an omitted file.
  local fake="$work/audit"
  mkdir -p "$fake/crates/demo/src" "$fake/fixtures"
  printf 'x\n' > "$fake/fixtures/x.txt"
  printf 'pub const X: &str = include_str!("../../../fixtures/x.txt");\n' > "$fake/crates/demo/src/lib.rs"
  printf 'Cargo.toml\nsrc/lib.rs\n' > "$work/demo.list"
  expect 1 "a path into fixtures/" "a compile-time read into fixtures/ is refused" audit_contents "$fake" "$fake/crates/demo=$work/demo.list"
  printf 'pub const X: &str = include_str!("data.sql");\n' > "$fake/crates/demo/src/lib.rs"
  printf 'select 1;\n' > "$fake/crates/demo/src/data.sql"
  expect 1 "which its package omits" "a compile-time input the package omits is refused" audit_contents "$fake" "$fake/crates/demo=$work/demo.list"
  printf 'Cargo.toml\nsrc/lib.rs\nsrc/data.sql\n' > "$work/demo.list"
  expect 0 "" "a compile-time input the package carries passes" audit_contents "$fake" "$fake/crates/demo=$work/demo.list"

  # FR-001: a copy of the tree whose store package omits its migrations.
  local copy="$work/workspace"
  mkdir -p "$copy"
  git ls-files -z --cached --others --exclude-standard \
    | (cd "$root" && xargs -0 tar -cf - 2> /dev/null) | tar -xf - -C "$copy" \
    || broke "cannot copy the workspace"
  python3 -I - "$copy/crates/aicortex-store/Cargo.toml" << 'PY' || broke "cannot edit the copy"
import sys
path = sys.argv[1]
text = open(path).read()
marker = "[package]\n"
if marker not in text:
    sys.exit(1)
open(path, "w").write(text.replace(marker, marker + 'exclude = ["src/migrations.rs"]\n', 1))
PY
  # The copy builds into the workspace's own target directory, so the self
  # test reuses the artifacts the check already built.
  # The copy is edited on purpose and may sit inside the repository's
  # ignored target directory, so it always packages as it stands.
  copied_package() { CARGO_TARGET_DIR="$tdir" package_crates "$copy" --allow-dirty; }
  expect 1 "migrations" "a package that omits a migration source is refused" copied_package

  [ "$failures" -eq 0 ] || fail "$failures self-test case(s) failed"
  echo "self-test: all cases passed"
}

mode=check
dirty=()
tag=""
main_ref=origin/main
while [ "$#" -gt 0 ]; do
  case "$1" in
    --guard-tag)
      [ "$#" -ge 2 ] || usage
      mode=guard
      tag=$2
      shift 2
      ;;
    --main-ref)
      [ "$#" -ge 2 ] || usage
      main_ref=$2
      shift 2
      ;;
    --allow-dirty)
      dirty=(--allow-dirty)
      shift
      ;;
    --publish)
      mode=publish
      shift
      ;;
    --self-test)
      mode=self-test
      shift
      ;;
    *) usage ;;
  esac
done

case "$mode" in
  check) run_check ;;
  guard) guard_tag "$tag" "$main_ref" || exit 1 ;;
  publish) run_publish ;;
  self-test) self_test ;;
esac
