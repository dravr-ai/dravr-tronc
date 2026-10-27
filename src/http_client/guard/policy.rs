// ABOUTME: The three fetch policies (webhook, allowlist, archive_fetch) and the URL rules each enforces
// ABOUTME: HostAllowlist is the exact, case-insensitive host list an operator or a source configures
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::net::IpAddr;
use std::time::Duration;

use url::{Host, Url};

use super::deny::is_denied_ip;
use super::error::GuardError;

/// The longest redirect chain a policy that follows redirects accepts.
pub const MAX_REDIRECTS: usize = 5;

/// An exact host allowlist: a host matches an entry only when it is that
/// name, compared case-insensitively, or that IP address.
///
/// There is no suffix or wildcard matching, so `api.example.com` on the list
/// does not admit `evil.api.example.com`, and `example.com` does not admit
/// `example.com.evil.org`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostAllowlist {
    entries: Vec<AllowedHost>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum AllowedHost {
    Domain(String),
    Ip(IpAddr),
}

impl HostAllowlist {
    /// An allowlist of the given entries.
    ///
    /// Each entry is a host name or an IP address; surrounding whitespace, the
    /// brackets of an IPv6 literal (`[::1]`) and a trailing dot are removed,
    /// and empty entries are skipped.
    pub fn new<I, S>(entries: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let entries = entries
            .into_iter()
            .filter_map(|entry| {
                let entry = entry.as_ref().trim();
                let entry = entry
                    .strip_prefix('[')
                    .and_then(|rest| rest.strip_suffix(']'))
                    .unwrap_or(entry);
                let entry = entry.strip_suffix('.').unwrap_or(entry);
                if entry.is_empty() {
                    return None;
                }
                Some(entry.parse::<IpAddr>().map_or_else(
                    |_| AllowedHost::Domain(entry.to_ascii_lowercase()),
                    AllowedHost::Ip,
                ))
            })
            .collect();
        Self { entries }
    }

    /// An allowlist read from a comma-separated list, the shape an
    /// environment variable carries (`llm.example.com, localhost,127.0.0.1`).
    #[must_use]
    pub fn from_comma_separated(text: &str) -> Self {
        Self::new(text.split(','))
    }

    /// Whether the allowlist has no entries, and so admits no host.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Whether `host` is on the allowlist.
    fn admits(&self, host: &Host<&str>) -> bool {
        self.entries.iter().any(|entry| match (entry, host) {
            (AllowedHost::Domain(name), Host::Domain(domain)) => {
                let domain = domain.strip_suffix('.').unwrap_or(domain);
                name.eq_ignore_ascii_case(domain)
            }
            (AllowedHost::Ip(ip), Host::Ipv4(v4)) => *ip == IpAddr::V4(*v4),
            (AllowedHost::Ip(ip), Host::Ipv6(v6)) => *ip == IpAddr::V6(*v6),
            _ => false,
        })
    }
}

/// Which rules a guarded fetch applies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchPolicy {
    kind: PolicyKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PolicyKind {
    Webhook,
    Allowlist(HostAllowlist),
    ArchiveFetch(HostAllowlist),
}

impl FetchPolicy {
    /// A URL a client registered to be called back on.
    ///
    /// `http` and `https`, any port, no credentials in the URL, no local-only
    /// name, and no address in the deny list — checked on the literal host and
    /// on every address the name resolves to, at connect time. Redirects are
    /// never followed: a 3xx is returned as the response.
    #[must_use]
    pub fn webhook() -> Self {
        Self {
            kind: PolicyKind::Webhook,
        }
    }

    /// A URL an operator vouches for by listing its host, such as a
    /// self-hosted LLM endpoint.
    ///
    /// `http` and `https`, any port, no credentials, and the host must be on
    /// `hosts`. The address deny list is **not** applied: the operator's list
    /// is the trust decision, and it legitimately names `localhost` for a
    /// model served beside the server. An empty list admits nothing. Each
    /// redirect hop is held to the same rules, up to [`MAX_REDIRECTS`].
    #[must_use]
    pub fn allowlist(hosts: HostAllowlist) -> Self {
        Self {
            kind: PolicyKind::Allowlist(hosts),
        }
    }

