//! The rule set, as data (spec 013 B-4).
//!
//! Every detector in this crate is a row in one of the tables below plus a
//! fixture in `testdata/corpus/`. Adding a credential shape is those two
//! things and nothing else: there is no new `match` arm, no new call in
//! [`crate::secrets`], and no new configuration key. That is the whole point
//! of the rule set being data. A detector that needed a code path would be a
//! detector nobody could review against the corpus.
//!
//! The one detector with no table is the entropy detector, which is a
//! threshold rather than a pattern ([`EntropyRule`]).

use std::collections::BTreeSet;

use aicortex_types::SourceSystem;

use crate::limits::Limits;

/// The stable name of a detector, as a refusal reports it and a fixture
/// records it.
///
/// A `&'static str` newtype rather than an enum: a detector is a table row,
/// and an enum would make adding one a code change in two places.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DetectorId(&'static str);

impl DetectorId {
    /// Name a detector.
    #[must_use]
    pub const fn new(name: &'static str) -> Self {
        Self(name)
    }

    /// The name, as it is reported and recorded.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

impl core::fmt::Display for DetectorId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.0)
    }
}

impl serde::Serialize for DetectorId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.0)
    }
}

/// Which characters a credential's body is made of, so that a prefix match is
/// not a match on prose that happens to start with the same letters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Charset {
    /// `A-Z a-z 0-9`.
    Alphanumeric,
    /// `A-Z a-z 0-9 - _`, the base64url alphabet without padding.
    Base64Url,
    /// `A-Z a-z 0-9 + / = - _ .`, which is every token alphabet in use.
    Token,
}

impl Charset {
    /// Whether `character` belongs to this alphabet.
    #[must_use]
    pub const fn admits(self, character: char) -> bool {
        match self {
            Self::Alphanumeric => character.is_ascii_alphanumeric(),
            Self::Base64Url => {
                character.is_ascii_alphanumeric() || character == '-' || character == '_'
            }
            Self::Token => {
                character.is_ascii_alphanumeric()
                    || matches!(character, '+' | '/' | '=' | '-' | '_' | '.')
            }
        }
    }
}

/// A credential recognised by the way it starts (B-4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PrefixRule {
    /// What a refusal names.
    pub detector: DetectorId,
    /// The literal the token begins with.
    pub prefix: &'static str,
    /// How many characters of [`Self::charset`] must follow it. A prefix with
    /// no length requirement matches the word "sky" and every sentence that
    /// contains it.
    pub min_tail: usize,
    /// What those characters may be.
    pub charset: Charset,
}

/// A private key recognised by its armour (B-4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PemRule {
    /// What a refusal names.
    pub detector: DetectorId,
    /// The armour line, matched as a substring anywhere in the text. A PEM
    /// header is unambiguous: no prose contains one by accident.
    pub marker: &'static str,
}

