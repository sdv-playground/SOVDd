//! Bulk-data command — SOVD §7.20. The spec-native "get all logs".
//!
//! `bulk-data <ecu> [action] …`:
//!   categories (default)  — list the entity's bulk-data categories
//!   list <category>       — list downloadable items in a category
//!   download <cat> <id>   — download one item (stdout, or -o file)
//!   get-all <category>    — download EVERY item in a category to -d <dir>
//!                           (one file per item id) — the "get all logs" flow.

use anyhow::{bail, Context, Result};
use serde::Serialize;
use sovd_client::SovdClient;
use tabled::Tabled;

use crate::output::OutputContext;

#[derive(Debug, Tabled, Serialize)]
struct CategoryRow {
    #[tabled(rename = "Category")]
    id: String,
}

#[derive(Debug, Tabled, Serialize)]
struct ItemRow {
    #[tabled(rename = "ID")]
    id: String,
    #[tabled(rename = "Size")]
    size: u64,
    #[tabled(rename = "Created")]
    created: String,
    #[tabled(rename = "Source")]
    source: String,
}

/// Entry point for `sovd-cli bulk-data …`.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    client: &SovdClient,
    ecu: &str,
    action: &str,
    category: Option<&str>,
    id: Option<&str>,
    created_after: Option<&str>,
    created_before: Option<&str>,
    out: Option<&str>,
    dir: Option<&str>,
    ctx: &OutputContext,
) -> Result<()> {
    match action {
        "categories" => {
            let cats = client.list_bulk_data_categories(ecu).await?;
            if cats.is_empty() {
                ctx.info("No bulk-data categories");
                return Ok(());
            }
            let rows: Vec<CategoryRow> = cats.into_iter().map(|c| CategoryRow { id: c.id }).collect();
            ctx.print(&rows);
        }
        "list" => {
            let category = category.context("`bulk-data list` needs a <category>")?;
            let items = client
                .list_bulk_data(ecu, category, created_after, created_before)
                .await?;
            if items.is_empty() {
                ctx.info(&format!("No items in category `{category}`"));
                return Ok(());
            }
            let rows: Vec<ItemRow> = items.iter().map(item_row).collect();
            ctx.print(&rows);
        }
        "download" => {
            let category = category.context("`bulk-data download` needs a <category>")?;
            let id = id.context("`bulk-data download` needs an <id>")?;
            let bytes = client.get_bulk_data(ecu, category, id).await?;
            match out {
                Some(path) => {
                    std::fs::write(path, &bytes)?;
                    ctx.success(&format!("wrote {} byte(s) to {path}", bytes.len()));
                }
                None => {
                    use std::io::Write;
                    std::io::stdout().write_all(&bytes)?;
                }
            }
        }
        "get-all" => {
            let category = category.context("`bulk-data get-all` needs a <category>")?;
            let dir = dir.context("`bulk-data get-all` needs -d <dir> to write items into")?;
            let items = client
                .list_bulk_data(ecu, category, created_after, created_before)
                .await?;
            if items.is_empty() {
                ctx.info(&format!("No items in category `{category}` — nothing to download"));
                return Ok(());
            }
            std::fs::create_dir_all(dir)
                .with_context(|| format!("create output dir {dir}"))?;
            let mut total = 0usize;
            for it in &items {
                let bytes = client.get_bulk_data(ecu, category, &it.id).await?;
                // File name = the item id + a source hint; ids are opaque
                // (base64url), so prefix the source for a human-readable name.
                let name = match &it.source {
                    Some(s) => format!("{s}--{}.log", short_id(&it.id)),
                    None => format!("{}.bin", short_id(&it.id)),
                };
                let path = std::path::Path::new(dir).join(&name);
                std::fs::write(&path, &bytes)
                    .with_context(|| format!("write {}", path.display()))?;
                ctx.info(&format!("  {} ({} byte(s))", path.display(), bytes.len()));
                total += bytes.len();
            }
            ctx.success(&format!(
                "downloaded {} item(s) from `{category}` ({total} byte(s)) into {dir}",
                items.len()
            ));
        }
        other => bail!("unknown bulk-data action `{other}` (expected: categories, list, download, get-all)"),
    }
    Ok(())
}

fn item_row(it: &sovd_client::BulkItemRef) -> ItemRow {
    ItemRow {
        id: it.id.clone(),
        size: it.size,
        created: it.created.clone().unwrap_or_default(),
        source: it.source.clone().unwrap_or_default(),
    }
}

/// A short, filesystem-safe slice of an opaque item id for building a filename
/// (the full base64url id can be long; the source prefix carries the meaning).
fn short_id(id: &str) -> String {
    id.chars().filter(|c| c.is_alphanumeric()).take(12).collect()
}
