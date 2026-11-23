use serde_json::json;

use crate::api::ApiClient;
use crate::config::resolve_auth;
use crate::error::Result;

pub async fn run(json_mode: bool) -> Result<()> {
    let auth = resolve_auth()?;
    let api = ApiClient::new(&auth)?;
    let titles = api.list_titles().await?;

    if json_mode {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({ "titles": titles }))?
        );
        return Ok(());
    }

    if titles.is_empty() {
        println!("This studio has no titles yet.");
        return Ok(());
    }

    for t in &titles {
        let kind = if t.kind == "addon" {
            "add-on"
        } else {
            "game  "
        };
        let state = if t.linked {
            ""
        } else {
            "  (not on Arcane yet: upload its first build from the portal)"
        };
        println!("{}  {kind}  {}{state}", t.id, t.name);
    }

    Ok(())
}
