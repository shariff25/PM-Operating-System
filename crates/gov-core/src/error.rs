//! One closed list of refusals. Each has a stable code for receipts and answers.

/// Why a document, a chain or a request was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("the envelope is malformed")]
    BadEnvelope,
    #[error("unknown format version")]
    UnknownVersion,
    #[error("no key with that id is trusted for this document")]
    UnknownKey,
    #[error("the signature does not verify")]
    BadSignature,
    #[error("the payload is malformed: {0}")]
    Malformed(&'static str),
    #[error("the document is for another tenant")]
    WrongTenant,
    #[error("the document is for another environment")]
    WrongEnv,
    #[error("the document has expired")]
    Expired,
    #[error("the document is not valid yet")]
    NotYetValid,
    #[error("the document, its agent or its key is revoked")]
    Revoked,
    #[error("the mandate chain is too deep")]
    ChainTooDeep,
    #[error("a mandate is wider than its parent: {0}")]
    WiderThanParent(&'static str),
    #[error("a mandate was not delegated by its parent's agent")]
    NotDelegatedByParent,
    #[error("the mandate requires a sandbox, which nothing can confirm yet")]
    SandboxUnverifiable,
    #[error("the request is for another gate")]
    WrongGate,
    #[error("the request is for another session")]
    WrongSession,
    #[error("the request names another mandate")]
    WrongMandate,
    #[error("the digest does not match the action")]
    WrongDigest,
    #[error("the request counter did not rise")]
    Replay,
    #[error("the countersignature is of a weaker class than policy asks for")]
    WrongClass,
    #[error("the approver is not in the group policy asks for")]
    WrongGroup,
    #[error("the approver's passkey signature was not verified")]
    PasskeyUnverified,
    #[error("the receipt does not follow the chain's head")]
    BrokenChain,
    #[error("the lease is too old for this action")]
    LeaseStale,
    #[error("the gate does not hold the policy or the revocations the lease names")]
    LeaseIncomplete,
    #[error("a number cannot be written in canonical form")]
    Number,
    #[error("the lease is not the answer to this gate's request")]
    LeaseReplayed,
    #[error("the lease's time or revocation number is below the one held")]
    AuthorityBehind,
    #[error("the public key is one anyone can forge for")]
    WeakKey,
    #[error("the approver is the person who issued the mandate")]
    ApproverIsIssuer,
    #[error("the mandate's template is not in the policy bundle held")]
    UnknownTemplate,
    #[error("a mandate is wider than its template: {0}")]
    WiderThanTemplate(&'static str),
    #[error("the policy bundle is older than the one held")]
    BundleOlder,
    #[error("another policy bundle is held under the same number")]
    BundleForked,
}

impl Error {
    /// A stable, lower-case code. Never free text.
    pub fn code(&self) -> &'static str {
        match self {
            Error::BadEnvelope => "bad_envelope",
            Error::UnknownVersion => "unknown_version",
            Error::UnknownKey => "unknown_key",
            Error::BadSignature => "bad_signature",
            Error::Malformed(_) => "malformed",
            Error::WrongTenant => "wrong_tenant",
            Error::WrongEnv => "wrong_env",
            Error::Expired => "expired",
            Error::NotYetValid => "not_yet_valid",
            Error::Revoked => "revoked",
            Error::ChainTooDeep => "chain_too_deep",
            Error::WiderThanParent(_) => "wider_than_parent",
            Error::NotDelegatedByParent => "not_delegated_by_parent",
            Error::SandboxUnverifiable => "sandbox_unverifiable",
            Error::WrongGate => "wrong_gate",
            Error::WrongSession => "wrong_session",
            Error::WrongMandate => "wrong_mandate",
            Error::WrongDigest => "wrong_digest",
            Error::Replay => "replay",
            Error::WrongClass => "wrong_class",
            Error::WrongGroup => "wrong_group",
            Error::PasskeyUnverified => "passkey_unverified",
            Error::BrokenChain => "broken_chain",
            Error::LeaseStale => "lease_stale",
            Error::LeaseIncomplete => "lease_incomplete",
            Error::Number => "number",
            Error::LeaseReplayed => "lease_replayed",
            Error::AuthorityBehind => "authority_behind",
            Error::WeakKey => "weak_key",
            Error::ApproverIsIssuer => "approver_is_issuer",
            Error::UnknownTemplate => "unknown_template",
            Error::WiderThanTemplate(_) => "wider_than_template",
            Error::BundleOlder => "bundle_older",
            Error::BundleForked => "bundle_forked",
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;
