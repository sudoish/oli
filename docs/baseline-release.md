# Baseline release procedure

This procedure cuts the release used by every private-agent reference setup.
The baseline version is `v0.1.0`.

## Preconditions

- The SUD-194 subscription release gate is merged and its applicable live
  matrix is recorded.
- `Cargo.toml` and the top release heading in `CHANGELOG.md` name the same
  version.
- The checkout is on `master`, up to date, and clean.
- `v0.1.0` does not already exist locally or on the remote.

## Verify

```console
cargo test --lib
cargo build --release
cargo build --release --no-default-features
cargo run --release -- login --help
```

Follow [the subscription release gate](subscription-release-gate.md), including
the browser, pasted-redirect, and device-auth rows applicable to the release
environment. Inspect the release notes and confirm every limitation still
matches the shipped behavior.

## Tag

```console
git tag -a v0.1.0 -m "oli v0.1.0"
git push origin v0.1.0
```

Create the GitHub release from `CHANGELOG.md`, preserving the Included and Known
limitations sections. Attach checksums for any uploaded binaries.

## Reference setup pin

Every private-agent reference setup must pin this tag or a later release. Source
checkouts use the tag directly:

```console
git clone --branch v0.1.0 --depth 1 https://github.com/sudoish/oli.git
```

If a setup uses a packaged binary, record the oli version and artifact checksum
next to its configuration. A moving `master` checkout does not satisfy the pin.

