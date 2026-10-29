// ABOUTME: The three fetch policies (webhook, allowlist, archive_fetch) and the URL rules each enforces
// ABOUTME: HostAllowlist is the exact host list an operator configures, each entry optionally confined to a path prefix
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::error::Error;
use std::fmt;
use std::net::IpAddr;
use std::time::Duration;

use percent_encoding::percent_decode_str;
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
///
/// An entry may also carry a path prefix, `host/prefix`, such as
/// `s3.amazonaws.com/strava.portability/`: it admits a URL only when the host
/// matches exactly **and** the URL's path lies under the prefix, compared
/// segment by segment, so `/strava.portabilityX/` is not under
/// `/strava.portability/`. A prefix ending in `/` names a directory and admits
/// only what is inside it. Each segment of the URL's path is percent-decoded
/// before the comparison, and a path spelled ambiguously is refused rather
/// than guessed at: a segment that decodes to one containing `/`, `\` or `%`,
/// to a control character, to `.` or `..`, or that is not UTF-8, and an empty
/// segment anywhere but at the end (`//`). `url` has already resolved dot
/// segments and the backslashes of a special scheme before this rule reads the
/// path. A host-only entry admits every path on its host, as it always has.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostAllowlist {
    entries: Vec<AllowedEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AllowedEntry {
    host: AllowedHost,
    path: Option<PathPrefix>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum AllowedHost {
    Domain(String),
    Ip(IpAddr),
}

impl AllowedHost {
    fn matches(&self, host: &Host<&str>) -> bool {
        match (self, host) {
            (Self::Domain(name), Host::Domain(domain)) => {
                let domain = domain.strip_suffix('.').unwrap_or(domain);
                name.eq_ignore_ascii_case(domain)
            }
            (Self::Ip(ip), Host::Ipv4(v4)) => *ip == IpAddr::V4(*v4),
            (Self::Ip(ip), Host::Ipv6(v6)) => *ip == IpAddr::V6(*v6),
            _ => false,
        }
    }
}

/// The path an entry confines its host to: whole segments, compared exactly.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PathPrefix {
    segments: Vec<String>,
    /// The prefix ended in `/`: the URL's path must go past it.
    directory: bool,
}

impl PathPrefix {
    /// The prefix an entry's text after its host spells, or `None` when it
    /// spells none (`host/`). A segment that is empty, a dot segment, or holds
    /// `%`, `\`, `?`, `#` or a control character makes the entry invalid.
    fn parse(text: &str) -> Result<Option<Self>, ()> {
        if text.is_empty() {
            return Ok(None);
        }
        let (text, directory) = text
            .strip_suffix('/')
            .map_or((text, false), |rest| (rest, true));
        let segments = text
            .split('/')
            .map(|segment| {
                let invalid = segment.is_empty()
                    || segment == "."
                    || segment == ".."
                    || segment
                        .chars()
                        .any(|c| matches!(c, '%' | '\\' | '?' | '#') || c.is_control());
                if invalid {
                    Err(())
                } else {
                    Ok(segment.to_owned())
                }
            })
            .collect::<Result<Vec<_>, ()>>()?;
        Ok(Some(Self {
            segments,
            directory,
        }))
    }

    /// Whether the URL path `path` (as `Url::path` serialises it) lies under
    /// the prefix and is spelled unambiguously.
    fn admits(&self, path: &str) -> bool {
        let Some(path) = path.strip_prefix('/') else {
            return false;
        };
        let raw: Vec<&str> = path.split('/').collect();
        let last = raw.len().saturating_sub(1);
        let mut decoded = Vec::with_capacity(raw.len());
        for (index, segment) in raw.iter().enumerate() {
            if segment.is_empty() && index != last {
                return false;
            }
            let Ok(text) = percent_decode_str(segment).decode_utf8() else {
                return false;
            };
            if text == "."
                || text == ".."
                || text
                    .chars()
                    .any(|c| matches!(c, '/' | '\\' | '%') || c.is_control())
            {
                return false;
            }
            decoded.push(text);
        }
        // A trailing empty segment (`/dir/`) names the directory itself.
        if decoded.last().is_some_and(|segment| segment.is_empty()) {
            decoded.pop();
        }
        let enough = if self.directory {
            decoded.len() > self.segments.len()
        } else {
            decoded.len() >= self.segments.len()
        };
        enough
            && self
                .segments
                .iter()
                .zip(&decoded)
                .all(|(expected, actual)| expected == actual)
    }
}

/// An allowlist entry that cannot be read: its path prefix holds an empty or
/// dot segment, or a `%`, `\`, `?`, `#` or control character.
///
/// The entry is the operator's configuration, not a user's URL, so the error
/// names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidAllowlistEntry {
    entry: String,
}

impl fmt::Display for InvalidAllowlistEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "allowlist entry `{}` has an invalid path prefix: segments must be non-empty, \
             not `.` or `..`, and hold no `%`, `\\`, `?`, `#` or control character",
            self.entry
        )
    }
}

impl Error for InvalidAllowlistEntry {}