    /// A file a user pointed the server at, such as a provider's data export.
    ///
    /// `https` only, the default port only, no credentials, the host must be
    /// on `hosts`, and no address in the deny list. Each redirect hop is held
    /// to the same rules, up to [`MAX_REDIRECTS`], so a listed host cannot
    /// bounce the fetch to an unlisted or internal one.
    #[must_use]
    pub fn archive_fetch(hosts: HostAllowlist) -> Self {
        Self {
            kind: PolicyKind::ArchiveFetch(hosts),
        }
    }

    /// Whether resolved addresses are checked against the deny list.
    pub(crate) const fn applies_deny_list(&self) -> bool {
        !matches!(self.kind, PolicyKind::Allowlist(_))
    }

    /// How many redirect hops the policy follows.
    pub(crate) const fn max_redirects(&self) -> usize {
        match self.kind {
            PolicyKind::Webhook => 0,
            PolicyKind::Allowlist(_) | PolicyKind::ArchiveFetch(_) => MAX_REDIRECTS,
        }
    }

    /// Whether the policy accepts only `https`.
    pub(crate) const fn https_only(&self) -> bool {
        matches!(self.kind, PolicyKind::ArchiveFetch(_))
    }

    /// The caps a client under this policy starts from.
    #[must_use]
    pub const fn default_limits(&self) -> FetchLimits {
        match self.kind {
            PolicyKind::Webhook => FetchLimits {
                max_body_bytes: 1024 * 1024,
                connect_timeout: Duration::from_secs(5),
                idle_timeout: Duration::from_secs(10),
                total_timeout: Duration::from_secs(10),
            },
            PolicyKind::Allowlist(_) => FetchLimits {
                max_body_bytes: 16 * 1024 * 1024,
                connect_timeout: Duration::from_secs(5),
                idle_timeout: Duration::from_secs(30),
                total_timeout: Duration::from_secs(120),
            },
            PolicyKind::ArchiveFetch(_) => FetchLimits {
                max_body_bytes: 1024 * 1024 * 1024,
                connect_timeout: Duration::from_secs(10),
                idle_timeout: Duration::from_secs(60),
                total_timeout: Duration::from_mins(30),
            },
        }
    }

    /// Check `url` against every rule of the policy that needs no DNS:
    /// scheme, credentials, port, host allowlist, local names and a literal
    /// address. A fetch applies the same rules, then checks every address the
    /// host resolves to; use this alone to vet a URL that something other
    /// than a [`GuardedClient`](super::GuardedClient) will call.
    ///
    /// # Errors
    ///
    /// The [`GuardError`] naming the first rule the URL breaks.
    pub fn check_url(&self, url: &str) -> Result<Url, GuardError> {
        let parsed = Url::parse(url).map_err(|_| GuardError::InvalidUrl)?;
        self.check(&parsed, is_denied_ip)?;
        Ok(parsed)
    }

