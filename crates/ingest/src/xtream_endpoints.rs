use crate::{DownloadRequest, IngestError};
use std::fmt;
use url::Url;

const PLAYER_API_FILE: &str = "player_api.php";

/// Credential-free Xtream endpoints for operator output and diagnostics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct XtreamPublicEndpoints {
    pub authentication: Url,
    pub live_categories: Url,
    pub live_streams: Url,
    pub stream_template: String,
}

/// A credential-bearing Xtream stream template.
///
/// Debug output does not include credentials. Create one concrete URL for each
/// stream before endpoint protection.
pub struct XtreamStreamEndpointTemplate {
    base: Url,
    username: String,
    password: String,
    public_template: String,
}

impl XtreamStreamEndpointTemplate {
    /// Create the credential-bearing URL for one provider stream.
    ///
    /// # Errors
    ///
    /// Returns an error if the saved base URL cannot accept path segments.
    pub fn url_for(&self, stream_id: u64) -> Result<Url, IngestError> {
        let mut endpoint = self.base.clone();
        endpoint
            .path_segments_mut()
            .map_err(|()| IngestError::EndpointProtection)?
            .push("live")
            .push(&self.username)
            .push(&self.password)
            .push(&format!("{stream_id}.ts"));
        Ok(endpoint)
    }

    /// Return the credential-free template for operator output.
    #[must_use]
    pub fn public_template(&self) -> &str {
        &self.public_template
    }
}

impl fmt::Debug for XtreamStreamEndpointTemplate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("XtreamStreamEndpointTemplate")
            .field("template", &self.public_template)
            .finish_non_exhaustive()
    }
}

/// Derived Xtream request endpoints and a secret stream template.
///
/// Debug output contains only credential-free endpoint values.
pub struct XtreamEndpoints {
    authentication: Url,
    live_categories: Url,
    live_streams: Url,
    stream_template: XtreamStreamEndpointTemplate,
    public: XtreamPublicEndpoints,
}

impl XtreamEndpoints {
    /// Derive Xtream endpoints from a saved `player_api.php` URL.
    ///
    /// The input must contain one nonempty `username` value and one nonempty
    /// `password` value.
    ///
    /// # Errors
    ///
    /// Returns an error if the URL or its credentials are invalid.
    pub fn from_player_api_url(saved_endpoint: &str) -> Result<Self, IngestError> {
        let endpoint = Url::parse(saved_endpoint)
            .map_err(|_| IngestError::InvalidRequest("Xtream endpoint must be an absolute URL"))?;
        Self::from_url(&endpoint)
    }

    fn from_url(saved_endpoint: &Url) -> Result<Self, IngestError> {
        validate_player_api_url(saved_endpoint)?;
        let (username, password) = query_credentials(saved_endpoint)?;

        let authentication = api_endpoint(saved_endpoint, &username, &password, None);
        let live_categories = api_endpoint(
            saved_endpoint,
            &username,
            &password,
            Some("get_live_categories"),
        );
        let live_streams = api_endpoint(
            saved_endpoint,
            &username,
            &password,
            Some("get_live_streams"),
        );

        let stream_base = stream_base(saved_endpoint);
        let public = public_endpoints(saved_endpoint);
        let stream_template = XtreamStreamEndpointTemplate {
            base: stream_base,
            username,
            password,
            public_template: public.stream_template.clone(),
        };

        Ok(Self {
            authentication,
            live_categories,
            live_streams,
            stream_template,
            public,
        })
    }

    /// Create an authentication request with credential-safe debug output.
    #[must_use]
    pub fn authentication_request(&self) -> DownloadRequest {
        DownloadRequest::new(self.authentication.clone())
    }

    /// Create a live-category request with credential-safe debug output.
    #[must_use]
    pub fn live_categories_request(&self) -> DownloadRequest {
        DownloadRequest::new(self.live_categories.clone())
    }

    /// Create a live-stream request with credential-safe debug output.
    #[must_use]
    pub fn live_streams_request(&self) -> DownloadRequest {
        DownloadRequest::new(self.live_streams.clone())
    }

    /// Create one short EPG request for a live stream.
    #[must_use]
    pub fn short_epg_request(&self, stream_id: u64, limit: u16) -> DownloadRequest {
        let mut endpoint = self.authentication.clone();
        endpoint
            .query_pairs_mut()
            .append_pair("action", "get_short_epg")
            .append_pair("stream_id", &stream_id.to_string())
            .append_pair("limit", &limit.max(1).to_string());
        DownloadRequest::new(endpoint)
    }

    /// Return the secret template that creates concrete stream URLs.
    #[must_use]
    pub const fn stream_template(&self) -> &XtreamStreamEndpointTemplate {
        &self.stream_template
    }

    /// Consume the endpoint set and return its secret stream template.
    #[must_use]
    pub fn into_stream_template(self) -> XtreamStreamEndpointTemplate {
        self.stream_template
    }

