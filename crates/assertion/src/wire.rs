//! The wire format: one line of `name=value` fields joined by `;`, in a fixed order, with the
//! signature last. Exactly one encoding is valid for any assertion, so the parser can be
//! strict: it re-derives each value's canonical spelling and refuses anything else.
//!
//! ```text
//! v=1;kid=<key id>;iss=<issuer>;aud=<audience>;sub=<handle>;roles=<r1,r2|empty>;
//! sid=<session>;iat=<unix s>;exp=<unix s>;nonce=<b64url 16 bytes>;req=<b64url 32 bytes|->;
//! sig=<b64url HMAC-SHA256>
//! ```
//! (shown wrapped; on the wire it is a single line with no spaces).

use base64::alphabet::URL_SAFE;
use base64::engine::general_purpose::{GeneralPurpose, GeneralPurposeConfig, URL_SAFE_NO_PAD};
use base64::engine::DecodePaddingMode;
use base64::Engine as _;
use pmos_contract::model::Role;
use thiserror::Error;

use crate::fields::{role_rank, valid_name, valid_session, valid_subject};

/// The only version this build reads and writes.
pub const VERSION: u32 = 1;
/// The longest assertion accepted, in bytes. A real one is well under 600.
pub const MAX_LEN: usize = 1024;
/// The longest lifetime (`exp - iat`), enforced when signing and when verifying.
pub const MAX_LIFETIME_SECS: u64 = 120;
/// Bytes of randomness in a nonce.
pub const NONCE_LEN: usize = 16;

pub(crate) const FIELDS: [&str; 12] = [
    "v", "kid", "iss", "aud", "sub", "roles", "sid", "iat", "exp", "nonce", "req", "sig",
];

