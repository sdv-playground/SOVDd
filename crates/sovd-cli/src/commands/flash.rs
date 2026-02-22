//! Flash command - firmware update

use anyhow::{Context, Result};
use indicatif::{ProgressBar, ProgressStyle};
use sovd_client::flash::{FlashClient, FlashProgress};
use std::path::Path;

use crate::output::OutputContext;

/// Flash firmware to an ECU
pub async fn flash(client: &FlashClient, file_path: &Path, ctx: &OutputContext) -> Result<()> {
    // Read firmware file
    ctx.info(&format!("Reading firmware from {}...", file_path.display()));
    let firmware = std::fs::read(file_path)
        .with_context(|| format!("Failed to read firmware file: {}", file_path.display()))?;

    ctx.info(&format!("Firmware size: {} bytes", firmware.len()));

    // Create progress bar
    let pb = ProgressBar::new(100);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{bar:40.cyan/blue}] {pos}% {msg}")
            .unwrap()
            .progress_chars("=>-"),
    );

    // Phase 1: Upload
    pb.set_message("Uploading...");
    let upload = client
        .upload_file(&firmware)
        .await
        .context("Failed to upload firmware")?;
    pb.set_position(10);

    // Wait for upload to complete
    pb.set_message("Processing upload...");
    let upload_status = client
        .poll_upload_complete(&upload.upload_id)
        .await
        .context("Upload failed")?;

    let file_id = upload_status
        .file_id
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("No file ID returned from upload"))?;
    pb.set_position(20);

    // Phase 2: Verify
    pb.set_message("Verifying...");
    let verify = client
        .verify_file(file_id)
        .await
        .context("Failed to verify firmware")?;

    if !verify.valid {
        pb.finish_with_message("Verification failed!");
        return Err(anyhow::anyhow!(
            "Firmware verification failed: {}",
            verify.error.unwrap_or_else(|| "Unknown error".to_string())
        ));
    }
    pb.set_position(30);

    // Phase 3: Flash
    pb.set_message("Flashing...");
    let transfer = client
        .start_flash(file_id)
        .await
        .context("Failed to start flash transfer")?;

    // Poll for completion with progress updates
    let progress_callback = |progress: &FlashProgress| {
        let percent = progress.percent.unwrap_or(0.0);
        let pos = 30 + (percent * 0.55) as u64;
        pb.set_position(pos.min(85));
        pb.set_message(format!("Flashing... {:.0}%", percent));
    };

    let final_status = client
        .poll_flash_complete(&transfer.transfer_id, Some(progress_callback))
        .await
        .context("Flash transfer failed")?;

    if !final_status.state.is_success() {
        pb.finish_with_message("Flash failed!");
        let error_msg = final_status
            .error
            .map(|e| e.message)
            .unwrap_or_else(|| "Unknown error".to_string());
        return Err(anyhow::anyhow!("Flash failed: {}", error_msg));
    }
    pb.set_position(90);

    // Phase 4: Finalize
    pb.set_message("Finalizing...");
    client
        .transfer_exit()
        .await
        .context("Failed to finalize transfer")?;
    pb.set_position(95);

    // Phase 5: Reset ECU
    pb.set_message("Resetting ECU...");
    client
        .ecu_reset_with_type("hard")
        .await
        .context("Failed to reset ECU")?;
    pb.set_position(100);

    pb.finish_with_message("Complete!");
    ctx.success("\nFirmware update completed successfully");

    Ok(())
}
