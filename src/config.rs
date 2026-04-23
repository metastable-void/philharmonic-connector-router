use std::{
    collections::HashMap,
    sync::atomic::{AtomicUsize, Ordering},
};

use axum::http::Uri;

/// Configuration and realm-to-upstream routing table.
#[derive(Debug, Default)]
pub struct DispatchConfig {
    domain_suffix: String,
    upstreams_by_realm: HashMap<String, UpstreamPool>,
}

#[derive(Debug)]
struct UpstreamPool {
    upstreams: Vec<Uri>,
    next_index: AtomicUsize,
}

impl UpstreamPool {
    fn new(upstreams: Vec<Uri>) -> Self {
        Self {
            upstreams,
            next_index: AtomicUsize::new(0),
        }
    }

    fn select_round_robin(&self) -> Uri {
        let index = self.next_index.fetch_add(1, Ordering::Relaxed);
        self.upstreams[index % self.upstreams.len()].clone()
    }
}

/// Dispatch configuration validation and lookup errors.
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum DispatchConfigError {
    /// Domain suffix must be non-empty.
    #[error("domain suffix must not be empty")]
    EmptyDomainSuffix,

    /// Realm name must be non-empty and must not contain dots.
    #[error("realm name '{realm}' is invalid")]
    InvalidRealmName { realm: String },

    /// At least one upstream URI is required per realm.
    #[error("realm '{realm}' has no configured upstreams")]
    EmptyRealmUpstreams { realm: String },

    /// Upstream URI must include a scheme and authority.
    #[error("realm '{realm}' upstream URI '{uri}' must include scheme and authority")]
    InvalidUpstreamUri { realm: String, uri: String },

    /// Request host does not match `<realm>.connector.<domain>`.
    #[error("request host '{host}' does not match expected connector domain")]
    HostMismatch { host: String },

    /// No upstreams configured for the resolved realm.
    #[error("no upstream configured for realm '{realm}'")]
    UnknownRealm { realm: String },
}

impl DispatchConfig {
    /// Construct an empty dispatch config for one connector domain suffix.
    pub fn new(domain_suffix: impl Into<String>) -> Result<Self, DispatchConfigError> {
        let domain_suffix = normalize_domain_suffix(domain_suffix.into());
        if domain_suffix.is_empty() {
            return Err(DispatchConfigError::EmptyDomainSuffix);
        }

        Ok(Self {
            domain_suffix,
            upstreams_by_realm: HashMap::new(),
        })
    }

    /// Insert or replace one realm's upstream pool.
    pub fn insert_realm(
        &mut self,
        realm: impl Into<String>,
        upstreams: Vec<Uri>,
    ) -> Result<(), DispatchConfigError> {
        let realm = realm.into();
        if realm.is_empty() || realm.contains('.') {
            return Err(DispatchConfigError::InvalidRealmName { realm });
        }

        if upstreams.is_empty() {
            return Err(DispatchConfigError::EmptyRealmUpstreams { realm });
        }

        for upstream in &upstreams {
            if upstream.scheme().is_none() || upstream.authority().is_none() {
                return Err(DispatchConfigError::InvalidUpstreamUri {
                    realm: realm.clone(),
                    uri: upstream.to_string(),
                });
            }
        }

        self.upstreams_by_realm
            .insert(realm.to_lowercase(), UpstreamPool::new(upstreams));
        Ok(())
    }

    /// Resolve one request host and select one upstream URI (round-robin).
    pub fn select_upstream_for_host(&self, host: &str) -> Result<Uri, DispatchConfigError> {
        let normalized_host = normalize_host(host);
        let realm = self.extract_realm(&normalized_host).ok_or_else(|| {
            DispatchConfigError::HostMismatch {
                host: host.to_owned(),
            }
        })?;

        let pool = self.upstreams_by_realm.get(realm).ok_or_else(|| {
            DispatchConfigError::UnknownRealm {
                realm: realm.to_owned(),
            }
        })?;

        Ok(pool.select_round_robin())
    }

    fn extract_realm<'a>(&'a self, normalized_host: &'a str) -> Option<&'a str> {
        let suffix = format!(".connector.{}", self.domain_suffix);
        if !normalized_host.ends_with(&suffix) {
            return None;
        }

        let realm = &normalized_host[..normalized_host.len().saturating_sub(suffix.len())];
        if realm.is_empty() || realm.contains('.') {
            return None;
        }

        Some(realm)
    }
}

fn normalize_host(host: &str) -> String {
    let without_port = host.split(':').next().unwrap_or(host);
    without_port.trim().trim_end_matches('.').to_lowercase()
}

fn normalize_domain_suffix(domain_suffix: String) -> String {
    domain_suffix
        .trim()
        .trim_start_matches('.')
        .trim_end_matches('.')
        .to_lowercase()
}