    /// Check a parsed URL against every rule that does not need DNS.
    ///
    /// `refuses` decides whether a literal IP host is denied; it is the same
    /// rule the resolver applies to the addresses a name resolves to.
    pub(crate) fn check(
        &self,
        url: &Url,
        refuses: impl Fn(IpAddr) -> bool,
    ) -> Result<(), GuardError> {
        let scheme_ok = match self.kind {
            PolicyKind::ArchiveFetch(_) => url.scheme() == "https",
            PolicyKind::Webhook | PolicyKind::Allowlist(_) => {
                matches!(url.scheme(), "http" | "https")
            }
        };
        if !scheme_ok {
            return Err(GuardError::SchemeNotAllowed);
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(GuardError::CredentialsInUrl);
        }
        // `Url` drops a port equal to the scheme's default, so any port left
        // is a custom one.
        if matches!(self.kind, PolicyKind::ArchiveFetch(_)) && url.port().is_some() {
            return Err(GuardError::PortNotAllowed);
        }
        let host = url.host().ok_or(GuardError::MissingHost)?;
        match &self.kind {
            PolicyKind::Webhook => {}
            PolicyKind::Allowlist(hosts) | PolicyKind::ArchiveFetch(hosts) => {
                if !hosts.admits(&host) {
                    return Err(GuardError::HostNotAllowed);
                }
            }
        }
        if !self.applies_deny_list() {
            return Ok(());
        }
        let denied = match host {
            Host::Domain(domain) => {
                if is_local_name(domain) {
                    return Err(GuardError::LocalName);
                }
                false
            }
            Host::Ipv4(v4) => refuses(IpAddr::V4(v4)),
            Host::Ipv6(v6) => refuses(IpAddr::V6(v6)),
        };
        if denied {
            return Err(GuardError::DeniedAddress);
        }
        Ok(())
    }
}

/// The caps on one guarded fetch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FetchLimits {
    /// The most response-body bytes read before the fetch fails, counted on
    /// the bytes received, never on a declared `Content-Length`.
    pub max_body_bytes: u64,
    /// The longest a connection attempt may take.
    pub connect_timeout: Duration,
    /// The longest the response may go without delivering a byte.
    pub idle_timeout: Duration,
    /// The longest the whole fetch may take, redirects and body included.
    pub total_timeout: Duration,
}

