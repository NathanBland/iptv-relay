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

1. Confirm that the local test runner passes.
2. Confirm that `.env.live` is not staged.
3. Create and push a version tag.
4. Wait for the container workflow to pass.
5. Verify the image signature before deployment.

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
