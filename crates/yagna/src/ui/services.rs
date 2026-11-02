//! The app's network side: the one runtime, the anonymous forge the
//! images go through, and the authenticated forge the tab's own
//! requests go through.
//!
//! Two clients, two purposes: a README can point at any host on the
//! internet, so the images never carry the user's token; the
//! repository's own structure and history do, so they go through the
//! authenticated client.
//!
//! Items: `Services` — the runtime and the two forges.
//! Depends on: yagna_forge (the runtime and the client).
//! Called by: crate::ui (the tab's workers).

use yagna_forge::{ForgeHost, GithubForge};

/// The app's network side.
pub struct Services {
    /// The one runtime — none when the runtime could not start, and
    /// then the app works offline. It is shared: the pre-fetch runs on
    /// its own thread and drives the same runtime.
    pub host: Option<std::sync::Arc<ForgeHost>>,
    /// The token the authenticated forge carries, kept for the
    /// pre-fetch.
    token: Option<String>,
    /// The anonymous forge: the images the READMEs point at.
    pub assets: Option<GithubForge>,
    /// The authenticated forge: the structure and the first commit.
    pub api: Option<GithubForge>,
}

impl Services {
    /// Opens the network side. Without a token the authenticated forge
    /// is absent — the tab says so instead of pretending.
    pub(crate) fn open(token: Option<&str>) -> Self {
        let Ok(host) = ForgeHost::new() else {
            return Self {
                host: None,
                token: None,
                assets: None,
                api: None,
            };
        };
        let assets = host.forge(GithubForge::anonymous).ok();
        let api = token.and_then(|token| host.forge(|| GithubForge::new(token)).ok());
        Self {
            host: Some(std::sync::Arc::new(host)),
            token: token.map(|token| token.to_string()),
            assets,
            api,
        }
    }

    /// The token the pre-fetch authenticates with.
    pub(crate) fn token(&self) -> Option<&str> {
        self.token.as_deref()
    }
}
