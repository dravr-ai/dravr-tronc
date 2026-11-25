// ABOUTME: DPoP (RFC 9449) proof verification for a resource or authorization server, and RFC 7638 thumbprints
// ABOUTME: Strings in, tronc types out: no jsonwebtoken type crosses the public surface, so callers survive its majors
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

//! `DPoP` proof verification (RFC 9449).
//!
//! A `DPoP` proof is a JWT the client signs, per request, with a key whose
//! public half rides in the proof's own header. A token bound to that key
//! (`cnf.jkt`, the key's RFC 7638 thumbprint) is worthless to anyone who
//! steals it without the private key.
//!
//! [`DpopVerifier::verify`] runs every check RFC 9449 §4.3 asks of a server
//! that a library can run alone: the header's `typ`, `alg` and public `jwk`,
//! the signature, `htm`, `htu`, the `iat` window, the presence of `jti`, and
//! — when a token or a nonce is presented — `ath` and `nonce`. It returns the
//! key's thumbprint, which the caller compares with the token's `cnf.jkt`
//! (or stores on the token it is about to mint), and the proof's `jti` and
//! `iat`.
//!
//! **Replay is the caller's.** RFC 9449 §11.1 has a server remember every
//! `jti` it accepted for as long as the proof's `iat` window lets it be
//! replayed. Only the caller knows whether that memory must be shared across
//! instances, so [`VerifiedProof`] hands back what it needs to keep, and the
//! verifier stays stateless.
//!
//! The public surface is strings in and tronc types out. The JWS stack under
//! it is an implementation detail, so a major of that stack is never a major
//! of this module.

use std::collections::HashSet;
use std::error::Error;
use std::fmt;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use jsonwebtoken::jwk::Jwk;
use jsonwebtoken::{decode, Algorithm, DecodingKey, Validation};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::dates::Timestamp;

/// The `typ` every `DPoP` proof header carries (RFC 9449 §4.2).
pub const DPOP_PROOF_TYPE: &str = "dpop+jwt";

/// The HTTP header a `DPoP` proof travels in (RFC 9449 §4.1).
pub const DPOP_HEADER: &str = "dpop";

/// The `alg` values a verifier accepts unless told otherwise: the asymmetric
/// algorithms RFC 9449 §4.2 allows that the JWS stack verifies.
pub const DEFAULT_SIGNING_ALGS: [&str; 5] = ["ES256", "ES384", "RS256", "PS256", "EdDSA"];

/// How far in the past a proof's `iat` may lie, by default: five minutes,
/// the window RFC 9449 §11.1 has servers keep `jti`s for.
pub const DEFAULT_MAX_AGE_SECS: i64 = 300;

/// How far in the future a proof's `iat` may lie, by default, for client
/// clock skew.
pub const DEFAULT_LEEWAY_SECS: i64 = 60;

/// JWK members that only a private key carries (RFC 7518 §6.2.2, §6.3.2,
/// §6.4). A proof whose `jwk` has any of them is refused: RFC 9449 §4.2
/// requires the public key, and a client that sends its private key has
/// leaked it.
const PRIVATE_JWK_MEMBERS: [&str; 8] = ["d", "p", "q", "dp", "dq", "qi", "oth", "k"];

/// Why a `DPoP` proof was refused. Every variant maps to RFC 9449's
/// `invalid_dpop_proof`, except [`Self::NonceRequired`], which maps to
/// `use_dpop_nonce`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum DpopError {
    /// The proof is not a three-part compact JWS, or a part is not base64url
    /// JSON.
    Malformed,
    /// The header's `typ` is not `dpop+jwt`.
    WrongType,
    /// The header's `alg` is missing, symmetric, `none`, or not allowed.
    UnsupportedAlgorithm,
    /// The header carries no `jwk`, a `jwk` the stack cannot read, or a
    /// private key.
    InvalidKey,
    /// The signature does not verify under the header's `jwk`.
    BadSignature,
    /// A required claim (`jti`, `htm`, `htu`, `iat`) is missing or mistyped.
    MissingClaim(&'static str),
    /// `htm` is not the request's method.
    MethodMismatch,
    /// `htu` is not the request's URI.
    UriMismatch,
    /// `iat` lies outside the accepted window.
    Stale,
    /// A token was presented and `ath` is missing or is not its hash.
    AccessTokenHashMismatch,
    /// The server requires a nonce and the proof carries none, or another.
    NonceRequired,
}

