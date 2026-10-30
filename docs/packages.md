# Package management

Tarn includes a pure-source package manager in the existing CLI. There is no
default public registry. Select a local registry or a HTTPS origin serving the
registry format below. Published package code executes only when the application
runs; installation and compilation never execute package hooks. This does not
sandbox a running application or the trusted compiler/C toolchain.

## First local workflow

```sh
tarn init greeting --lib
cd greeting
# Edit lib.tarn; exported declarations use pub.
tarn publish --registry /tmp/tarn-registry
cd ..
tarn init application
cd application
tarn add greeting --registry /tmp/tarn-registry
```

Use the package without a version in its import:

```tarn sketch
import "greeting"

fn main() {
    print(greeting.greet())
}
```

```sh
tarn run
tarn test
tarn deps --tree
tarn deps --why greeting
tarn deps --trust
tarn verify --json
tarn audit --json
```

Commit both `tarn.toml` and `tarn.lock`. On another machine, `tarn fetch` retrieves
exact locked sources; subsequent builds need no registry connection. A missing
cache, altered file or incompatible lock causes an error. Builds never download,
update or rewrite a lock. A hash mismatch remains an error even with `fetch`.
Investigate altered data; do not regenerate a lock simply to accept unknown bytes.

## Manifest

```toml
[package]
name = "application"
version = "0.1.0"
entry = "main.tarn"

[registry]
source = "https://packages.example.org/"

[dependencies]
greeting = "^1.2"
helper = { version = ">=1.0, <2.0", registry = "file:///srv/tarn-registry/" }

[security]
minimum_release_age = 0 # seconds; bootstrap default
require_provenance = false
```

Package names are lowercase ASCII Tarn identifiers, at most 64 characters;
bundled standard-library names are reserved. Entry paths are relative `.tarn`
files, including `src/main.tarn`. Commands discover the nearest ancestor manifest
and use its entry when no file is given. Manifestless programs still work.

SemVer requirements support `=1.2.3`, `^1.2`, `~1.2`, `1.*` and comma-separated
comparators. Prereleases require an explicit matching prerelease requirement.
`add` without `--version` records a caret requirement for the latest stable release.
The bounded resolver chooses one version and origin per identity, backtracking
when constraints conflict. Add/remove prefer existing compatible locked versions;
`update` chooses highest compatible releases. `update helper` freezes other
locked versions and fails if they also need to change. Cycles and origin conflicts
are rejected. Conflicts explain which dependencies introduced the requirements.

Unknown fields, build hooks, features and package permissions are rejected.
Published manifests bind dependency origins absolutely. Dependencies may import
only their declared packages and local modules. Application imports cannot reach
an undeclared transitive package. Package-local modules are separately scoped;
local/package ambiguity is an error. Compiler visibility rules still apply.

## Commands and storage

| Command | Behavior |
| --- | --- |
| `init [directory] [--lib] [--name name] [--registry origin]` | Create a project without overwriting existing source or manifest |
| `add name [--version requirement] [--registry origin]` | Resolve, verify and explicitly write manifest/lock |
| `remove name` | Remove a direct dependency and prune its unused graph |
| `update [name]` | Intentionally refresh compatible versions |
| `fetch` | Retrieve and verify existing lock; never change it |
| `deps [--tree \| --why name] [--trust] [--json]` | Explain selected graph, origins, hashes and unknown trust |
| `verify [--json]` | Check graph, policy and every cached source |
| `audit [--json]` | Check origin advisory evidence and report unknown provenance |
| `publish [--registry origin]` | Compiler-check and publish an immutable local release |

`TARN_HOME` defaults to `~/.tarn`; `sources/<sha256>/` deduplicates source bytes.
Global `TARN_HOME/config.toml` accepts `[registry]` and `[security]`. Project/global
age combine by maximum; required provenance combines by OR. Required signed
provenance currently refuses because the bootstrap protocol has no signatures.
A verified hash proves integrity against the lock, not publisher trust. Registry
release times are declared evidence, not independently authenticated timestamps.

`tarn.lock` is schema-1 JSON: semantic manifest fingerprint and ordered packages,
each with an explicit registry and its complete release record. SHA-256 binds a
sorted source inventory using the domain `Tarn package source v1\0`, followed by
little-endian u64 path length, path bytes, u64 content length and content bytes.
Inventory excludes executable hooks; publication includes `.tarn`, `tarn.toml`,
root README/license files, excluding hidden/build directories. Sources must be
UTF-8; symlinks and traversal are forbidden. Limits: 128 packages, 512 versions
per index, 10,000 resolution steps, 1,024 files/32 MiB per package, 4 MiB per file,
32 directory levels. Compiler reads the verified byte snapshot.

Explicit mutations hold `.tarn-package-transaction` containing the owning PID.
If a process was interrupted, inspect its PID before removing a stale guard.
Manifest and lock replacements are individually atomic; an interrupted pair is
rejected by its fingerprint rather than silently building a mixed graph.

## Registry protocol and limits

A registry contains `<name>/index.json` (array of SemVer strings),
`<name>/<version>/release.json` and `<name>/<version>/files/<relative-path>`.
Release records contain schema, name, version, hash, entry, published (Unix seconds),
dependencies (version and normalized registry), and file path/SHA-256 pairs.
Versions cannot be republished. Serve this directory over HTTPS for consumption.

Downloads use system `curl`, TLS validation, no redirects, a cleared credential
and proxy environment, no curlrc, 30-second limits and bounded bytes. Explicit
`TARN_CA_BUNDLE` can select a caller-approved CA bundle. Local publication is
filesystem-controlled; authenticated network publication is not implemented.

Optional `advisories.json` is an array of objects with exactly `package`, `affected`
(SemVer requirement), `id` and `summary`. Missing evidence is unknown; malformed
evidence fails. Audit exit codes: 0 only for a graph with no unknown evidence,
1 for vulnerabilities/errors, 3 for unknown evidence/provenance. An unsigned clean
advisory list does not establish safety. `verify` may return 0 for integrity while
explicitly reporting provenance as unknown.

Public registry deployment, accounts/MFA, publisher identity, signing/attestations,
yanking, federation, multi-major coexistence, package features, authorized build
sandboxes and artifact caching remain separate work. See
[ADR 0051](adr/0051-verified-source-packages.md) and
[the security requirements](dependency-security.md).
