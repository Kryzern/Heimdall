//! One-shot import of Flow Launcher's custom web searches.

use crate::config::{Config, WebSearch};

/// Adds Flow Launcher web searches whose keyword isn't already taken.
/// Returns how many were added.
pub fn import(cfg: &mut Config) -> usize {
    let Some(appdata) = std::env::var_os("APPDATA") else { return 0 };
    let path = std::path::PathBuf::from(appdata)
        .join(r"FlowLauncher\Settings\Plugins\Flow.Launcher.Plugin.WebSearch\Settings.json");
    let Ok(text) = std::fs::read_to_string(path) else { return 0 };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(text.trim_start_matches('\u{FEFF}')) else { return 0 };
    let Some(sources) = json.get("SearchSources").and_then(|s| s.as_array()) else { return 0 };
    let mut added = 0;
    for s in sources {
        let get = |k: &str| s.get(k).and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
        let (key, name, url) = (get("ActionKeyword"), get("Title"), get("Url"));
        let enabled = s.get("Enabled").and_then(|v| v.as_bool()).unwrap_or(true);
        if !enabled || key.is_empty() || key == "*" || url.is_empty() || !url.contains("{q}") {
            continue;
        }
        if cfg.web.iter().any(|w| w.key.eq_ignore_ascii_case(&key)) {
            continue;
        }
        cfg.web.push(WebSearch { key, name: if name.is_empty() { url.clone() } else { name }, url });
        added += 1;
    }
    added
}