impl DpopError {
    /// The RFC 9449 error code a response names: `use_dpop_nonce` for a
    /// missing or wrong nonce, `invalid_dpop_proof` for everything else.
    #[must_use]
    pub const fn error_code(&self) -> &'static str {
        match self {
            Self::NonceRequired => "use_dpop_nonce",
            _ => "invalid_dpop_proof",
        }
    }
}

impl fmt::Display for DpopError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed => f.write_str("DPoP proof is not a compact JWS"),
            Self::WrongType => f.write_str("DPoP proof typ is not dpop+jwt"),
            Self::UnsupportedAlgorithm => f.write_str("DPoP proof alg is not accepted"),
            Self::InvalidKey => f.write_str("DPoP proof jwk is missing, unreadable or private"),
            Self::BadSignature => f.write_str("DPoP proof signature does not verify"),
            Self::MissingClaim(claim) => write!(f, "DPoP proof claim {claim} is missing"),
            Self::MethodMismatch => f.write_str("DPoP proof htm does not match the request"),
            Self::UriMismatch => f.write_str("DPoP proof htu does not match the request"),
            Self::Stale => f.write_str("DPoP proof iat is outside the accepted window"),
            Self::AccessTokenHashMismatch => {
                f.write_str("DPoP proof ath does not match the access token")
            }
            Self::NonceRequired => f.write_str("DPoP proof nonce is missing or not current"),
        }
    }
}

impl Error for DpopError {}

/// The request a proof must have been made for.
#[derive(Debug, Clone, Copy)]
pub struct DpopRequest<'a> {
    /// The HTTP method, compared exactly (`POST`, `GET`, …).
    pub htm: &'a str,
    /// The request URI as the client dialed it, which only the caller knows
    /// behind a proxy. Its query and fragment are ignored, on both sides
    /// (RFC 9449 §4.3 step 9).
    pub htu: &'a str,
    /// The access token the request presents, for the `ath` check; `None` at
    /// a token endpoint, where the proof binds a token not yet minted.
    pub access_token: Option<&'a str>,
    /// The nonce the server last issued this client; `None` when the server
    /// issues none.
    pub nonce: Option<&'a str>,
}

/// What a verified proof establishes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedProof {
    /// The RFC 7638 SHA-256 thumbprint of the proof's key, base64url: what a
    /// bound token carries as `cnf.jkt`.
    pub jkt: String,
    /// The proof's unique id, for the caller's replay memory.
    pub jti: String,
    /// When the client says it made the proof; the replay memory keeps `jti`
    /// until `iat` plus the verifier's maximum age.
    pub iat: Timestamp,
}

/// Verifies `DPoP` proofs against a set of accepted algorithms and an `iat`
/// window.
#[derive(Debug, Clone)]
pub struct DpopVerifier {
    algorithms: Vec<Algorithm>,
    max_age_secs: i64,
    leeway_secs: i64,
}

impl Default for DpopVerifier {
    fn default() -> Self {
        Self::new(&DEFAULT_SIGNING_ALGS)
    }
}

impl DpopVerifier {
    /// A verifier accepting the named algorithms. A name the stack does not
    /// know, and every symmetric algorithm, is dropped: a proof is always
    /// signed with a private key.
    #[must_use]
    pub fn new(algs: &[&str]) -> Self {
        let algorithms = algs
            .iter()
            .filter_map(|name| asymmetric_algorithm(name))
            .collect();
        Self {
            algorithms,
            max_age_secs: DEFAULT_MAX_AGE_SECS,
            leeway_secs: DEFAULT_LEEWAY_SECS,
        }
    }

    /// How old a proof's `iat` may be, in seconds.
    #[must_use]
    pub const fn with_max_age(mut self, secs: i64) -> Self {
        self.max_age_secs = secs;
        self
    }

    /// How far ahead of now a proof's `iat` may be, in seconds.
    #[must_use]
    pub const fn with_leeway(mut self, secs: i64) -> Self {
        self.leeway_secs = secs;
        self
    }

