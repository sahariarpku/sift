//! Bibliographic sources: catalogue, key status and key tests. The search
//! adapters (§7) arrive in Phase 3; Phase 1 only checks that credentials work.

pub mod adapter;
pub mod core;
pub mod eric;
pub mod import;
pub mod keytest;
pub mod openalex;
pub mod semantic_scholar;

use serde::Serialize;

use crate::error::Result;
use crate::secrets::{self, SecretStatus, SecretStore};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceAuth {
    /// No credential.
    None,
    /// A key is required.
    Key,
    /// Works without a key; a key raises limits.
    OptionalKey,
    /// API key plus an optional institution token (Scopus).
    KeyAndInstToken,
    /// An email address, not a secret (Unpaywall).
    Email,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceInfo {
    pub id: &'static str,
    pub name: &'static str,
    pub auth: SourceAuth,
    pub note: &'static str,
    pub get_key_url: Option<&'static str>,
    pub docs_url: &'static str,
}

/// In the §7 priority order. Unpaywall is configured under Full-text access.
pub const SOURCES: &[SourceInfo] = &[
    SourceInfo {
        id: "eric",
        name: "ERIC",
        auth: SourceAuth::None,
        note: "No key needed",
        get_key_url: None,
        docs_url: "https://eric.ed.gov/?api",
    },
    SourceInfo {
        id: "openalex",
        name: "OpenAlex",
        // Keyless calls work on a smaller daily budget (help.openalex.org/api/authentication).
        auth: SourceAuth::OptionalKey,
        note: "Free key recommended (larger daily budget)",
        get_key_url: Some("https://openalex.org/settings/api"),
        docs_url: "https://help.openalex.org/api/authentication/",
    },
    SourceInfo {
        id: "wos",
        name: "Web of Science",
        auth: SourceAuth::Key,
        note: "Free Starter plan or institutional key",
        get_key_url: Some("https://developer.clarivate.com/apis/wos-starter"),
        docs_url: "https://developer.clarivate.com/apis/wos-starter",
    },
    SourceInfo {
        id: "scopus",
        name: "Scopus",
        auth: SourceAuth::KeyAndInstToken,
        note: "API key + institution token",
        get_key_url: Some("https://dev.elsevier.com/"),
        docs_url: "https://dev.elsevier.com/api_key_settings.html",
    },
    SourceInfo {
        id: "pubmed",
        name: "PubMed",
        auth: SourceAuth::OptionalKey,
        note: "Optional key raises rate limit",
        get_key_url: Some("https://account.ncbi.nlm.nih.gov/settings/"),
        docs_url: "https://www.ncbi.nlm.nih.gov/books/NBK25497/",
    },
    SourceInfo {
        id: "semantic_scholar",
        name: "Semantic Scholar",
        auth: SourceAuth::OptionalKey,
        note: "Optional",
        get_key_url: Some("https://www.semanticscholar.org/product/api"),
        docs_url: "https://api.semanticscholar.org/api-docs/",
    },
    SourceInfo {
        id: "core",
        name: "CORE",
        auth: SourceAuth::Key,
        note: "Free key · open-access full texts",
        get_key_url: Some("https://core.ac.uk/api-keys/register"),
        docs_url: "https://core.ac.uk/documentation/api",
    },
    // Full-text APIs: subscription access is granted by the institution's IP
    // range (campus network or VPN), checked 2026-09-25.
    SourceInfo {
        id: "elsevier",
        name: "Elsevier (ScienceDirect)",
        auth: SourceAuth::Key,
        note: "Free key · subscribed full texts on your institution's network",
        get_key_url: Some("https://dev.elsevier.com/apikey/manage"),
        docs_url: "https://dev.elsevier.com/documentation/ArticleRetrievalAPI.wadl",
    },
    SourceInfo {
        id: "wiley",
        name: "Wiley (TDM)",
        auth: SourceAuth::Key,
        note: "Free token · subscribed full texts on your institution's network",
        get_key_url: Some(
            "https://onlinelibrary.wiley.com/library-info/resources/text-and-datamining",
        ),
        docs_url: "https://github.com/WileyLabs/tdm-client",
    },
    SourceInfo {
        id: "unpaywall",
        name: "Unpaywall",
        auth: SourceAuth::Email,
        note: "Email address, no key",
        get_key_url: None,
        docs_url: "https://unpaywall.org/products/api",
    },
];

pub fn find(id: &str) -> Option<&'static SourceInfo> {
    SOURCES.iter().find(|s| s.id == id)
}

pub fn key_ref(id: &str) -> String {
    format!("source:{id}")
}

pub fn inst_token_ref(id: &str) -> String {
    format!("source:{id}:insttoken")
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceStatus {
    #[serde(flatten)]
    pub info: SourceInfo,
    pub key: SecretStatus,
    pub inst_token: Option<SecretStatus>,
    /// Usable now: no credential needed, or the required one is present.
    pub ready: bool,
}

pub fn statuses(
    store: &dyn SecretStore,
    unpaywall_email: Option<&str>,
) -> Result<Vec<SourceStatus>> {
    SOURCES
        .iter()
        .map(|info| {
            let key = secrets::status(store, &key_ref(info.id))?;
            let inst_token = match info.auth {
                SourceAuth::KeyAndInstToken => {
                    Some(secrets::status(store, &inst_token_ref(info.id))?)
                }
                _ => None,
            };
            let ready = match info.auth {
                SourceAuth::None | SourceAuth::OptionalKey => true,
                SourceAuth::Key | SourceAuth::KeyAndInstToken => key.present,
                SourceAuth::Email => unpaywall_email.is_some(),
            };
            Ok(SourceStatus {
                info: info.clone(),
                key,
                inst_token,
                ready,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::MemoryStore;

    #[test]
    fn catalogue_follows_section_7() {
        let ids: Vec<&str> = SOURCES.iter().map(|s| s.id).collect();
        assert_eq!(
            ids,
            [
                "eric",
                "openalex",
                "wos",
                "scopus",
                "pubmed",
                "semantic_scholar",
                "core",
                "elsevier",
                "wiley",
                "unpaywall"
            ]
        );
        for s in SOURCES {
            assert!(s.docs_url.starts_with("https://"));
            if let Some(u) = s.get_key_url {
                assert!(u.starts_with("https://"), "{}", s.id);
            }
        }
    }

    #[test]
    fn readiness_depends_on_auth_kind() {
        let store = MemoryStore::default();
        let s = statuses(&store, None).unwrap();
        let ready = |id: &str| s.iter().find(|x| x.info.id == id).unwrap().ready;
        assert!(ready("eric") && ready("pubmed"));
        assert!(
            ready("openalex"),
            "keyless OpenAlex works on a smaller budget"
        );
        assert!(!ready("scopus") && !ready("unpaywall") && !ready("core"));

        store
            .set(&key_ref("openalex"), "oa-key-000000007Tx0")
            .unwrap();
        let s = statuses(&store, Some("me@uni.ac.uk")).unwrap();
        let oa = s.iter().find(|x| x.info.id == "openalex").unwrap();
        assert!(oa.ready);
        assert_eq!(oa.key.hint.as_deref(), Some("key ending 7Tx0"));
        assert!(s.iter().find(|x| x.info.id == "unpaywall").unwrap().ready);
        assert!(s
            .iter()
            .find(|x| x.info.id == "scopus")
            .unwrap()
            .inst_token
            .is_some());
    }
}
