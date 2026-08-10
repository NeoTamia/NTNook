//! Caddy Admin API integration and canonical proxy targets.
#![allow(dead_code)]

use std::fmt;
use url::Url;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Error {
    InvalidPort,
    InvalidUrl(String),
    UnsupportedScheme,
    MissingHost,
    Credentials,
    Path,
    Query,
    Fragment,
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPort => write!(formatter, "alias target port must be between 1 and 65535"),
            Self::InvalidUrl(reason) => write!(formatter, "invalid alias target URL: {reason}"),
            Self::UnsupportedScheme => {
                write!(formatter, "alias target scheme must be http or https")
            }
            Self::MissingHost => write!(formatter, "alias target URL must contain a host"),
            Self::Credentials => write!(formatter, "alias target URL must not contain credentials"),
            Self::Path => write!(formatter, "alias target URL path must be empty or `/`"),
            Self::Query => write!(formatter, "alias target URL must not contain a query"),
            Self::Fragment => write!(formatter, "alias target URL must not contain a fragment"),
        }
    }
}

impl std::error::Error for Error {}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Upstream {
    pub(crate) url: Url,
}

pub(crate) fn normalize_upstream(target: &str) -> Result<Upstream, Error> {
    if target.bytes().all(|byte| byte.is_ascii_digit()) {
        let port: u16 = target.parse().map_err(|_| Error::InvalidPort)?;
        if port == 0 {
            return Err(Error::InvalidPort);
        }
        return Ok(Upstream {
            url: Url::parse(&format!("http://127.0.0.1:{port}"))
                .expect("generated loopback URL is valid"),
        });
    }
    let url = Url::parse(target).map_err(|error| Error::InvalidUrl(error.to_string()))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(Error::UnsupportedScheme);
    }
    if url.host().is_none() {
        return Err(Error::MissingHost);
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(Error::Credentials);
    }
    if url.path() != "/" {
        return Err(Error::Path);
    }
    if url.query().is_some() {
        return Err(Error::Query);
    }
    if url.fragment().is_some() {
        return Err(Error::Fragment);
    }
    Ok(Upstream { url })
}

#[cfg(test)]
mod tests {
    use super::{Error, normalize_upstream};

    #[test]
    fn port_becomes_loopback_http_url() {
        assert_eq!(
            normalize_upstream("3000").unwrap().url.as_str(),
            "http://127.0.0.1:3000/"
        );
    }

    #[test]
    fn accepts_http_and_https_hosts_on_any_network() {
        for target in [
            "http://127.0.0.1:8080",
            "http://192.168.1.2",
            "https://example.com",
        ] {
            assert!(normalize_upstream(target).is_ok(), "{target}");
        }
    }

    #[test]
    fn rejects_each_forbidden_url_component_precisely() {
        assert_eq!(
            normalize_upstream("https://user@example.com"),
            Err(Error::Credentials)
        );
        assert_eq!(
            normalize_upstream("https://example.com/api"),
            Err(Error::Path)
        );
        assert_eq!(
            normalize_upstream("https://example.com/?x=1"),
            Err(Error::Query)
        );
        assert_eq!(
            normalize_upstream("https://example.com/#top"),
            Err(Error::Fragment)
        );
        assert_eq!(
            normalize_upstream("ftp://example.com"),
            Err(Error::UnsupportedScheme)
        );
        assert_eq!(normalize_upstream("0"), Err(Error::InvalidPort));
        assert_eq!(normalize_upstream("70000"), Err(Error::InvalidPort));
    }
}
