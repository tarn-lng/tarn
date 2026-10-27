# Linux toolchain releases

GitHub Actions checks the workspace on main and pull requests. A `vX.Y.Z` tag
runs the same full tests, builds Tarn and Tarn LSP on native Ubuntu 22.04 x86_64 and ARM64 runners, exercises the
installer, packages the binaries and publishes a GitHub prerelease. The version
must match Cargo.toml. Release assets are never replaced by the workflow.
Formatting currently reports existing debt without blocking releases.

Supported binary platforms are little-endian Linux x86_64 and ARM64 (aarch64),
with glibc 2.35 or newer. ARM64 artifacts begin with
0.0.8; older published releases stay x86_64-only.
Native application compilation additionally requires `cc` and libc development
headers. Particular APIs may require native libraries, e.g. OpenSSL or libdbus.
The standard library and runtime are embedded in the compiler.

Download the installer, inspect it, then run it:

```sh
curl --fail --proto '=https' https://tarn-lng.github.io/website/install.sh -o install-tarn.sh
sh install-tarn.sh --version 0.0.8
```

The default directory is `$HOME/.local/bin`. Use `--prefix /absolute/directory`
to choose another location. No sudo or shell-profile modification is performed.
`--force` explicitly replaces unowned or modified binaries. Repeating the
command updates an unchanged managed installation. `--uninstall` removes only
an unchanged managed installation, preserving modified binaries.

SHA256SUMS detects corruption; it is retrieved from the same GitHub release and
is not an independent publisher signature. The installer validates the archive
inventory and compiler version before installing. Each binary replacement is
atomic; replacing the two binaries together is not a transactional operation.

To publish, update the workspace version and lock, run checks, commit, and push
the matching tag. Manual workflow dispatch can retry an existing tag before a
release exists. Published tags and release assets must remain immutable.

## Official application-builder image

Every successful release also publishes `ghcr.io/tarn-lng/tarn:X.Y.Z` for
Linux amd64 and arm64. The version tag is a platform index: Docker chooses the
native variant. Explicit `X.Y.Z-amd64` and `X.Y.Z-arm64` tags are also available
for new releases. `ghcr.io/tarn-lng/tarn:latest` points to the same image as the
highest numeric published release, including Tarn's experimental prereleases.
Main-branch pushes do not move latest. Version tags are never replaced by the
workflow; a retry checks the existing image's version and source revision and
runs its smoke tests again. Retrying an older release cannot move latest back.
For content-pinned builds, use the registry digest (`@sha256:...`).

The image contains the exact compiler and LSP binaries from the release archive,
verified against that release's SHA256SUMS, plus Debian Bookworm C/C++ build
tools, libc development headers, curl, CA certificates and libcurl. It does not
need Rust. The standard library remains embedded in Tarn. This is a build image;
application runtime images should contain only their actual native dependencies.
The Debian base and apt packages are resolved at first publication; the image is
not a claim of reproducible OS package resolution. New OS security rebuilds need
a new release/version rather than replacing a published version tag.

Example application build:

```dockerfile
FROM ghcr.io/tarn-lng/tarn:0.0.6 AS builder
WORKDIR /app
COPY . .
RUN mkdir -p /out && tarn fetch && tarn test && tarn build -o /out/app

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends libcurl4 ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /out/app /usr/local/bin/app
ENTRYPOINT ["/usr/local/bin/app"]
```

Adapt runtime libraries and the runtime user to the application. Tarn's TLS
bridge loads libcurl dynamically, so `ldd` alone does not enumerate every
runtime requirement. The output parent directory (`/out`) must exist.

### Publication and recovery

1. The release workflow runs all compiler tests, builds and publishes the
   immutable archives with combined SHA256SUMS, then calls `container.yml` directly. A release created
   with GITHUB_TOKEN does not trigger a second release-event workflow.
2. The container workflow downloads the published archive, verifies its hash
   and exact file inventory, builds the image and tests native compilation,
   execution, Base64/SHA-256 and the stdio LSP without network access.
3. It publishes the version tag and serializes latest promotions. Before
   promotion it selects the highest published release in numeric version order,
   excluding drafts, regardless of which release queued the promotion. This
   prevents both downgrades and lost promotions when GitHub replaces a pending
   concurrency job. If that version image is not available yet, promotion fails
   without moving latest; its image publication or a manual retry can recover.
   Promotion copies the complete platform index rather than a local single-architecture image.
4. On first publication, an organization administrator must make the GHCR
   package public in its package settings. Anonymous pulls are checked after
   setup. GitHub defaults new container packages to private.

If image publication fails after the GitHub release exists, rerun the
**Publish release container** workflow on main with the existing `vX.Y.Z` tag.
This also backfills an existing release such as v0.0.6 without changing its
assets or tag. Do not rerun the archive publication job to replace assets.
The release-event trigger covers releases published manually; the direct call
covers releases published by Tarn's workflow.

Local checks:

```sh
python3 tools/release/test-container-policy.py
python3 tools/release/container.py prepare v0.0.6 /path/to/downloaded-assets target/container-context
docker build --platform linux/amd64 --build-arg VERSION=0.0.6 \
    --build-arg REVISION="$(git rev-parse 'v0.0.6^{commit}')" \
    -t tarn-container-test:0.0.6 target/container-context
python3 tools/release/test-container.py tarn-container-test:0.0.6 0.0.6
```

### Architecture selection

Release archives are named `tarn-X.Y.Z-linux-x86_64.tar.gz` and
`tarn-X.Y.Z-linux-aarch64.tar.gz`. The installer selects from `uname -m`; ARMv7,
big-endian ARM, macOS and Windows are rejected. A single SHA256SUMS lists both
archives; only the selected archive is downloaded and verified.

`tools/release/platform.sh` detects the native package architecture. For local
ARM container preparation, pass `aarch64` as the final argument to
`tools/release/container.py prepare`. Build and run that image on ARM64 using
`--platform linux/arm64`. Release CI tests both architectures natively before
publishing the multi-platform version index.
