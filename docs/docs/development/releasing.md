# Release container images

The repository builds release images with GitHub Actions.

The workflow runs when a maintainer pushes a tag that starts with `v`.

The workflow also supports a manual run from the Actions page.

The workflow builds these platforms:

- `linux/amd64`
- `linux/arm64`

The workflow publishes three application images to GitHub Container Registry:

| Image | Purpose |
|-------|---------|
| `ghcr.io/nathanbland/iptv-relay` | `core` and `worker` services. |
| `ghcr.io/nathanbland/iptv-relay-web` | Web management UI. |
| `ghcr.io/nathanbland/iptv-relay-gateway` | Caddy reverse proxy with baked-in configuration. |

The workflow tags each image with the release tag, the semantic version, the short SHA, and `latest`.
The `latest` tag is generated only for stable tag pushes.
A stable tag push uses a tag that starts with `v` and does not contain a hyphen.
A prerelease tag push uses a tag that contains a hyphen, such as `v1.0.0-rc.1`.
A manual run does not generate the `latest` tag.

The workflow creates an SBOM and a provenance attestation for each image.

The workflow signs each published image digest with Cosign keyless signing.
The workflow verifies each signature before the job completes.

The workflow uses least-privilege job permissions.
The `v1-gates` job requests only `contents: read`.
The image build job requests `contents: read`, `packages: write`, `id-token: write`, and `attestations: write`.

## Release steps

Run the release script from a clean and current `main` branch:

```bash
./cut_release.sh
```

The default command increments the patch version.
Use `minor`, `major`, or an exact version when necessary:

```bash
./cut_release.sh minor
./cut_release.sh 1.4.0
```

The script does these operations:

1. Confirm that the worktree is clean.
2. Confirm that `main` is not behind `origin/main`.
3. Run the full local test suite.
4. Update the workspace version and lock file.
5. Create a version commit and an annotated tag.
6. Push `main` and the tag.
7. Wait for the container workflow to pass.
8. Create the GitHub release with generated notes.

The script stops when `origin/main` changes during validation.
Run the script again from the updated branch.

Do not run the script when credential files are in the worktree.

Use this command to verify a signature:

```bash
REPOSITORY=OWNER/REPOSITORY
VERSION=v1.0.0
REF="refs/tags/${VERSION}"
cosign verify \
  --certificate-identity="https://github.com/${REPOSITORY}/.github/workflows/container.yml@${REF}" \
  --certificate-oidc-issuer='https://token.actions.githubusercontent.com' \
  "ghcr.io/${REPOSITORY}:${VERSION}"
```

Replace `OWNER`, `REPOSITORY`, and `VERSION` with the release values.
Set `REF` to `refs/tags/${VERSION}` for a tagged release.

Do not publish an image from a working tree that contains provider credentials.
