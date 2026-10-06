//! Resolve one route per destination before DNS validation or client creation.
//! Explicit web settings override the environment; automatic routing uses the
//! transport's standard proxy and NO_PROXY matcher rather than separate rules.
use std::{fmt, sync::Arc};

use hyper_util::client::proxy::matcher::Matcher;

use super::*;

#[derive(Clone)]
pub struct WebClient {
    pub(super) sandbox: bool,
    explicit: Option<ProxyRoute>,
    automatic: Arc<Matcher>,
}

#[derive(Clone)]
pub(super) struct ProxyRoute {
    pub proxy: reqwest::Proxy,
    pub browser_proxy: Option<Url>,
}

impl fmt::Debug for WebClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WebClient")
            .field("explicit_proxy", &self.explicit.is_some())
            .finish_non_exhaustive()
    }
}

impl Default for WebClient {
    fn default() -> Self {
        Self {
            sandbox: false,
            explicit: None,
            automatic: Arc::new(Matcher::from_env()),
        }
    }
}

impl WebClient {
    pub fn with_sandbox(mut self, sandbox: bool) -> Self {
        self.sandbox = sandbox;
        self
    }

    /// Capture proxy routing for a runtime. All of its web consumers should
    /// share this client, including image previews and browser fallback.
    pub fn new(config: &WebConfig) -> Result<Self, ToolError> {
        let Some(value) = &config.proxy else {
            return Ok(Self::default());
        };
        let url = Url::parse(value).map_err(|_| failed("invalid web.proxy URL"))?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host().is_none()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() != "/"
        {
            return Err(failed(
                "web.proxy must be an HTTP(S) proxy URL without path, query or fragment",
            ));
        }

        let proxy =
            reqwest::Proxy::all(value).map_err(|_| failed("invalid web.proxy configuration"))?;
        Ok(Self {
            sandbox: false,
            explicit: Some(ProxyRoute {
                proxy,
                browser_proxy: browser_proxy(&url),
            }),
            automatic: Arc::new(Matcher::builder().build()),
        })
    }

    pub(super) fn route(&self, url: &Url) -> Result<Option<ProxyRoute>, ToolError> {
        if let Some(route) = &self.explicit {
            return Ok(Some(route.clone()));
        }
        let uri = url
            .as_str()
            .parse()
            .map_err(|_| failed("invalid web request URL"))?;
        let Some(intercept) = self.automatic.intercept(&uri) else {
            return Ok(None);
        };
        let url = Url::parse(&intercept.uri().to_string())
            .map_err(|_| failed("invalid environment proxy URL"))?;
        let mut proxy = reqwest::Proxy::all(url.as_str())
            .map_err(|_| failed("unsupported environment proxy configuration"))?;
        if let Some(auth) = intercept.basic_auth() {
            proxy = proxy.custom_http_auth(auth.clone());
        } else if let Some((user, password)) = intercept.raw_auth() {
            proxy = proxy.basic_auth(user, password);
        }

        Ok(Some(ProxyRoute {
            proxy,
            browser_proxy: if intercept.basic_auth().is_none() && intercept.raw_auth().is_none() {
                browser_proxy(&url)
            } else {
                None
            },
        }))
    }

    #[cfg(test)]
    pub(super) fn for_test(proxy: Option<&reqwest::Proxy>) -> Self {
        Self {
            sandbox: false,
            explicit: proxy.map(|proxy| ProxyRoute {
                proxy: proxy.clone(),
                browser_proxy: None,
            }),
            automatic: Arc::new(Matcher::builder().build()),
        }
    }
}

fn browser_proxy(url: &Url) -> Option<Url> {
    (url.scheme() == "http" && url.username().is_empty() && url.password().is_none())
        .then(|| url.clone())
}

#[cfg(test)]
mod tests;
