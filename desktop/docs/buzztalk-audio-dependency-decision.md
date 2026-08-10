# `buzztalk-audio` dependency ownership recommendation

Status: **Open — maintainer acceptance required**  
Recommendation owner: BuzzTalk gateway execution lane  
Review target: Buzz maintainers for the Desktop Audio Bridge

## Decision gate

This document is a recommendation, not maintainer approval. The bridge PR
must remain draft while the ownership model is open.

**Merge allowed: no.**

No Buzz Desktop Cargo manifest or bridge production source should consume a
personal Git revision as a merge-ready dependency. A personal revision may be
used only for a draft experiment whose provenance and revision are recorded.

## Options considered

### 1. Publish `buzztalk-audio` to crates.io

The BuzzTalk maintainers would publish a semver-versioned `buzztalk-audio`
package from the canonical `mrobinson2/buzztalk` repository. A maintained
crates.io owner would own releases, yanks, advisories, changelog entries, and
the compatibility policy consumed by Buzz.

Advantages:

- Buzz consumes a reviewable, immutable registry release rather than a moving
  Git branch or personal revision.
- Version selection, lockfile review, checksum verification, and rollback fit
  existing Rust supply-chain controls.
- The crate remains reusable by other maintained native integrations.

Costs and required controls:

- Buzz and BuzzTalk must agree on API and semver support windows.
- The publishing owner needs a two-person release review, a documented
  security contact, and a prompt response path for native-audio advisories.
- License files, dependency provenance, generated/native bindings, and release
  artifacts must be reviewed before every release.

### 2. Vendor the minimal crate into the Buzz monorepo

Buzz would copy the minimal `buzztalk-audio` surface into an explicitly owned
Buzz Desktop path, preserving license and provenance notices and recording the
upstream source commit. Buzz code owners would own changes and releases as
part of the desktop repository.

Advantages:

- Ownership, review, rollback, and platform policy are in the same repository
  as the only current consumer.
- Buzz's existing CI, security review, license review, and release process
  can cover the native code without a personal external dependency.
- The bridge can pin the exact source used by each Buzz release.

Costs and required controls:

- Vendoring creates a synchronization obligation with upstream BuzzTalk.
- Every sync must record the source commit, diff, license provenance, and
  compatibility impact; local patches need explicit owners.
- Later extraction into a shared package must be treated as a new maintainer
  decision, not an implicit consequence of vendoring.

### 3. Use another maintained home accepted by Buzz maintainers

Buzz could depend on a foundation, organization, or other repository that
accepts named long-term ownership of the crate. This is viable only if the
maintainer-approved home provides a public release policy and the source is
not controlled by an individual account.

Required controls:

- The accepted owner and repository must be named in the bridge decision and
  Cargo manifest.
- The owner must publish versioning/deprecation rules, update cadence,
  vulnerability response, release provenance, and license obligations.
- Buzz maintainers must complete source review and record the acceptance before
  the dependency is changed from a draft experiment.

This option has no proposed destination yet, so it cannot be the merge
recommendation without a named maintainer and an accepted governance model.

## Recommendation

Recommend **option 2: vendor the minimal `buzztalk-audio` crate into the Buzz
monorepo under explicit Buzz ownership** for the first Desktop Audio Bridge
release. The proposed long-term owner is the Buzz Desktop maintainers/code
owners; the proposed source is a vendored copy with an upstream commit and
license/provenance record attached to each Buzz release.

This recommendation minimizes the unresolved ownership risk for a native
audio dependency while keeping the bridge's platform scope narrow: Apple
Silicon macOS only, with no Windows or Linux native bridge claim. It does not
transfer Buzz identity, signing, relay, STT, TTS, models, routing, or message
publication into `buzztalk-audio`.

Before acceptance, Buzz maintainers must name the responsible code owners and
accept all of the following operating rules:

- **Versioning:** record the upstream source commit and vendored API version
  in the Buzz changelog/release metadata; review breaking changes explicitly.
- **Updates:** schedule upstream sync review at least once per release and
  document local patches, compatibility testing, and rollback procedure.
- **Security response:** route native-audio vulnerabilities to named Buzz
  maintainers, assess advisories promptly, and ship or backport fixes through
  the Buzz release process.
- **License/provenance:** retain upstream license and NOTICE files, inventory
  transitive dependencies and native platform APIs, and attach source commit
  provenance to the vendored copy and release artifact.
- **Source review:** require Buzz code-owner review plus CI on the supported
  Apple Silicon macOS route; do not treat fake-driver tests as a replacement
  for the separately required human hardware validation.

## Required maintainer disposition

Buzz maintainers must either accept this vendoring recommendation, accept one
of the other two options with equivalent named controls, or reject the bridge
proposal. Until that response is recorded:

- Disposition remains **Open**.
- The bridge PR remains **Draft**.
- **Merge allowed: no.**
- The draft experiment may use the recorded personal Git revision only for
  non-merge testing; the production manifest must not claim it as a maintained
  source.
