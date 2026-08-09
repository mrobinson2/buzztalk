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

```
cargo build --release -p buzztalk-pipeline -p buzztalk-buzz
tar czf buzztalk-<version>-macos-arm64.tar.gz -C target/release buzztalk-demo buzztalkd
shasum -a 256 buzztalk-<version>-macos-arm64.tar.gz > SHA256SUMS.txt
git tag -a <version> -m "..." && git push origin <version>
gh release create <version> --notes-file NOTES.md --prerelease *.tar.gz SHA256SUMS.txt
```

Binaries are macOS arm64 only. Cross-compiled artifacts are not published because no
Windows or Linux machine has run the audio path — CI proves compilation, not audio.

## Version honesty

Keep the `-alpha` suffix until the acoustic path is validated on physical hardware.
Every echo-cancellation number in this repository is synthetic, and a bare `v0.1.0`
would imply a completeness that does not exist.
