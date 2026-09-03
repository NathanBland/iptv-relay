# Release container images

The repository builds release images with GitHub Actions.

The workflow runs when a maintainer pushes a tag that starts with `v`.

The workflow also supports a manual run from the Actions page.

The workflow builds these platforms:

- `linux/amd64`
- `linux/arm64`

The workflow publishes the image to GitHub Container Registry.

The workflow creates an SBOM and a provenance attestation.

The workflow signs the published image digest with Cosign keyless signing.

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