/// The token shapes published by the systems whose credentials people paste.
///
/// Each row is a prefix, a minimum body length, and the alphabet that body is
/// written in. The lengths are the published ones where a system publishes
/// them and a conservative floor where it does not; a floor that is too low
/// costs a false positive an operator can override (B-5), and one that is too
/// high costs a leaked credential, so they lean low.
pub const PREFIX_RULES: &[PrefixRule] = &[
    PrefixRule {
        detector: DetectorId::new("anthropic-api-key"),
        prefix: "sk-ant-",
        min_tail: 24,
        charset: Charset::Base64Url,
    },
    PrefixRule {
        detector: DetectorId::new("openai-api-key"),
        prefix: "sk-",
        min_tail: 20,
        charset: Charset::Base64Url,
    },
    PrefixRule {
        detector: DetectorId::new("stripe-secret-key"),
        prefix: "sk_live_",
        min_tail: 16,
        charset: Charset::Alphanumeric,
    },
    PrefixRule {
        detector: DetectorId::new("stripe-restricted-key"),
        prefix: "rk_live_",
        min_tail: 16,
        charset: Charset::Alphanumeric,
    },
    PrefixRule {
        detector: DetectorId::new("github-token"),
        prefix: "ghp_",
        min_tail: 36,
        charset: Charset::Alphanumeric,
    },
    PrefixRule {
        detector: DetectorId::new("github-oauth-token"),
        prefix: "gho_",
        min_tail: 36,
        charset: Charset::Alphanumeric,
    },
    PrefixRule {
        detector: DetectorId::new("github-user-token"),
        prefix: "ghu_",
        min_tail: 36,
        charset: Charset::Alphanumeric,
    },
    PrefixRule {
        detector: DetectorId::new("github-server-token"),
        prefix: "ghs_",
        min_tail: 36,
        charset: Charset::Alphanumeric,
    },
    PrefixRule {
        detector: DetectorId::new("github-refresh-token"),
        prefix: "ghr_",
        min_tail: 36,
        charset: Charset::Alphanumeric,
    },
    PrefixRule {
        detector: DetectorId::new("github-fine-grained-token"),
        prefix: "github_pat_",
        min_tail: 40,
        charset: Charset::Base64Url,
    },
    PrefixRule {
        detector: DetectorId::new("gitlab-personal-token"),
        prefix: "glpat-",
        min_tail: 20,
        charset: Charset::Base64Url,
    },
    PrefixRule {
        detector: DetectorId::new("slack-bot-token"),
        prefix: "xoxb-",
        min_tail: 24,
        charset: Charset::Token,
    },
    PrefixRule {
        detector: DetectorId::new("slack-user-token"),
        prefix: "xoxp-",
        min_tail: 24,
        charset: Charset::Token,
    },
    PrefixRule {
        detector: DetectorId::new("slack-app-token"),
        prefix: "xapp-",
        min_tail: 24,
        charset: Charset::Token,
    },
    PrefixRule {
        detector: DetectorId::new("aws-access-key-id"),
        prefix: "AKIA",
        min_tail: 16,
        charset: Charset::Alphanumeric,
    },
    PrefixRule {
        detector: DetectorId::new("aws-temporary-access-key-id"),
        prefix: "ASIA",
        min_tail: 16,
        charset: Charset::Alphanumeric,
    },
    PrefixRule {
        detector: DetectorId::new("google-api-key"),
        prefix: "AIza",
        min_tail: 35,
        charset: Charset::Base64Url,
    },
    PrefixRule {
        detector: DetectorId::new("google-oauth-token"),
        prefix: "ya29.",
        min_tail: 32,
        charset: Charset::Token,
    },
    PrefixRule {
        detector: DetectorId::new("npm-token"),
        prefix: "npm_",
        min_tail: 36,
        charset: Charset::Alphanumeric,
    },
    PrefixRule {
        detector: DetectorId::new("digitalocean-token"),
        prefix: "dop_v1_",
        min_tail: 60,
        charset: Charset::Alphanumeric,
    },
    PrefixRule {
        detector: DetectorId::new("sendgrid-api-key"),
        prefix: "SG.",
        min_tail: 32,
        charset: Charset::Token,
    },
    PrefixRule {
        detector: DetectorId::new("huggingface-token"),
        prefix: "hf_",
        min_tail: 32,
        charset: Charset::Alphanumeric,
    },
    PrefixRule {
        detector: DetectorId::new("shopify-access-token"),
        prefix: "shpat_",
        min_tail: 32,
        charset: Charset::Alphanumeric,
    },
    PrefixRule {
        detector: DetectorId::new("supabase-service-key"),
        prefix: "sbp_",
        min_tail: 36,
        charset: Charset::Alphanumeric,
    },
];

/// The armour lines of the private-key formats.
pub const PEM_RULES: &[PemRule] = &[
    PemRule {
        detector: DetectorId::new("pem-private-key"),
        marker: "-----BEGIN PRIVATE KEY-----",
    },
    PemRule {
        detector: DetectorId::new("pem-rsa-private-key"),
        marker: "-----BEGIN RSA PRIVATE KEY-----",
    },
    PemRule {
        detector: DetectorId::new("pem-ec-private-key"),
        marker: "-----BEGIN EC PRIVATE KEY-----",
    },
    PemRule {
        detector: DetectorId::new("pem-dsa-private-key"),
        marker: "-----BEGIN DSA PRIVATE KEY-----",
    },
    PemRule {
        detector: DetectorId::new("pem-encrypted-private-key"),
        marker: "-----BEGIN ENCRYPTED PRIVATE KEY-----",
    },
    PemRule {
        detector: DetectorId::new("openssh-private-key"),
        marker: "-----BEGIN OPENSSH PRIVATE KEY-----",
    },
    PemRule {
        detector: DetectorId::new("pgp-private-key"),
        marker: "-----BEGIN PGP PRIVATE KEY BLOCK-----",
    },
];

/// The detector that reports a credential embedded in a URL's userinfo.
pub const URL_CREDENTIAL: DetectorId = DetectorId::new("url-embedded-credential");

/// The detector that reports a JSON Web Token.
pub const JWT: DetectorId = DetectorId::new("json-web-token");

/// The detector that reports an unbroken token of high entropy.
pub const HIGH_ENTROPY: DetectorId = DetectorId::new("high-entropy-token");