/// One entry read: `None` when it is empty.
fn parse_entry(entry: &str) -> Result<Option<AllowedEntry>, InvalidAllowlistEntry> {
    let entry = entry.trim();
    let (host, path) = entry
        .split_once('/')
        .map_or((entry, None), |(host, path)| (host, Some(path)));
    let host = host
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(host);
    let host = host.strip_suffix('.').unwrap_or(host);
    if host.is_empty() {
        return Ok(None);
    }
    let path = match path {
        None => None,
        Some(path) => PathPrefix::parse(path).map_err(|()| InvalidAllowlistEntry {
            entry: entry.to_owned(),
        })?,
    };
    let host = host.parse::<IpAddr>().map_or_else(
        |_| AllowedHost::Domain(host.to_ascii_lowercase()),
        AllowedHost::Ip,
    );
    Ok(Some(AllowedEntry { host, path }))
}

impl HostAllowlist {
    /// An allowlist of the given entries.
    ///
    /// Each entry is a host name or an IP address, optionally followed by a
    /// path prefix (`host/prefix/`); surrounding whitespace, the brackets of
    /// an IPv6 literal (`[::1]`) and a trailing dot on the host are removed,
    /// and empty entries are skipped. An entry whose path prefix cannot be
    /// read is skipped too, so it admits nothing: use
    /// [`try_new`](Self::try_new) to refuse it instead.
    pub fn new<I, S>(entries: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let entries = entries
            .into_iter()
            .filter_map(|entry| parse_entry(entry.as_ref()).ok().flatten())
            .collect();
        Self { entries }
    }