    /// The maximum age, which a replay memory keeps each `jti` for.
    #[must_use]
    pub const fn max_age_secs(&self) -> i64 {
        self.max_age_secs
    }

    /// The algorithm names this verifier accepts, for a server's metadata
    /// (`dpop_signing_alg_values_supported`).
    #[must_use]
    pub fn algorithm_names(&self) -> Vec<String> {
        self.algorithms
            .iter()
            .map(|alg| format!("{alg:?}"))
            .collect()
    }

    /// Verify `proof` for `request` at `now`.
    ///
    /// # Errors
    ///
    /// The first check the proof fails, as a [`DpopError`].
    pub fn verify(
        &self,
        proof: &str,
        request: &DpopRequest<'_>,
        now: Timestamp,
    ) -> Result<VerifiedProof, DpopError> {
        let header = proof_header(proof)?;
        if header.get("typ").and_then(Value::as_str) != Some(DPOP_PROOF_TYPE) {
            return Err(DpopError::WrongType);
        }
        let algorithm = header
            .get("alg")
            .and_then(Value::as_str)
            .and_then(asymmetric_algorithm)
            .filter(|alg| self.algorithms.contains(alg))
            .ok_or(DpopError::UnsupportedAlgorithm)?;
        let jwk_value = header.get("jwk").ok_or(DpopError::InvalidKey)?;
        let jkt = jwk_thumbprint(jwk_value)?;
        let jwk: Jwk =
            serde_json::from_value(jwk_value.clone()).map_err(|_| DpopError::InvalidKey)?;
        let key = DecodingKey::from_jwk(&jwk).map_err(|_| DpopError::InvalidKey)?;

        let mut validation = Validation::new(algorithm);
        validation.validate_exp = false;
        validation.validate_aud = false;
        validation.validate_nbf = false;
        validation.required_spec_claims = HashSet::new();
        let claims = decode::<Map<String, Value>>(proof, &key, &validation)
            .map_err(|_| DpopError::BadSignature)?
            .claims;

        let jti = string_claim(&claims, "jti")?;
        let htm = string_claim(&claims, "htm")?;
        let htu = string_claim(&claims, "htu")?;
        let iat_secs = claims
            .get("iat")
            .and_then(Value::as_i64)
            .ok_or(DpopError::MissingClaim("iat"))?;

        if htm != request.htm {
            return Err(DpopError::MethodMismatch);
        }
        if without_query(htu) != without_query(request.htu) {
            return Err(DpopError::UriMismatch);
        }
        let now_secs = now.epoch_seconds();
        if iat_secs < now_secs.saturating_sub(self.max_age_secs)
            || iat_secs > now_secs.saturating_add(self.leeway_secs)
        {
            return Err(DpopError::Stale);
        }
        let iat = Timestamp::from_epoch_seconds(iat_secs).map_err(|_| DpopError::Stale)?;
        if let Some(token) = request.access_token {
            let expected = URL_SAFE_NO_PAD.encode(Sha256::digest(token.as_bytes()));
            if claims.get("ath").and_then(Value::as_str) != Some(expected.as_str()) {
                return Err(DpopError::AccessTokenHashMismatch);
            }
        }
        if let Some(nonce) = request.nonce {
            if claims.get("nonce").and_then(Value::as_str) != Some(nonce) {
                return Err(DpopError::NonceRequired);
            }
        }
        Ok(VerifiedProof {
            jkt,
            jti: jti.to_owned(),
            iat,
        })
    }
}