/// Whether `domain` is a name that only ever means a local or internal host:
/// `localhost`, or a name under `.localhost`, `.local` or `.internal` (where
/// `metadata.google.internal` lives).
fn is_local_name(domain: &str) -> bool {
    let domain = domain.strip_suffix('.').unwrap_or(domain);
    let last_label = domain.rsplit('.').next().unwrap_or(domain);
    ["localhost", "local", "internal"]
        .iter()
        .any(|suffix| last_label.eq_ignore_ascii_case(suffix))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(policy: &FetchPolicy, url: &str) -> Result<(), GuardError> {
        policy.check_url(url).map(drop)
    }

    fn strava() -> HostAllowlist {
        HostAllowlist::new(["export.strava.com"])
    }

    #[test]
    fn archive_fetch_accepts_only_https_on_the_default_port_of_a_listed_host() {
        let policy = FetchPolicy::archive_fetch(strava());
        assert_eq!(check(&policy, "https://export.strava.com/a.zip"), Ok(()));
        assert_eq!(
            check(&policy, "https://export.strava.com:443/a.zip"),
            Ok(())
        );
        assert_eq!(
            check(&policy, "http://export.strava.com/a.zip"),
            Err(GuardError::SchemeNotAllowed)
        );
        assert_eq!(
            check(&policy, "https://export.strava.com:8443/a.zip"),
            Err(GuardError::PortNotAllowed)
        );
        assert_eq!(
            check(&policy, "https://user:pw@export.strava.com/a.zip"),
            Err(GuardError::CredentialsInUrl)
        );
        assert_eq!(
            check(&policy, "https://user@export.strava.com/a.zip"),
            Err(GuardError::CredentialsInUrl)
        );
        assert_eq!(
            check(&policy, "https://evil.export.strava.com/a.zip"),
            Err(GuardError::HostNotAllowed)
        );
        assert_eq!(
            check(&policy, "https://export.strava.com.evil.org/a.zip"),
            Err(GuardError::HostNotAllowed)
        );
        assert_eq!(
            check(&policy, "ftp://export.strava.com/a.zip"),
            Err(GuardError::SchemeNotAllowed)
        );
    }

    #[test]
    fn archive_fetch_refuses_a_listed_ip_in_a_denied_range() {
        let policy = FetchPolicy::archive_fetch(HostAllowlist::new(["169.254.169.254"]));
        assert_eq!(
            check(&policy, "https://169.254.169.254/latest"),
            Err(GuardError::DeniedAddress)
        );
    }

    #[test]
    fn webhook_refuses_denied_literals_and_local_names_in_every_spelling() {
        let policy = FetchPolicy::webhook();
        assert_eq!(check(&policy, "http://hooks.example.com:8080/x"), Ok(()));
        for url in [
            "http://127.0.0.1/",
            "http://2130706433/",
            "http://0x7f.0.0.1/",
            "http://0177.0.0.1/",
            "http://[::1]/",
            "http://[::ffff:169.254.169.254]/",
            "http://[64:ff9b::a9fe:a9fe]/",
            "http://[2002:a9fe:a9fe::]/",
            "http://10.1.2.3/",
            "http://100.64.0.1/",
            "http://198.18.0.1/",
            "http://0.0.0.0/",
        ] {
            assert_eq!(check(&policy, url), Err(GuardError::DeniedAddress), "{url}");
        }
        for url in [
            "http://localhost/",
            "http://LOCALHOST./",
            "http://printer.local/",
            "http://metadata.google.internal/",
            "http://app.localhost/",
        ] {
            assert_eq!(check(&policy, url), Err(GuardError::LocalName), "{url}");
        }
        assert_eq!(
            check(&policy, "file:///etc/passwd"),
            Err(GuardError::SchemeNotAllowed)
        );
    }

    #[test]
    fn allowlist_admits_listed_local_hosts_and_nothing_unlisted() {
        let policy = FetchPolicy::allowlist(HostAllowlist::from_comma_separated(
            " localhost, [::1] ,LLM.Example.com,127.0.0.1",
        ));
        assert_eq!(check(&policy, "http://localhost:11434/v1"), Ok(()));
        assert_eq!(check(&policy, "http://[::1]:11434/v1"), Ok(()));
        assert_eq!(check(&policy, "http://127.0.0.1:8080/v1"), Ok(()));
        assert_eq!(check(&policy, "https://llm.example.COM:8443/v1"), Ok(()));
        assert_eq!(
            check(&policy, "http://169.254.169.254/"),
            Err(GuardError::HostNotAllowed)
        );
        assert_eq!(
            check(&policy, "http://[::ffff:169.254.169.254]/"),
            Err(GuardError::HostNotAllowed)
        );
        assert_eq!(
            check(&policy, "https://evil.example.org/"),
            Err(GuardError::HostNotAllowed)
        );
        assert_eq!(
            check(&policy, "https://user:pw@llm.example.com/"),
            Err(GuardError::CredentialsInUrl)
        );
    }

    #[test]
    fn a_text_that_is_not_an_absolute_url_is_invalid() {
        let policy = FetchPolicy::allowlist(HostAllowlist::new(["not-a-url"]));
        assert_eq!(check(&policy, "not-a-url"), Err(GuardError::InvalidUrl));
        assert_eq!(check(&policy, ""), Err(GuardError::InvalidUrl));
        assert_eq!(
            check(&FetchPolicy::webhook(), "http://"),
            Err(GuardError::InvalidUrl)
        );
    }

    #[test]
    fn an_empty_allowlist_admits_nothing() {
        let empty = HostAllowlist::from_comma_separated(" , ,");
        assert!(empty.is_empty());
        let policy = FetchPolicy::allowlist(empty);
        assert_eq!(
            check(&policy, "http://localhost:11434/"),
            Err(GuardError::HostNotAllowed)
        );
    }

    #[test]
    fn only_the_redirect_following_policies_follow_redirects() {
        assert_eq!(FetchPolicy::webhook().max_redirects(), 0);
        assert_eq!(
            FetchPolicy::allowlist(strava()).max_redirects(),
            MAX_REDIRECTS
        );
        assert_eq!(
            FetchPolicy::archive_fetch(strava()).max_redirects(),
            MAX_REDIRECTS
        );
    }
}
