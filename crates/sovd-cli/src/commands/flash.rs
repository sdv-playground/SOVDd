//! Flash command — firmware update over the spec-compliant /updates wire.

use anyhow::{Context, Result};
use indicatif::{ProgressBar, ProgressStyle};
use sovd_client::flash::{FlashClient, FlashUpdatePhase};
use std::path::Path;

use crate::output::OutputContext;

/// Flash firmware to an ECU via the /updates wire (ISO 17978-3 §7.13).
///
/// One-shot single-part flash: open_update → upload_part →
/// verify → finalize → ecu_reset → commit.  Multi-part flows
/// (manifest + payloads) compose the lower-level FlashClient
/// primitives directly; sovd-cli is intentionally the simple path.
pub async fn flash(client: &FlashClient, file_path: &Path, ctx: &OutputContext) -> Result<()> {
    ctx.info(&format!("Reading firmware from {}...", file_path.display()));
    let firmware = std::fs::read(file_path)
        .with_context(|| format!("Failed to read firmware file: {}", file_path.display()))?;
    ctx.info(&format!("Firmware size: {} bytes", firmware.len()));

    let pb = ProgressBar::new(5);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{bar:40.cyan/blue}] {pos}/{len} {msg}")
            .unwrap()
            .progress_chars("=>-"),
    );

    let pb_ref = pb.clone();
    let progress = move |phase: FlashUpdatePhase| {
        let (pos, msg) = match phase {
            FlashUpdatePhase::Uploading => (1, "Uploading"),
            FlashUpdatePhase::Verifying => (2, "Verifying"),
            FlashUpdatePhase::Finalizing => (3, "Finalizing"),
            FlashUpdatePhase::Resetting => (4, "Resetting ECU"),
            FlashUpdatePhase::Committing => (5, "Committing"),
            FlashUpdatePhase::Complete => (5, "Complete"),
        };
        pb_ref.set_position(pos);
        pb_ref.set_message(msg);
    };

    // `manifest` is the default part_id when the caller doesn't
    // care — single-part flashes don't need a real SUIT envelope
    // structure.  Multi-part callers use the typed primitives.
    client
        .flash_update("manifest", &firmware, "hard", Some(progress))
        .await
        .context("flash_update failed")?;

    pb.finish_with_message("Complete!");
    ctx.success("\nFirmware update completed successfully");

    Ok(())
}