/// How many fractional bits the entropy arithmetic carries.
///
/// The entropy detector is integer arithmetic in Q16 fixed point rather than
/// floating point, for two reasons. The workspace denies
/// `clippy::float_arithmetic` outside two named crates (spec 010 B-1), and
/// this is not one of them. And B-10 requires the same candidate to yield the
/// same verdict always, which an integer comparison gives on every target
/// without an argument about rounding (D-4).
pub const ENTROPY_FRACTION_BITS: u32 = 16;

/// One whole bit, in the fixed point [`ENTROPY_FRACTION_BITS`] defines.
pub const ENTROPY_ONE: u64 = 1 << ENTROPY_FRACTION_BITS;

/// `whole.hundredths` bits per character, in Q16.
#[must_use]
pub const fn bits(whole: u64, hundredths: u64) -> u64 {
    whole * ENTROPY_ONE + (hundredths * ENTROPY_ONE) / 100
}

/// The threshold detector of B-4: a long unbroken token whose Shannon entropy
/// is above a configurable bound.
///
/// The three guards below are all false-positive control, and FR-005 is what
/// holds them honest: the benign corpus is a committed file, and a change to
/// any of these numbers that starts refusing an identifier in it fails the
/// build.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EntropyRule {
    /// The shortest token considered at all. Below this, entropy per
    /// character is noise: a six-character token cannot have more than
    /// log2(6) bits of measured entropy however random it is.
    pub min_len: usize,
    /// Bits per character, in Q16, at or above which a token is a credential.
    pub threshold: u64,
    /// Whether a token must mix upper case, lower case and digits to be
    /// considered. A hex digest, a ULID and a UUID each fail this and so are
    /// never measured, which is most of FR-005 in one condition.
    pub require_mixed_case_and_digit: bool,
}

impl EntropyRule {
    /// The shipped threshold.
    ///
    /// 4.50 bits per character over at least 24 characters. A 40-character
    /// hex digest measures at most 4.00 by construction, a dash-separated
    /// UUID less; a random 32-byte secret in base64 measures about 5.50.
    /// The gap between those is where this sits.
    pub const STANDARD: Self = Self {
        min_len: 24,
        threshold: bits(4, 50),
        require_mixed_case_and_digit: true,
    };
}

impl Default for EntropyRule {
    fn default() -> Self {
        Self::STANDARD
    }
}

/// Which detectors run, and how the threshold one is tuned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SecretRules {
    /// The prefix table. Defaults to [`PREFIX_RULES`].
    pub prefixes: &'static [PrefixRule],
    /// The armour table. Defaults to [`PEM_RULES`].
    pub pem: &'static [PemRule],
    /// Whether a credential in a URL's userinfo is a refusal.
    pub url_credentials: bool,
    /// Whether a JSON Web Token is a refusal.
    pub json_web_tokens: bool,
    /// The threshold detector, or `None` to run only the patterns.
    pub entropy: Option<EntropyRule>,
}

impl Default for SecretRules {
    fn default() -> Self {
        Self {
            prefixes: PREFIX_RULES,
            pem: PEM_RULES,
            url_credentials: true,
            json_web_tokens: true,
            entropy: Some(EntropyRule::STANDARD),
        }
    }
}

/// Everything the gate consults, in one value.
///
/// There is no flag here that turns the gate off, and there is no constructor
/// that produces an empty rule set (B-5). What a deployment may do is narrow
/// the limits ([`Limits::narrowed_to`]) and add denied sources; the detectors
/// are the crate's.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct RuleSet {
    /// The size and media ceilings (B-6).
    pub limits: Limits,
    /// The detectors (B-4).
    pub secrets: SecretRules,
    /// Source systems this deployment refuses outright, whatever they carry.
    ///
    /// The one place `PolicyDenied` comes from. Empty by default: a
    /// deployment that has no policy has no denials, rather than a default
    /// list somebody has to discover.
    pub denied_sources: BTreeSet<SourceSystem>,
}

impl RuleSet {
    /// The shipped rules: the full detector tables and the default ceilings.
    #[must_use]
    pub fn standard() -> Self {
        Self::default()
    }

    /// The same rules, refusing `system` outright.
    #[must_use]
    pub fn denying(mut self, system: SourceSystem) -> Self {
        self.denied_sources.insert(system);
        self
    }

    /// The same rules with narrower limits (B-6).
    ///
    /// # Errors
    ///
    /// [`crate::GateError`] when `limits` raises a ceiling rather than
    /// lowering it.
    pub fn narrowed_to(mut self, limits: Limits) -> Result<Self, crate::GateError> {
        self.limits = self.limits.narrowed_to(limits)?;
        Ok(self)
    }
}
