use anyhow::{Context, Result, bail};
use gx_metadata::{apple_chart, fetch_lyrics, find_replacements, search_all};

fn main() -> Result<()> {
    let query = std::env::args().nth(1).unwrap_or_else(|| "周杰伦".into());
    let results = search_all(&query, 10).context("real metadata search failed")?;
    if results.is_empty() {
        bail!("real metadata search returned no tracks");
    }
    println!(
        "GX_PHASE3_SEARCH_OK count={} providers={:?}",
        results.len(),
        results
            .iter()
            .map(|track| track.provider_id.as_str())
            .collect::<std::collections::BTreeSet<_>>()
    );
    let lx_track = results
        .iter()
        .find(|track| {
            matches!(track.provider_id.as_str(), "kg" | "kw" | "wy")
                && track.resolver_payload.get("source").is_some()
                && track.resolver_payload.get("musicInfo").is_some()
        })
        .context("metadata search returned no LX-compatible full-track payload")?;
    println!(
        "GX_ONLINE_LX_METADATA_OK provider={} id={} title={}",
        lx_track.provider_id, lx_track.provider_track_id, lx_track.title
    );
    let playable = results
        .iter()
        .find(|track| track.preview.is_some())
        .context("metadata search returned no structured preview request")?;
    println!(
        "GX_PHASE3_STRUCTURED_PREVIEW_OK {} {}",
        playable.provider_id, playable.title
    );
    let lyrics = fetch_lyrics(&playable.title, &playable.artist, playable.duration_ms)
        .context("real lyrics query failed")?;
    println!(
        "GX_PHASE3_LYRICS_OK lines={}",
        lyrics.as_ref().map_or(0, |lyrics| lyrics.lines.len())
    );
    let chart = apple_chart(10).context("real chart query failed")?;
    if chart.is_empty() {
        bail!("real chart query returned no tracks");
    }
    println!("GX_PHASE3_CHART_OK count={}", chart.len());
    if let Some(wanted) = results.first() {
        let replacements = find_replacements(wanted, results.clone());
        println!("GX_PHASE3_REPLACEMENT_OK matches={}", replacements.len());
    }
    Ok(())
}