    /// Return credential-free endpoints for operator output.
    #[must_use]
    pub const fn public(&self) -> &XtreamPublicEndpoints {
        &self.public
    }
}

impl fmt::Debug for XtreamEndpoints {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("XtreamEndpoints")
            .field("authentication", &self.public.authentication)
            .field("live_categories", &self.public.live_categories)
            .field("live_streams", &self.public.live_streams)
            .field("stream_template", &self.public.stream_template)
            .finish()
    }
}

fn validate_player_api_url(endpoint: &Url) -> Result<(), IngestError> {
    if !matches!(endpoint.scheme(), "http" | "https") || endpoint.host_str().is_none() {
        return Err(IngestError::InvalidRequest(
            "Xtream endpoint must use HTTP or HTTPS and include a host",
        ));
    }
    if !endpoint.username().is_empty() || endpoint.password().is_some() {
        return Err(IngestError::InvalidRequest(
            "Xtream endpoint must use query credentials",
        ));
    }
    if endpoint.fragment().is_some() {
        return Err(IngestError::InvalidRequest(
            "Xtream endpoint must not include a fragment",
        ));
    }
    if endpoint.path_segments().and_then(Iterator::last) != Some(PLAYER_API_FILE) {
        return Err(IngestError::InvalidRequest(
            "Xtream endpoint path must end with player_api.php",
        ));
    }
    Ok(())
}

fn query_credentials(endpoint: &Url) -> Result<(String, String), IngestError> {
    let mut username = None;
    let mut password = None;
    for (key, value) in endpoint.query_pairs() {
        match key.as_ref() {
            "username" => set_credential(&mut username, value.into_owned())?,
            "password" => set_credential(&mut password, value.into_owned())?,
            _ => {}
        }
    }
    let username =
        username
            .filter(|value| !value.is_empty())
            .ok_or(IngestError::InvalidRequest(
                "Xtream endpoint requires a nonempty username",
            ))?;
    let password =
        password
            .filter(|value| !value.is_empty())
            .ok_or(IngestError::InvalidRequest(
                "Xtream endpoint requires a nonempty password",
            ))?;
    Ok((username, password))
}

fn set_credential(target: &mut Option<String>, value: String) -> Result<(), IngestError> {
    if target.replace(value).is_some() {
        return Err(IngestError::InvalidRequest(
            "Xtream endpoint contains duplicate credentials",
        ));
    }
    Ok(())
}

fn api_endpoint(saved_endpoint: &Url, username: &str, password: &str, action: Option<&str>) -> Url {
    let mut endpoint = saved_endpoint.clone();
    endpoint.set_query(None);
    {
        let mut query = endpoint.query_pairs_mut();
        query.append_pair("username", username);
        query.append_pair("password", password);
        if let Some(action) = action {
            query.append_pair("action", action);
        }
    }
    endpoint
}

fn stream_base(saved_endpoint: &Url) -> Url {
    let mut base = saved_endpoint.clone();
    base.set_query(None);
    base.set_fragment(None);
    base.path_segments_mut()
        .expect("an HTTP URL always supports path segments")
        .pop();
    base
}