/// The RFC 7638 SHA-256 thumbprint of a public JWK, base64url without
/// padding: the value a DPoP-bound token carries as `cnf.jkt`.
///
/// Computed over the key type's required members only, in lexicographic
/// order (RFC 7638 §3.2): `crv, kty, x, y` for EC, `e, kty, n` for RSA and
/// `crv, kty, x` for OKP.
///
/// # Errors
///
/// [`DpopError::InvalidKey`] for a key that is not an object, carries a
/// private member, has another `kty`, or lacks a required member.
pub fn jwk_thumbprint(jwk: &Value) -> Result<String, DpopError> {
    let object = jwk.as_object().ok_or(DpopError::InvalidKey)?;
    if PRIVATE_JWK_MEMBERS
        .iter()
        .any(|member| object.contains_key(*member))
    {
        return Err(DpopError::InvalidKey);
    }
    let required: &[&str] = match object.get("kty").and_then(Value::as_str) {
        Some("EC") => &["crv", "kty", "x", "y"],
        Some("RSA") => &["e", "kty", "n"],
        Some("OKP") => &["crv", "kty", "x"],
        _ => return Err(DpopError::InvalidKey),
    };
    // serde_json::Map is ordered by key unless preserve_order is on, which a
    // dependent may switch on; build the canonical form by hand instead.
    let mut canonical = String::from("{");
    for (index, member) in required.iter().enumerate() {
        let value = object
            .get(*member)
            .and_then(Value::as_str)
            .ok_or(DpopError::InvalidKey)?;
        if index > 0 {
            canonical.push(',');
        }
        canonical.push_str(&Value::String((*member).to_owned()).to_string());
        canonical.push(':');
        canonical.push_str(&Value::String(value.to_owned()).to_string());
    }
    canonical.push('}');
    Ok(URL_SAFE_NO_PAD.encode(Sha256::digest(canonical.as_bytes())))
}

/// The base64url SHA-256 of an access token: what a proof sent with it
/// carries as `ath` (RFC 9449 §4.2).
#[must_use]
pub fn access_token_hash(access_token: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(access_token.as_bytes()))
}

/// The proof's protected header as JSON, read before the signature is
/// checked because it names the key the signature is checked with.
fn proof_header(proof: &str) -> Result<Map<String, Value>, DpopError> {
    let mut parts = proof.split('.');
    let (Some(header), Some(_), Some(_), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(DpopError::Malformed);
    };
    let bytes = URL_SAFE_NO_PAD
        .decode(header)
        .map_err(|_| DpopError::Malformed)?;
    serde_json::from_slice(&bytes).map_err(|_| DpopError::Malformed)
}

/// A required string claim.
fn string_claim<'c>(
    claims: &'c Map<String, Value>,
    name: &'static str,
) -> Result<&'c str, DpopError> {
    claims
        .get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or(DpopError::MissingClaim(name))
}

/// `uri` without its query and fragment.
fn without_query(uri: &str) -> &str {
    uri.split(['?', '#']).next().unwrap_or(uri)
}

/// The asymmetric JWS algorithm `name` names, if the stack verifies it.
fn asymmetric_algorithm(name: &str) -> Option<Algorithm> {
    let algorithm = match name {
        "ES256" => Algorithm::ES256,
        "ES384" => Algorithm::ES384,
        "RS256" => Algorithm::RS256,
        "RS384" => Algorithm::RS384,
        "RS512" => Algorithm::RS512,
        "PS256" => Algorithm::PS256,
        "PS384" => Algorithm::PS384,
        "PS512" => Algorithm::PS512,
        "EdDSA" => Algorithm::EdDSA,
        _ => return None,
    };
    Some(algorithm)
}

/// Whether a `DPoP` header value is present and single, from the values a
/// request carries under that name. RFC 9449 §4.3 step 1 refuses a request
/// with more than one.
#[must_use]
pub fn single_proof<'v, I>(values: I) -> Option<&'v str>
where
    I: IntoIterator<Item = &'v str>,
{
    let mut values = values.into_iter();
    let first = values.next()?;
    values.next().is_none().then_some(first)
}

#[cfg(test)]
mod tests {
    use jsonwebtoken::{encode, EncodingKey, Header};
    use serde_json::json;

    use super::*;

    const TEST_KEY: &str = "-----BEGIN PRIVATE KEY-----
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgpbH6oQSVSYUgbrn3
4AOEqaR1OtL0XIwAXxhiGRvRpcOhRANCAAR8xBbzOIvvGO0sqN5xb5q6xtZc01ze
isqVehX/E2QN6niEKvzcpz1bCIJu3Lu5xmKN7H+YO77aKTQALV2uwDtD
-----END PRIVATE KEY-----";

    const HTU: &str = "https://api.example.test/mcp";

    fn public_jwk() -> Value {
        json!({
            "kty": "EC",
            "crv": "P-256",
            "x": "fMQW8ziL7xjtLKjecW-ausbWXNNc3orKlXoV_xNkDeo",
            "y": "eIQq_NynPVsIgm7cu7nGYo3sf5g7vtopNAAtXa7AO0M",
        })
    }

