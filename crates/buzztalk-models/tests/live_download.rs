//! Real end-to-end download test -- hits the actual network (GitHub,
//! HuggingFace) and downloads the full ~295 MB of both bundles into a
//! throwaway temp directory. `#[ignore]`d so `cargo test -p
//! buzztalk-models` never does this by accident; run explicitly with:
//!
//! ```text
//! cargo test -p buzztalk-models --test live_download -- --ignored --nocapture
//! ```

use std::time::Instant;

use buzztalk_models::{ensure_models_at, status_at, ModelSet, UreqFetcher};

#[test]
#[ignore = "hits the real network and downloads ~295 MB; run explicitly"]
fn downloads_and_verifies_both_bundles_into_a_temp_dir() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let fetcher = UreqFetcher;

    let start = Instant::now();
    ensure_models_at(ModelSet::All, dir.path(), &fetcher, &mut |_| {})
        .expect("ensure_models_at should succeed against the real, pinned URLs");
    let elapsed = start.elapsed();

    let status = status_at(dir.path());
    assert!(
        status.all_present(),
        "both bundles should be fully installed: {status}"
    );
    println!(
        "live_download: installed {} in {:.1}s",
        buzztalk_models::human_bytes(status.total_bytes_on_disk()),
        elapsed.as_secs_f64()
    );

    // Re-run: should be a no-op (idempotent), and much faster.
    let start2 = Instant::now();
    ensure_models_at(ModelSet::All, dir.path(), &fetcher, &mut |_| {})
        .expect("second ensure_models_at (idempotent re-run) should succeed");
    println!(
        "live_download: idempotent re-run took {:.2}s",
        start2.elapsed().as_secs_f64()
    );
}
