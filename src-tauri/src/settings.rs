//! App-level settings in `app.db` (not secret; keys live in the keychain).

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

const KEY: &str = "app_settings";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AppSettings {
    /// Never switch to another model mid-review (§6.4). On by default.
    pub strict_mode: bool,
    /// Flag providers whose free tiers may use prompts for training, and ask
    /// before sending full texts to them (§11).
    pub warn_data_policy: bool,
    /// Sent to Unpaywall as the `email` parameter.
    pub unpaywall_email: Option<String>,
    /// e.g. `https://login.proxy.example.edu/login?url=`
    pub library_proxy_prefix: Option<String>,
    /// Pause a review when its estimated spend reaches this many US dollars.
    pub spending_cap_usd: Option<f64>,
    /// Python interpreter with `laya-mlx` installed; detected when empty.
    pub laya_python: Option<String>,
    /// The computer is on an institutional network or VPN: follow DOI links
    /// to publishers' own PDF links (off by default).
    pub institutional_access: bool,
    /// New reviews start in bypass mode: nothing waits for a person, and the
    /// review stays unverified (see `pipeline::bypass`).
    pub bypass_by_default: bool,
    /// Title page of the APA manuscript.
    pub author_name: Option<String>,
    pub author_affiliation: Option<String>,
    /// The author's own statements for PRISMA items 25 and 26, reused in
    /// every review. Never drafted by a model.
    pub funding_statement: Option<String>,
    pub competing_interests: Option<String>,
    /// Serve the MCP endpoint so an outside assistant can run reviews and
    /// answer the app's model calls (see `mcp`).
    pub mcp_enabled: bool,
    pub mcp_port: u16,
}

pub const DEFAULT_MCP_PORT: u16 = 31480;

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            strict_mode: true,
            warn_data_policy: true,
            unpaywall_email: None,
            library_proxy_prefix: None,
            spending_cap_usd: Some(50.0),
            laya_python: None,
            institutional_access: false,
            bypass_by_default: false,
            author_name: None,
            author_affiliation: None,
            funding_statement: None,
            competing_interests: None,
            mcp_enabled: false,
            mcp_port: DEFAULT_MCP_PORT,
        }
    }
}

impl AppSettings {
    /// Trims text, drops empty strings, and rejects values that cannot work.
    pub fn normalised(mut self) -> Result<Self> {
        let clean = |v: Option<String>| v.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty());
        self.unpaywall_email = clean(self.unpaywall_email);
        self.library_proxy_prefix = clean(self.library_proxy_prefix);
        self.laya_python = clean(self.laya_python);
        self.author_name = clean(self.author_name);
        self.author_affiliation = clean(self.author_affiliation);
        self.funding_statement = clean(self.funding_statement);
        self.competing_interests = clean(self.competing_interests);
        if self.mcp_port < 1024 {
            return Err(Error::Invalid(
                "Choose an MCP port from 1024 to 65535.".into(),
            ));
        }
        if let Some(email) = &self.unpaywall_email {
            let valid = email
                .split_once('@')
                .is_some_and(|(a, b)| !a.is_empty() && b.contains('.'))
                && !email.contains(char::is_whitespace);
            if !valid {
                return Err(Error::Invalid(
                    "Enter a valid email address for Unpaywall.".into(),
                ));
            }
        }
        if let Some(prefix) = &self.library_proxy_prefix {
            if !prefix.starts_with("https://") && !prefix.starts_with("http://") {
                return Err(Error::Invalid(
                    "The library proxy prefix must start with https://".into(),
                ));
            }
        }
        if let Some(cap) = self.spending_cap_usd {
            if !cap.is_finite() || cap < 0.0 {
                return Err(Error::Invalid(
                    "The spending cap must be zero or more.".into(),
                ));
            }
        }
        Ok(self)
    }
}

pub fn load(conn: &Connection) -> Result<AppSettings> {
    let raw: Option<String> = conn
        .query_row("SELECT value FROM setting WHERE key = ?1", [KEY], |r| {
            r.get(0)
        })
        .optional()?;
    Ok(match raw {
        Some(json) => serde_json::from_str(&json)?,
        None => AppSettings::default(),
    })
}

pub fn save(conn: &Connection, settings: AppSettings) -> Result<AppSettings> {
    let settings = settings.normalised()?;
    conn.execute(
        "INSERT INTO setting (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![KEY, serde_json::to_string(&settings)?],
    )?;
    Ok(settings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    #[test]
    fn defaults_then_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let conn = db::open_app(&dir.path().join("app.db")).unwrap();
        let s = load(&conn).unwrap();
        assert!(s.strict_mode && s.warn_data_policy);
        assert_eq!(s.spending_cap_usd, Some(50.0));
        let saved = save(
            &conn,
            AppSettings {
                unpaywall_email: Some("  researcher@uni.ac.uk ".into()),
                library_proxy_prefix: Some("".into()),
                spending_cap_usd: Some(20.0),
                ..s
            },
        )
        .unwrap();
        assert_eq!(
            saved.unpaywall_email.as_deref(),
            Some("researcher@uni.ac.uk")
        );
        assert_eq!(saved.library_proxy_prefix, None);
        assert_eq!(load(&conn).unwrap(), saved);
    }

    #[test]
    fn rejects_bad_values() {
        let bad_email = AppSettings {
            unpaywall_email: Some("not an email".into()),
            ..Default::default()
        };
        assert!(bad_email.normalised().is_err());
        let bad_proxy = AppSettings {
            library_proxy_prefix: Some("proxy.uni.edu".into()),
            ..Default::default()
        };
        assert!(bad_proxy.normalised().is_err());
        let bad_cap = AppSettings {
            spending_cap_usd: Some(-5.0),
            ..Default::default()
        };
        assert!(bad_cap.normalised().is_err());
    }
}
