//! Provider presets, loaded from `presets.json` so they can change without code edits (§6.3).

use serde::{Deserialize, Serialize};

use super::{Protocol, ProviderOptions};

const PRESETS_JSON: &str = include_str!("presets.json");

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Preset {
    pub id: String,
    pub display_name: String,
    pub protocol: Protocol,
    pub base_url: String,
    pub requires_key: bool,
    pub local: bool,
    pub get_key_url: Option<String>,
    pub data_policy_warning: bool,
    pub structured_output: bool,
    pub max_tokens_param: String,
}

impl Preset {
    pub fn options(&self) -> ProviderOptions {
        ProviderOptions {
            structured_output: self.structured_output,
            max_tokens_param: self.max_tokens_param.clone(),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
struct File {
    presets: Vec<RawPreset>,
}

#[derive(Deserialize)]
struct RawPreset {
    id: String,
    display_name: String,
    protocol: Protocol,
    base_url: String,
    requires_key: bool,
    local: bool,
    get_key_url: Option<String>,
    data_policy_warning: bool,
    structured_output: bool,
    max_tokens_param: String,
}

/// All presets, in display order. The file is compiled in and covered by a
/// test, so a parse failure here is a build defect; it yields an empty list
/// rather than a panic.
pub fn all() -> Vec<Preset> {
    serde_json::from_str::<File>(PRESETS_JSON)
        .map(|f| {
            f.presets
                .into_iter()
                .map(|p| Preset {
                    id: p.id,
                    display_name: p.display_name,
                    protocol: p.protocol,
                    base_url: p.base_url,
                    requires_key: p.requires_key,
                    local: p.local,
                    get_key_url: p.get_key_url,
                    data_policy_warning: p.data_policy_warning,
                    structured_output: p.structured_output,
                    max_tokens_param: p.max_tokens_param,
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn find(id: &str) -> Option<Preset> {
    all().into_iter().find(|p| p.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ships_the_section_6_3_presets_and_the_mcp_bridge() {
        let ids: Vec<String> = all().into_iter().map(|p| p.id).collect();
        assert_eq!(
            ids,
            [
                "anthropic",
                "openai",
                "openrouter",
                "gemini",
                "groq",
                "mistral",
                "ollama",
                "lmstudio",
                "mcp_bridge",
                "custom"
            ]
        );
    }

    #[test]
    fn the_bridge_preset_points_at_the_default_mcp_port() {
        let p = find("mcp_bridge").expect("bridge preset");
        assert!(crate::mcp::is_bridge_url(&p.base_url));
        assert!(p.base_url.contains(&crate::settings::DEFAULT_MCP_PORT.to_string()));
        // Answers go to a cloud assistant, so full texts need the warning.
        assert!(p.data_policy_warning);
    }

    #[test]
    fn presets_are_consistent() {
        for p in all() {
            if p.local {
                assert!(!p.requires_key, "{} is local but needs a key", p.id);
                assert!(p.base_url.starts_with("http://localhost"), "{}", p.id);
            } else if p.id != "custom" {
                assert!(p.base_url.starts_with("https://"), "{}", p.id);
                assert!(
                    p.get_key_url
                        .as_deref()
                        .is_some_and(|u| u.starts_with("https://")),
                    "{}",
                    p.id
                );
            }
        }
        assert_eq!(
            find("anthropic").map(|p| p.protocol),
            Some(Protocol::Anthropic)
        );
        assert_eq!(
            find("ollama").map(|p| p.base_url),
            Some("http://localhost:11434/v1".into())
        );
        assert_eq!(
            find("lmstudio").map(|p| p.base_url),
            Some("http://localhost:1234/v1".into())
        );
    }
}
