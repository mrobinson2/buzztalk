# Releasing

## crates.io

The four foundation crates are independently useful and are the ones worth publishing;
`-stt`, `-tts`, `-session`, `-pipeline` and `-buzz` carry heavy native dependencies and a
much less stable API, so they stay unpublished until the API settles.

**Publish in dependency order.** `cargo publish --dry-run` cannot verify a crate whose
workspace sibling is not yet on crates.io, so the first three dry-runs will fail with
`no matching package named buzztalk-core found` until `buzztalk-core` is actually live.
That is expected, not a defect.

```
cargo login                      # needs a crates.io token
cargo publish -p buzztalk-core   # must be first — everything depends on it
# wait for the index to update, usually under a minute
cargo publish -p buzztalk-aec
cargo publish -p buzztalk-vad
cargo publish -p buzztalk-audio
```

Verified ready: every internal dependency carries an explicit `version` alongside its
`path` (crates.io rejects path-only dependencies), and all four declare `keywords`,
`categories`, `description`, `license` and `repository`. `buzztalk-core` passes
`cargo publish --dry-run` cleanly today.

## GitHub release

The `Release` workflow builds native archives on each supported installer target:

| Runner | Release asset |
|---|---|
| Apple Silicon macOS | `buzztalk-macos-arm64.tar.gz` |
| Linux x86_64 | `buzztalk-linux-x86_64.tar.gz` |
| Windows x86_64 | `buzztalk-windows-x86_64.zip` |

Each archive contains `buzztalkd` and `buzztalk-demo` (with `.exe` suffixes on
Windows). The workflow uploads a checksum beside every archive and assembles all three
entries into `SHA256SUMS`, which is what the installers verify.

Create and push the tag to start the native build:

```bash
git tag -a <version> -m "..."
git push origin <version>
```

If the tag has no GitHub release yet, the workflow creates a draft release before attaching
the assets. Review and publish that draft after the build succeeds. An existing release is
reused, and the workflow can also be dispatched manually with an existing tag if release
assets need to be rebuilt. Successful compilation and offline tests on Windows/Linux do not
constitute physical audio validation; keep that distinction explicit in release notes.

## Version honesty

Keep the `-alpha` suffix until the acoustic path is validated on physical hardware.
Every echo-cancellation number in this repository is synthetic, and a bare `v0.1.0`
would imply a completeness that does not exist.
