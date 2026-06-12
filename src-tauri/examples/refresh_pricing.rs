//! Live verification of the pricing remote-refresh path (task 3.3):
//! fetches the real pinned LiteLLM URL over TLS, validates, writes the
//! cache into a temp dir, and reloads it. Run with:
//!
//! ```sh
//! cargo run --example refresh_pricing
//! ```

use farthing_lib::pricing;

fn main() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let dir = std::env::temp_dir().join("cut-pricing-live-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let state = pricing::PricingState::new(pricing::PricingTable::bundled());
        let count = pricing::refresh_once(
            pricing::PRICING_REFRESH_URL,
            &state,
            &pricing::cache_path(&dir),
        )
        .await
        .expect("live refresh failed");
        println!("live refresh ok: {count} models");
        let reloaded = pricing::PricingTable::load(&dir);
        println!(
            "reload source: {} ({} models)",
            reloaded.source,
            reloaded.len()
        );
        assert!(reloaded.lookup("claude-fable-5").is_some());
        let _ = std::fs::remove_dir_all(&dir);
    });
}