/// Why an assertion could not be read. Names a field at most, never echoes input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum Malformed {
    #[error("empty")]
    Empty,
    #[error("longer than {MAX_LEN} bytes")]
    TooLong,
    #[error("a character outside visible ASCII")]
    BadCharacter,
    #[error("does not start with a version field")]
    NoVersion,
    #[error("a version this build does not know")]
    UnknownVersion,
    #[error("a field without `=`")]
    NoSeparator,
    #[error("an unknown field")]
    UnknownField,
    #[error("the field `{0}` appears twice")]
    DuplicateField(&'static str),
    #[error("the field `{0}` is out of order")]
    FieldOrder(&'static str),
    #[error("the field `{0}` is missing")]
    MissingField(&'static str),
    #[error("the field `{0}` has an invalid value")]
    BadValue(&'static str),
    #[error("the field `{0}` is not in canonical form")]
    NonCanonical(&'static str),
    #[error("the lifetime is not between 1 and {MAX_LIFETIME_SECS} seconds")]
    Lifetime,
}

/// A random value that makes each assertion unique; the replay cache's key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Nonce(pub(crate) [u8; NONCE_LEN]);

impl Nonce {
    pub fn as_bytes(&self) -> &[u8; NONCE_LEN] {
        &self.0
    }
}

/// A verified assertion. Only [`crate::KeyRing::verify`] hands these out, so holding one
/// means every check passed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assertion {
    pub(crate) key_id: String,
    pub(crate) issuer: String,
    pub(crate) audience: String,
    pub(crate) subject: String,
    pub(crate) roles: Vec<Role>,
    pub(crate) session: String,
    pub(crate) issued_at: u64,
    pub(crate) expires_at: u64,
    pub(crate) nonce: Nonce,
    pub(crate) request: Option<[u8; 32]>,
}

impl Assertion {
    pub fn version(&self) -> u32 {
        VERSION
    }
    pub fn key_id(&self) -> &str {
        &self.key_id
    }
    /// The calling service's client name.
    pub fn issuer(&self) -> &str {
        &self.issuer
    }
    pub fn audience(&self) -> &str {
        &self.audience
    }
    /// The PM OS handle the caller acts for.
    pub fn subject(&self) -> &str {
        &self.subject
    }
    /// Known roles in canonical order.
    pub fn roles(&self) -> &[Role] {
        &self.roles
    }
    pub fn has_role(&self, role: &Role) -> bool {
        self.roles.contains(role)
    }
    pub fn session(&self) -> &str {
        &self.session
    }
    /// Unix seconds.
    pub fn issued_at(&self) -> u64 {
        self.issued_at
    }
    /// Unix seconds.
    pub fn expires_at(&self) -> u64 {
        self.expires_at
    }
    pub fn nonce(&self) -> &Nonce {
        &self.nonce
    }
    /// Whether it was bound to one request.
    pub fn is_request_bound(&self) -> bool {
        self.request.is_some()
    }

    /// The signed part: every field but `sig`, in order.
    pub(crate) fn signed_part(&self) -> String {
        let roles = self
            .roles
            .iter()
            .map(Role::as_str)
            .collect::<Vec<_>>()
            .join(",");
        let req = match &self.request {
            Some(d) => URL_SAFE_NO_PAD.encode(d),
            None => "-".to_string(),
        };
        format!(
            "v={VERSION};kid={};iss={};aud={};sub={};roles={roles};sid={};iat={};exp={};nonce={};req={req}",
            self.key_id,
            self.issuer,
            self.audience,
            self.subject,
            self.session,
            self.issued_at,
            self.expires_at,
            URL_SAFE_NO_PAD.encode(self.nonce.0),
        )
    }
}

/// An assertion read from the wire, not yet verified.
pub(crate) struct Parsed<'a> {
    pub assertion: Assertion,
    /// The exact bytes the signature covers.
    pub signed: &'a str,
    pub signature: [u8; 32],
}

/// A lenient decoder (stray trailing bits allowed) so that [`b64`] can tell a non-canonical
/// spelling of valid bytes from garbage, and refuse it by re-encoding.
const LENIENT: GeneralPurpose = GeneralPurpose::new(
    &URL_SAFE,
    GeneralPurposeConfig::new()
        .with_encode_padding(false)
        .with_decode_allow_trailing_bits(true)
        .with_decode_padding_mode(DecodePaddingMode::RequireNone),
);

/// Decode canonical unpadded base64url of exactly `N` bytes.
fn b64<const N: usize>(field: &'static str, s: &str) -> Result<[u8; N], Malformed> {
    // Unpadded base64url of N bytes is exactly this long; reject before decoding.
    if s.len() != (N * 4).div_ceil(3) {
        return Err(Malformed::BadValue(field));
    }
    let bytes = LENIENT.decode(s).map_err(|_| Malformed::BadValue(field))?;
    let out: [u8; N] = bytes.try_into().map_err(|_| Malformed::BadValue(field))?;
    // Exactly one spelling is valid: the one the signer writes.
    if URL_SAFE_NO_PAD.encode(out) != s {
        return Err(Malformed::NonCanonical(field));
    }
    Ok(out)
}

/// Decimal unix seconds with no sign and no leading zero.
fn seconds(field: &'static str, s: &str) -> Result<u64, Malformed> {
    if s.is_empty() || s.len() > 20 || !s.bytes().all(|c| c.is_ascii_digit()) {
        return Err(Malformed::BadValue(field));
    }
    if s.len() > 1 && s.starts_with('0') {
        return Err(Malformed::NonCanonical(field));
    }
    s.parse().map_err(|_| Malformed::BadValue(field))
}

fn roles(s: &str) -> Result<Vec<Role>, Malformed> {
    if s.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    let mut last: Option<usize> = None;
    for name in s.split(',') {
        let role = Role::parse(name).ok_or(Malformed::BadValue("roles"))?;
        let rank = role_rank(&role).ok_or(Malformed::BadValue("roles"))?;
        if last.is_some_and(|l| rank <= l) {
            // A duplicate or an out-of-order role.
            return Err(Malformed::NonCanonical("roles"));
        }
        last = Some(rank);
        out.push(role);
    }
    Ok(out)
}

/// Strictly parse an assertion. The signature is not checked here.
pub(crate) fn parse(token: &str) -> Result<Parsed<'_>, Malformed> {
    if token.is_empty() {
        return Err(Malformed::Empty);
    }
    if token.len() > MAX_LEN {
        return Err(Malformed::TooLong);
    }
    if !token.bytes().all(|c| (0x21..=0x7e).contains(&c)) {
        return Err(Malformed::BadCharacter);
    }
    // The version comes first and is judged on its own, so a newer format with other fields
    // reads as an unknown version rather than as unknown fields.
    let Some(rest) = token.strip_prefix("v=") else {
        return Err(Malformed::NoVersion);
    };
    let version = rest.split(';').next().unwrap_or_default();
    if version != "1" {
        return Err(Malformed::UnknownVersion);
    }

    let mut values: [Option<&str>; FIELDS.len()] = [None; FIELDS.len()];
    let mut last = 0usize;
    for (n, part) in token.split(';').enumerate() {
        let (name, value) = part.split_once('=').ok_or(Malformed::NoSeparator)?;
        let i = FIELDS
            .iter()
            .position(|f| *f == name)
            .ok_or(Malformed::UnknownField)?;
        if values[i].is_some() {
            return Err(Malformed::DuplicateField(FIELDS[i]));
        }
        if n > 0 && i <= last {
            return Err(Malformed::FieldOrder(FIELDS[i]));
        }
        last = i;
        values[i] = Some(value);
    }
    let mut get = {
        let mut i = 0;
        move || {
            let field = FIELDS[i];
            let v = values[i].ok_or(Malformed::MissingField(field));
            i += 1;
            v.map(|v| (field, v))
        }
    };

    let _version = get()?;
    let (f, key_id) = get()?;
    if !valid_name(key_id) {
        return Err(Malformed::BadValue(f));
    }
    let (f, issuer) = get()?;
    if !valid_name(issuer) {
        return Err(Malformed::BadValue(f));
    }
    let (f, audience) = get()?;
    if !valid_name(audience) {
        return Err(Malformed::BadValue(f));
    }
    let (f, subject) = get()?;
    if !valid_subject(subject) {
        return Err(Malformed::BadValue(f));
    }
    let (_, role_list) = get()?;
    let roles = roles(role_list)?;
    let (f, session) = get()?;
    if !valid_session(session) {
        return Err(Malformed::BadValue(f));
    }
    let (f, iat) = get()?;
    let issued_at = seconds(f, iat)?;
    let (f, exp) = get()?;
    let expires_at = seconds(f, exp)?;
    if expires_at <= issued_at || expires_at - issued_at > MAX_LIFETIME_SECS {
        return Err(Malformed::Lifetime);
    }
    let (f, nonce) = get()?;
    let nonce = Nonce(b64::<NONCE_LEN>(f, nonce)?);
    let (f, req) = get()?;
    let request = if req == "-" {
        None
    } else {
        Some(b64::<32>(f, req)?)
    };
    let (f, sig) = get()?;
    let signature = b64::<32>(f, sig)?;

    // `sig` is the last field and its value is canonical, so the signed part is everything
    // before `;sig=`.
    let signed = &token[..token.len() - sig.len() - ";sig=".len()];
    let assertion = Assertion {
        key_id: key_id.to_string(),
        issuer: issuer.to_string(),
        audience: audience.to_string(),
        subject: subject.to_string(),
        roles,
        session: session.to_string(),
        issued_at,
        expires_at,
        nonce,
        request,
    };
    // Belt and braces: the fields re-encode to exactly the bytes that were signed.
    if assertion.signed_part() != signed {
        return Err(Malformed::NonCanonical("v"));
    }
    Ok(Parsed {
        assertion,
        signed,
        signature,
    })
}