    fn sign(typ: &str, jwk: Value, claims: &Value) -> String {
        let mut header = Header::new(Algorithm::ES256);
        header.typ = Some(typ.to_owned());
        header.jwk = Some(serde_json::from_value(jwk).expect("jwk")); // Safe: test fixture
        let key = EncodingKey::from_ec_pem(TEST_KEY.as_bytes()).expect("key"); // Safe: test fixture
        encode(&header, claims, &key).expect("signed") // Safe: test fixture
    }

    fn now() -> Timestamp {
        Timestamp::from_epoch_seconds(1_800_000_000).expect("now") // Safe: test fixture
    }

    fn claims(extra: &Value) -> Value {
        let mut claims = json!({"jti": "j-1", "htm": "POST", "htu": HTU, "iat": 1_800_000_000});
        if let (Some(base), Some(extra)) = (claims.as_object_mut(), extra.as_object()) {
            base.extend(extra.clone());
        }
        claims
    }

    fn request(token: Option<&str>) -> DpopRequest<'_> {
        DpopRequest {
            htm: "POST",
            htu: HTU,
            access_token: token,
            nonce: None,
        }
    }

    #[test]
    fn a_valid_proof_yields_its_key_thumbprint_and_jti() {
        let proof = sign(DPOP_PROOF_TYPE, public_jwk(), &claims(&json!({})));
        let verified = DpopVerifier::default()
            .verify(&proof, &request(None), now())
            .expect("valid proof");
        assert_eq!(
            verified.jkt,
            jwk_thumbprint(&public_jwk()).expect("thumbprint")
        );
        assert_eq!(verified.jti, "j-1");
        assert_eq!(verified.iat, now());
    }

    #[test]
    fn a_proof_for_a_token_must_carry_its_hash() {
        let token = "access-token-value";
        let bound = sign(
            DPOP_PROOF_TYPE,
            public_jwk(),
            &claims(&json!({"ath": access_token_hash(token)})),
        );
        let verifier = DpopVerifier::default();
        assert!(verifier
            .verify(&bound, &request(Some(token)), now())
            .is_ok());
        assert_eq!(
            verifier.verify(&bound, &request(Some("another-token")), now()),
            Err(DpopError::AccessTokenHashMismatch)
        );
        let unbound = sign(DPOP_PROOF_TYPE, public_jwk(), &claims(&json!({})));
        assert_eq!(
            verifier.verify(&unbound, &request(Some(token)), now()),
            Err(DpopError::AccessTokenHashMismatch)
        );
    }

    #[test]
    fn method_uri_and_time_must_match_the_request() {
        let verifier = DpopVerifier::default();
        let wrong_method = sign(
            DPOP_PROOF_TYPE,
            public_jwk(),
            &claims(&json!({"htm": "GET"})),
        );
        assert_eq!(
            verifier.verify(&wrong_method, &request(None), now()),
            Err(DpopError::MethodMismatch)
        );
        let wrong_uri = sign(
            DPOP_PROOF_TYPE,
            public_jwk(),
            &claims(&json!({"htu": "https://evil.example.test/mcp"})),
        );
        assert_eq!(
            verifier.verify(&wrong_uri, &request(None), now()),
            Err(DpopError::UriMismatch)
        );
        let with_query = sign(
            DPOP_PROOF_TYPE,
            public_jwk(),
            &claims(&json!({"htu": format!("{HTU}?a=b#frag")})),
        );
        assert!(verifier.verify(&with_query, &request(None), now()).is_ok());
        let old = sign(
            DPOP_PROOF_TYPE,
            public_jwk(),
            &claims(&json!({"iat": 1_800_000_000 - DEFAULT_MAX_AGE_SECS - 1})),
        );
        assert_eq!(
            verifier.verify(&old, &request(None), now()),
            Err(DpopError::Stale)
        );
        let future = sign(
            DPOP_PROOF_TYPE,
            public_jwk(),
            &claims(&json!({"iat": 1_800_000_000 + DEFAULT_LEEWAY_SECS + 1})),
        );
        assert_eq!(
            verifier.verify(&future, &request(None), now()),
            Err(DpopError::Stale)
        );
    }

    #[test]
    fn header_type_key_and_signature_are_checked() {
        let verifier = DpopVerifier::default();
        let wrong_type = sign("JWT", public_jwk(), &claims(&json!({})));
        assert_eq!(
            verifier.verify(&wrong_type, &request(None), now()),
            Err(DpopError::WrongType)
        );
        // A key that is not the signer's: the signature cannot verify.
        let mut other = public_jwk();
        other["x"] = json!("WKn-ZIGevcwGIyyrzFoZNBdaq9_TsqzGl96oc0CWuis");
        other["y"] = json!("y77t-RvAHRKTsSGdIYUfweuOvwrvDD-Q3Hv5J0fSKbE");
        let forged = sign(DPOP_PROOF_TYPE, other, &claims(&json!({})));
        assert!(verifier.verify(&forged, &request(None), now()).is_err());
        let mut private = public_jwk();
        private["d"] = json!("secret");
        assert_eq!(jwk_thumbprint(&private), Err(DpopError::InvalidKey));
        assert_eq!(
            verifier.verify("a.b", &request(None), now()),
            Err(DpopError::Malformed)
        );
        let only_rsa = DpopVerifier::new(&["RS256", "HS256"]);
        assert_eq!(only_rsa.algorithm_names(), vec!["RS256".to_owned()]);
        let proof = sign(DPOP_PROOF_TYPE, public_jwk(), &claims(&json!({})));
        assert_eq!(
            only_rsa.verify(&proof, &request(None), now()),
            Err(DpopError::UnsupportedAlgorithm)
        );
    }

    #[test]
    fn a_missing_claim_or_nonce_is_named() {
        let verifier = DpopVerifier::default();
        let no_jti = sign(
            DPOP_PROOF_TYPE,
            public_jwk(),
            &json!({"htm": "POST", "htu": HTU, "iat": 1_800_000_000}),
        );
        assert_eq!(
            verifier.verify(&no_jti, &request(None), now()),
            Err(DpopError::MissingClaim("jti"))
        );
        let proof = sign(
            DPOP_PROOF_TYPE,
            public_jwk(),
            &claims(&json!({"nonce": "n-1"})),
        );
        let mut with_nonce = request(None);
        with_nonce.nonce = Some("n-2");
        let refused = verifier.verify(&proof, &with_nonce, now());
        assert_eq!(refused, Err(DpopError::NonceRequired));
        assert_eq!(DpopError::NonceRequired.error_code(), "use_dpop_nonce");
        with_nonce.nonce = Some("n-1");
        assert!(verifier.verify(&proof, &with_nonce, now()).is_ok());
    }

    #[test]
    fn thumbprint_matches_rfc_7638_example() {
        // RFC 7638 §3.1.
        let jwk = json!({
            "kty": "RSA",
            "n": "0vx7agoebGcQSuuPiLJXZptN9nndrQmbXEps2aiAFbWhM78LhWx4cbbfAAtVT86zwu1RK7aPFFxuhDR1L6tSoc_BJECPebWKRXjBZCiFV4n3oknjhMstn64tZ_2W-5JsGY4Hc5n9yBXArwl93lqt7_RN5w6Cf0h4QyQ5v-65YGjQR0_FDW2QvzqY368QQMicAtaSqzs8KJZgnYb9c7d0zgdAZHzu6qMQvRL5hajrn1n91CbOpbISD08qNLyrdkt-bFTWhAI4vMQFh6WeZu0fM4lFd2NcRwr3XPksINHaQ-G_xBniIqbw0Ls1jF44-csFCur-kEgU8awapJzKnqDKgw",
            "e": "AQAB",
            "alg": "RS256",
            "kid": "2011-04-29",
        });
        assert_eq!(
            jwk_thumbprint(&jwk).expect("thumbprint"),
            "NzbLsXh8uDCcd-6MNwXF4W_7noWXFZAfHkxZsRGC9Xs"
        );
    }

    #[test]
    fn only_a_single_proof_header_is_taken() {
        assert_eq!(single_proof(["a"]), Some("a"));
        assert_eq!(single_proof(["a", "b"]), None);
        assert_eq!(single_proof(Vec::<&str>::new()), None);
    }
}
