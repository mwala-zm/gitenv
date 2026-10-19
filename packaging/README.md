# Packaging

Everything here is driven by `.github/workflows/release.yml`. Pushing a tag
`vX.Y.Z` (matching `version` in `Cargo.toml`) does the following:

| Channel | How | Needs |
|---|---|---|
| GitHub release | tarballs for macOS and Linux, arm64 and x86_64 | nothing |
| `.deb` and `.rpm` | `cargo-deb` and `cargo-generate-rpm`, attached to the release | metadata in `Cargo.toml` |
| Homebrew | `render.sh` fills `homebrew/gitenv.rb.in` and pushes it to [mwala-zm/homebrew-tap](https://github.com/mwala-zm/homebrew-tap) | secret `HOMEBREW_TAP_DEPLOY_KEY`: a deploy key with write access to the tap |
| crates.io | `cargo publish` | secret `CARGO_REGISTRY_TOKEN` |
| AUR (`gitenv-bin`) | `render.sh` fills `aur/PKGBUILD.in`, pushed with `github-actions-deploy-aur` | secret `AUR_SSH_PRIVATE_KEY`: the SSH key registered on your AUR account |

The crates.io and AUR jobs are skipped, not failed, while their secret is
missing, so a release still goes out to the other channels.

To check a formula or PKGBUILD by hand before tagging, put the four release
archives in a directory and run:

```sh
packaging/render.sh 0.1.0 path/to/archives out
brew style out/gitenv.rb
```