    /// An allowlist of the given entries, read as [`new`](Self::new) reads
    /// them.
    ///
    /// # Errors
    ///
    /// [`InvalidAllowlistEntry`] naming the first entry whose path prefix
    /// cannot be read.
    pub fn try_new<I, S>(entries: I) -> Result<Self, InvalidAllowlistEntry>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut parsed = Vec::new();
        for entry in entries {
            if let Some(entry) = parse_entry(entry.as_ref())? {
                parsed.push(entry);
            }
        }
        Ok(Self { entries: parsed })
    }

    /// An allowlist read from a comma-separated list, the shape an
    /// environment variable carries (`llm.example.com, localhost,127.0.0.1`).
    #[must_use]
    pub fn from_comma_separated(text: &str) -> Self {
        Self::new(text.split(','))
    }

    /// An allowlist read from a comma-separated list, as
    /// [`try_new`](Self::try_new) reads its entries.
    ///
    /// # Errors
    ///
    /// [`InvalidAllowlistEntry`] naming the first entry whose path prefix
    /// cannot be read.
    pub fn try_from_comma_separated(text: &str) -> Result<Self, InvalidAllowlistEntry> {
        Self::try_new(text.split(','))
    }

    /// Whether the allowlist has no entries, and so admits no host.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Whether `url`, whose host is `host`, is on the allowlist.
    ///
    /// [`GuardError::PathNotAllowed`] when its host is listed only under path
    /// prefixes it lies outside, [`GuardError::HostNotAllowed`] when its host
    /// is not listed at all.
    fn admits(&self, host: &Host<&str>, url: &Url) -> Result<(), GuardError> {
        let mut listed_under_a_prefix = false;
        for entry in self.entries.iter().filter(|entry| entry.host.matches(host)) {
            match &entry.path {
                None => return Ok(()),
                Some(prefix) if prefix.admits(url.path()) => return Ok(()),
                Some(_) => listed_under_a_prefix = true,
            }
        }
        Err(if listed_under_a_prefix {
            GuardError::PathNotAllowed
        } else {
            GuardError::HostNotAllowed
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
                hosts.admits(&host, url)?;
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

    fn export_bucket() -> FetchPolicy {
        FetchPolicy::archive_fetch(HostAllowlist::new([
            "email.strava.com",
            "s3.amazonaws.com/strava.portability/",
        ]))
    }

    #[test]
    fn a_prefixed_entry_admits_the_path_style_export_url() {
        let policy = export_bucket();
        assert_eq!(
            check(
                &policy,
                "https://s3.amazonaws.com/strava.portability/athlete/1/export/live/export_1.zip?X-Amz-Expires=604800&X-Amz-Signature=abc"
            ),
            Ok(())
        );
        assert_eq!(
            check(
                &policy,
                "https://S3.AmazonAWS.com./strava.portability/a.zip"
            ),
            Ok(()),
            "the host still compares case-insensitively"
        );
        assert_eq!(
            check(
                &policy,
                "https://s3.amazonaws.com/strava%2Eportability/a.zip"
            ),
            Ok(()),
            "an encoded segment that decodes to the prefix is the prefix"
        );
        assert_eq!(
            check(&policy, "https://email.strava.com/ls/click?upn=anything"),
            Ok(()),
            "a host-only entry beside it admits every path"
        );
    }

    #[test]
    fn a_prefixed_entry_refuses_another_bucket_on_the_same_host() {
        let policy = export_bucket();
        for url in [
            "https://s3.amazonaws.com/other-bucket/a.zip",
            "https://s3.amazonaws.com/strava.portabilityX/a.zip",
            "https://s3.amazonaws.com/strava.portability.evil/a.zip",
            "https://s3.amazonaws.com/STRAVA.PORTABILITY/a.zip",
            "https://s3.amazonaws.com/strava.portability",
            "https://s3.amazonaws.com/strava.portability/",
            "https://s3.amazonaws.com/",
        ] {
            assert_eq!(
                check(&policy, url),
                Err(GuardError::PathNotAllowed),
                "{url}"
            );
        }
        assert_eq!(
            check(&policy, "https://strava.portability.s3.amazonaws.com/a.zip"),
            Err(GuardError::HostNotAllowed),
            "a virtual-hosted spelling is another host"
        );
    }

    #[test]
    fn no_spelling_of_the_path_escapes_the_prefix() {
        let policy = export_bucket();
        for url in [
            // Dot segments, plain and encoded: `url` resolves them first.
            "https://s3.amazonaws.com/strava.portability/../other/a.zip",
            "https://s3.amazonaws.com/strava.portability/%2e%2e/other/a.zip",
            "https://s3.amazonaws.com/strava.portability/.%2E/other/a.zip",
            "https://s3.amazonaws.com/strava.portability\\..\\other/a.zip",
            // Separators hidden in an encoding are refused, not guessed at.
            "https://s3.amazonaws.com/strava.portability%2F..%2Fother/a.zip",
            "https://s3.amazonaws.com/strava.portability/..%2Fother/a.zip",
            "https://s3.amazonaws.com/strava.portability/a%5C..%5Cb.zip",
            "https://s3.amazonaws.com/strava.portability/a%252F..%252Fb.zip",
            "https://s3.amazonaws.com/strava.portability/a%00.zip",
            "https://s3.amazonaws.com/strava.portability/%ff.zip",
            // An empty segment, before or inside the prefix.
            "https://s3.amazonaws.com//strava.portability/a.zip",
            "https://s3.amazonaws.com/strava.portability//a.zip",
        ] {
            assert_eq!(
                check(&policy, url),
                Err(GuardError::PathNotAllowed),
                "{url}"
            );
        }
    }

    #[test]
    fn a_prefix_without_a_trailing_slash_admits_the_path_itself() {
        let policy = FetchPolicy::archive_fetch(HostAllowlist::new(["export.test/a/b"]));
        assert_eq!(check(&policy, "https://export.test/a/b"), Ok(()));
        assert_eq!(check(&policy, "https://export.test/a/b/c.zip"), Ok(()));
        assert_eq!(
            check(&policy, "https://export.test/a/bc"),
            Err(GuardError::PathNotAllowed),
            "the prefix ends on a segment boundary"
        );
        assert_eq!(
            check(&policy, "https://export.test/a"),
            Err(GuardError::PathNotAllowed)
        );
    }

    #[test]
    fn host_only_entries_behave_as_before() {
        let hosts = HostAllowlist::new(["export.strava.com", "export.strava.com/"]);
        assert_eq!(
            hosts,
            HostAllowlist::new(["export.strava.com", "export.strava.com"])
        );
        let policy = FetchPolicy::archive_fetch(hosts);
        assert_eq!(check(&policy, "https://export.strava.com/"), Ok(()));
        assert_eq!(
            check(&policy, "https://export.strava.com/any/where/%2F..%2F"),
            Ok(()),
            "a host-only entry reads no path"
        );
        assert_eq!(
            check(&policy, "https://other.strava.com/a.zip"),
            Err(GuardError::HostNotAllowed)
        );
    }

    #[test]
    fn an_unreadable_prefix_is_refused_by_try_and_admits_nothing_otherwise() {
        for entry in [
            "s3.amazonaws.com/a//b/",
            "s3.amazonaws.com/../b/",
            "s3.amazonaws.com/./b",
            "s3.amazonaws.com/a%2Fb/",
            "s3.amazonaws.com/a\\b/",
            "s3.amazonaws.com/a?b",
            "s3.amazonaws.com//",
        ] {
            assert_eq!(
                HostAllowlist::try_new([entry]).map(drop),
                Err(InvalidAllowlistEntry {
                    entry: entry.to_owned()
                }),
                "{entry}"
            );
            assert!(HostAllowlist::new([entry]).is_empty(), "{entry}");
        }
        let parsed = HostAllowlist::try_from_comma_separated(
            " email.strava.com , s3.amazonaws.com/strava.portability/ ,",
        )
        .expect("both entries read"); // Safe: test assertion
        assert_eq!(parsed, export_bucket_hosts());
        let refusal = HostAllowlist::try_from_comma_separated("ok.test, bad.test/a/../b")
            .expect_err("the dot segment is refused"); // Safe: test assertion
        assert_eq!(
            refusal,
            InvalidAllowlistEntry {
                entry: "bad.test/a/../b".to_owned()
            }
        );
        assert!(refusal.to_string().contains("bad.test/a/../b"));
    }

    fn export_bucket_hosts() -> HostAllowlist {
        HostAllowlist::new(["email.strava.com", "s3.amazonaws.com/strava.portability/"])
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
