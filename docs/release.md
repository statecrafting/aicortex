# Releasing the library crates

Spec 055 owns this procedure. A release publishes five library crates to
crates.io at one version. Publication is not a deployment, not an image,
not a reference-deployment qualification, and not a claim of production
readiness; a release note says so every time.

## What publishes

In dependency order, which is also the order they publish in:

1. `aicortex-types`
2. `aicortex-claims`
3. `aicortex-gate`
4. `aicortex-store`
5. `aicortex-embed`

All five share `[workspace.package] version`, and every requirement between
them is exact (`=x.y.z`). `apps/aicortex`, every crate under `fixtures/`,
and every other library keep `publish = false`. A crate joins the set only
through an amendment of spec 055; `scripts/release-check.sh` refuses any
other change to it.

## Who decides

A human cuts the release. No workflow, schedule, or agent session creates or
pushes the tag. Merging a pull request never publishes anything.

## The release check

`scripts/release-check.sh` runs on every pull request that touches one of
the five crates, the external host fixture, the workspace manifest or lock,
or the release tooling. It:

1. checks the manifests: the five crates are publishable with a
   description, license, repository, and readme, share the workspace
   version, and depend on each other exactly and in order; everything else
   keeps `publish = false`;
2. packages the five crates with `cargo package --locked`, which builds each
   package against the ones before it, and refuses a package that omits a
   file its library reads at compile time or reads from `fixtures/`,
   `data/`, or `.statecraft/derived/`;
3. builds a consumer outside the workspace whose only source is a registry:
   the locked tree vendored with the five packages unpacked into it as
   crates.io would serve them, replacing crates.io, with no path, git, or
   `[patch]` source. It runs the spec 053 external host proof against them.

`scripts/release-check.sh --self-test` proves the refusals: a lightweight
tag, a tag off `main`, a version mismatch, a missing changelog section, a
malformed tag, a compile-time read into `fixtures/`, an omitted
compile-time input, and a store package whose migrations are left out.

Run both locally before a release pull request:

```sh
scripts/release-check.sh
scripts/release-check.sh --self-test
```

## One-time setup (owner)

1. On crates.io, create an API token scoped to `publish-new` and
   `publish-update` for the five crate names.
2. In the repository settings, create the environment `release`. Add
   required reviewers (a human), restrict deployment to tags matching `v*`,
   and add the token as the environment secret `CARGO_REGISTRY_TOKEN`. Do
   not add it as a repository or organization secret: only the `release`
   environment may hold it, so no pull-request run can read it.

## Cutting a release

1. **Prepare the release pull request.** On a branch from `main`:
   - set `[workspace.package] version` and the five `aicortex-*` entries
     of `[workspace.dependencies]` to the new version (`=x.y.z`);
   - add a `CHANGELOG.md` section headed `## x.y.z` (format below);
   - run the release check and the gate, then open the pull request.

   The version bump touches `Cargo.toml` and the changelog does not, so the
   policy-isolation guard requires two pull requests: the manifest first,
   then the changelog. The tag goes on the commit that carries both.
2. **Merge.** Nothing publishes.
3. **Tag.** Check out the merged commit on `main` and create a signed,
   annotated tag, then push it:

   ```sh
   git switch main && git pull --ff-only
   git tag -s -a vX.Y.Z -m "aicortex library crates X.Y.Z"
   git push origin vX.Y.Z
   ```

4. **Approve.** The `release` workflow guards the tag (annotated, on
   `main`, matching the workspace version, with a changelog section that
   carries the non-assertion), reruns the release check, and waits for
   approval of the `release` environment. Approving it releases the token
   to the publish job.
5. **Record.** After the crates are on crates.io, add the tag's commit and
   the published versions to the release's changelog section in a pull
   request. The tagged commit cannot name itself, so this record follows
   the publication.

## A partial release

A crates.io version can be neither unpublished nor reused. The publish job
publishes in order and stops at the first failure, listing what published
and what did not. To recover:

- **Finish it.** Fix the cause outside the crates (a token, a network
  failure) and rerun the failed workflow at the same tag. A version already
  on crates.io is skipped.
- **Or replace it.** Yank what published (`cargo yank --version x.y.z
  <crate>`), then release the next version through the whole procedure.

Never move or recreate a tag.

## Changelog section format

```markdown
## x.y.z

- Commit: the tagged commit, added after publication.
- Rahi: the chassis version pinned in `[workspace.dependencies]`.
- Specs: the specs whose implementation this release carries.
- Breaking changes: what breaks, or "none". Before 1.0.0 any release may
  break the API, and this line says whether this one did.
- Known limitations: what the crates do not yet do.

This release is a publication of library crates and asserts no deployment,
image, reference-deployment qualification, or production readiness.
```

The guard refuses a tag whose section is missing or omits the final
sentence.