fn public_endpoints(saved_endpoint: &Url) -> XtreamPublicEndpoints {
    let mut authentication = saved_endpoint.clone();
    authentication.set_query(None);

    let mut live_categories = authentication.clone();
    live_categories
        .query_pairs_mut()
        .append_pair("action", "get_live_categories");

    let mut live_streams = authentication.clone();
    live_streams
        .query_pairs_mut()
        .append_pair("action", "get_live_streams");

    let mut public_stream = stream_base(saved_endpoint);
    public_stream
        .path_segments_mut()
        .expect("an HTTP URL always supports path segments")
        .push("live")
        .push("IPTV_STREAM_ID.ts");
    let stream_template = public_stream
        .as_str()
        .replace("IPTV_STREAM_ID.ts", "{stream_id}.ts");

    XtreamPublicEndpoints {
        authentication,
        live_categories,
        live_streams,
        stream_template,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAVED: &str =
        "https://provider.test:8443/panel/player_api.php?username=alice&password=s3cret";

    #[test]
    fn derives_api_requests_and_concrete_stream_urls() {
        let endpoints = XtreamEndpoints::from_player_api_url(SAVED).expect("valid Xtream URL");

        assert_eq!(
            endpoints.authentication_request().endpoint().as_str(),
            SAVED
        );
        assert_eq!(
            endpoints.live_categories_request().endpoint().as_str(),
            "https://provider.test:8443/panel/player_api.php?username=alice&password=s3cret&action=get_live_categories"
        );
        assert_eq!(
            endpoints.live_streams_request().endpoint().as_str(),
            "https://provider.test:8443/panel/player_api.php?username=alice&password=s3cret&action=get_live_streams"
        );
        assert_eq!(
            endpoints
                .stream_template()
                .url_for(42)
                .expect("concrete stream URL")
                .as_str(),
            "https://provider.test:8443/panel/live/alice/s3cret/42.ts"
        );
    }

    #[test]
    fn percent_encodes_credentials_as_path_segments() {
        let endpoints = XtreamEndpoints::from_player_api_url(
            "http://provider.test/player_api.php?username=user%2Fname&password=p%3Fa%23ss",
        )
        .expect("encoded credentials");

        assert_eq!(
            endpoints
                .stream_template()
                .url_for(7)
                .expect("concrete stream URL")
                .as_str(),
            "http://provider.test/live/user%2Fname/p%3Fa%23ss/7.ts"
        );
    }

    #[test]
    fn replaces_untrusted_action_and_ignores_unrelated_query_values() {
        let endpoints = XtreamEndpoints::from_player_api_url(
            "https://provider.test/player_api.php?action=wrong&username=user&token=ignored&password=pass",
        )
        .expect("valid Xtream URL");

        assert_eq!(
            endpoints.live_streams_request().endpoint().as_str(),
            "https://provider.test/player_api.php?username=user&password=pass&action=get_live_streams"
        );
    }

    #[test]
    fn derives_bounded_short_epg_requests() {
        let endpoints = XtreamEndpoints::from_player_api_url(SAVED).expect("valid Xtream URL");

        assert_eq!(
            endpoints.short_epg_request(42, 0).endpoint().as_str(),
            "https://provider.test:8443/panel/player_api.php?username=alice&password=s3cret&action=get_short_epg&stream_id=42&limit=1"
        );
        assert_eq!(
            endpoints.short_epg_request(42, 3).endpoint().as_str(),
            "https://provider.test:8443/panel/player_api.php?username=alice&password=s3cret&action=get_short_epg&stream_id=42&limit=3"
        );
        let debug = format!("{:?}", endpoints.short_epg_request(42, 3));
        assert!(!debug.contains("alice"));
        assert!(!debug.contains("s3cret"));
    }

    #[test]
    fn public_and_debug_values_exclude_all_credentials() {
        let endpoints = XtreamEndpoints::from_player_api_url(SAVED).expect("valid Xtream URL");
        let public = endpoints.public();

        assert_eq!(
            public.authentication.as_str(),
            "https://provider.test:8443/panel/player_api.php"
        );
        assert_eq!(
            public.live_categories.as_str(),
            "https://provider.test:8443/panel/player_api.php?action=get_live_categories"
        );
        assert_eq!(
            public.live_streams.as_str(),
            "https://provider.test:8443/panel/player_api.php?action=get_live_streams"
        );
        assert_eq!(
            public.stream_template,
            "https://provider.test:8443/panel/live/{stream_id}.ts"
        );

        let debug_values = [
            format!("{endpoints:?}"),
            format!("{:?}", endpoints.stream_template()),
            format!("{:?}", endpoints.authentication_request()),
            format!("{:?}", endpoints.live_categories_request()),
            format!("{:?}", endpoints.live_streams_request()),
        ];
        for debug in debug_values {
            assert!(!debug.contains("alice"));
            assert!(!debug.contains("s3cret"));
        }
    }

    #[test]
    fn supports_ipv6_hosts() {
        let endpoints = XtreamEndpoints::from_player_api_url(
            "http://[2001:db8::1]:8080/player_api.php?username=user&password=pass",
        )
        .expect("IPv6 Xtream URL");

        assert_eq!(
            endpoints
                .stream_template()
                .url_for(9)
                .expect("concrete stream URL")
                .as_str(),
            "http://[2001:db8::1]:8080/live/user/pass/9.ts"
        );
        assert_eq!(
            endpoints.public().stream_template,
            "http://[2001:db8::1]:8080/live/{stream_id}.ts"
        );
    }

    #[test]
    fn rejects_invalid_or_ambiguous_saved_endpoints_without_secret_errors() {
        let cases = [
            "not-a-url",
            "ftp://provider.test/player_api.php?username=private-user&password=private-password",
            "https:///player_api.php?username=private-user&password=private-password",
            "https://authority-user:authority-password@provider.test/player_api.php?username=private-user&password=private-password",
            "https://provider.test/get.php?username=private-user&password=private-password",
            "https://provider.test/player_api.php#username=private-user&password=private-password",
            "https://provider.test/player_api.php?password=private-password",
            "https://provider.test/player_api.php?username=private-user",
            "https://provider.test/player_api.php?username=&password=private-password",
            "https://provider.test/player_api.php?username=private-user&password=",
            "https://provider.test/player_api.php?username=first-private-user&username=second-private-user&password=private-password",
            "https://provider.test/player_api.php?username=private-user&password=first-private-password&password=second-private-password",
        ];

        for case in cases {
            let error = XtreamEndpoints::from_player_api_url(case).expect_err("invalid endpoint");
            let output = format!("{error:?} {error}");
            for secret in [
                "private-user",
                "private-password",
                "authority-user",
                "authority-password",
                "first-private-user",
                "second-private-user",
                "first-private-password",
                "second-private-password",
            ] {
                assert!(
                    !output.contains(secret),
                    "secret occurred in error: {output}"
                );
            }
        }
    }
}
