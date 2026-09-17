//! The detectors (spec 013 B-4).
//!
//! One pass over the normalized text produces at most one finding, and a
//! finding is a detector name and a byte offset. Neither the value nor a
//! masked form of it is carried anywhere: B-3 refuses rather than redacts,
//! and a redaction that reached this far would already have put the secret in
//! the error, in the log and in whatever the caller does with them. FR-002 is
//! the assertion that keeps it that way.
//!
//! The scan is deterministic and left to right, so the offset a refusal names
//! is the first offending position and two runs never disagree about which
//! detector fired (B-10).
//!
//! # What is scanned
//!
//! The armour markers are matched against the whole text, because a PEM block
//! contains newlines. Everything else is matched against *tokens*: maximal
//! runs of characters that are not whitespace and not one of the delimiters
//! prose puts around a pasted value (quotes, brackets, commas, backticks).
//! That is what B-4 means by an unbroken token, and it is why a sentence
//! ending in a credential is caught while a sentence that merely contains the
//! letters is not.

use crate::rules::{Charset, DetectorId, EntropyRule, PrefixRule, SecretRules};

/// A detector's hit: which one, and where.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Finding {
    /// The detector that fired.
    pub detector: DetectorId,
    /// The byte offset into the normalized text at which the value starts.
    pub offset: usize,
}

/// The characters that end a token even though they are not whitespace.
///
/// A pasted credential is usually surrounded by some of these, and treating
/// them as part of the token would break every prefix match.
const DELIMITERS: [char; 16] = [
    '"', '\'', '`', ',', ';', '(', ')', '[', ']', '{', '}', '<', '>', '|', '\\', '\u{feff}',
];

/// Whether `character` separates one token from the next.
fn is_boundary(character: char) -> bool {
    character.is_whitespace() || DELIMITERS.contains(&character)
}

/// The tokens of `text`, each with its byte offset.
///
/// An iterator rather than a `Vec`: the scan stops at the first finding, and
/// a body at the ceiling is 64 KiB of tokens nobody needs to materialize.
fn tokens(text: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut offset = 0usize;
    core::iter::from_fn(move || {
        let rest = text.get(offset..)?;
        let start = offset + rest.find(|c: char| !is_boundary(c))?;
        let tail = text.get(start..)?;
        let len = tail.find(is_boundary).unwrap_or(tail.len());
        offset = start + len;
        tail.get(..len).map(|token| (start, token))
    })
}

/// The first detector to fire on `text`, or `None` when nothing does.
#[must_use]
pub fn scan(text: &str, rules: &SecretRules) -> Option<Finding> {
    let armour = rules
        .pem
        .iter()
        .filter_map(|rule| {
            text.find(rule.marker).map(|offset| Finding {
                detector: rule.detector,
                offset,
            })
        })
        .min_by_key(|finding| finding.offset);

    let token_finding = tokens(text).find_map(|(offset, token)| {
        prefix_match(token, rules.prefixes)
            .or_else(|| {
                rules
                    .url_credentials
                    .then(|| url_credential(token))
                    .flatten()
            })
            .or_else(|| {
                rules
                    .json_web_tokens
                    .then(|| json_web_token(token))
                    .flatten()
            })
            .map(|detector| (detector, 0usize))
            .or_else(|| rules.entropy.and_then(|rule| high_entropy(token, &rule)))
            .map(|(detector, within)| Finding {
                detector,
                offset: offset.saturating_add(within),
            })
    });

    match (armour, token_finding) {
        (Some(pem), Some(token)) if token.offset < pem.offset => Some(token),
        (Some(pem), _) => Some(pem),
        (None, token) => token,
    }
}

/// The prefix rule `token` satisfies, if any.
fn prefix_match(token: &str, rules: &[PrefixRule]) -> Option<DetectorId> {
    rules.iter().find_map(|rule| {
        let tail = token.strip_prefix(rule.prefix)?;
        let body: usize = tail.chars().take_while(|c| rule.charset.admits(*c)).count();
        (body >= rule.min_tail).then_some(rule.detector)
    })
}

/// Whether `token` is a URL whose userinfo carries a password.
///
/// `scheme://user:secret@host` only. A userinfo with no colon is a user name,
/// which is not a credential, and an empty password is a placeholder someone
/// wrote in documentation.
fn url_credential(token: &str) -> Option<DetectorId> {
    let after_scheme = token.split_once("://")?.1;
    let authority = after_scheme.split('/').next()?;
    let (userinfo, host) = authority.rsplit_once('@')?;
    let (_, password) = userinfo.split_once(':')?;
    let credible = !password.is_empty() && !host.is_empty() && !password.contains('@');
    credible.then_some(crate::rules::URL_CREDENTIAL)
}

/// Whether `token` is a JSON Web Token: three base64url segments whose first
/// decodes to a JSON object naming an algorithm or a type.
///
/// The header decode is what separates a JWT from a dotted identifier. A
/// version string, a hostname and a file name all have three dot-separated
/// parts; none of them base64url-decodes to `{"alg":...}`.
fn json_web_token(token: &str) -> Option<DetectorId> {
    let mut parts = token.split('.');
    let header = parts.next()?;
    let payload = parts.next()?;
    let signature = parts.next()?;
    if parts.next().is_some() || header.is_empty() || payload.is_empty() {
        return None;
    }
    let base64url =
        |part: &str| !part.is_empty() && part.chars().all(|c| Charset::Base64Url.admits(c));
    if !base64url(header) || !base64url(payload) || (!signature.is_empty() && !base64url(signature))
    {
        return None;
    }
    let decoded = base64url_decode(header)?;
    let text = String::from_utf8(decoded).ok()?;
    let object: serde_json::Value = serde_json::from_str(&text).ok()?;
    let names_a_header = object
        .as_object()
        .is_some_and(|map| map.contains_key("alg") || map.contains_key("typ"));
    names_a_header.then_some(crate::rules::JWT)
}

/// Decode unpadded base64url, or `None` when `part` is not base64url.
///
/// Written here rather than taken from a crate because it decodes one JWT
/// header, and the gate's dependency list is part of what B-10 promises: a
/// pure function with nothing under it that could read a clock.
fn base64url_decode(part: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(part.len() * 3 / 4);
    let mut accumulator: u32 = 0;
    let mut bits: u32 = 0;
    for character in part.chars() {
        let value = match character {
            'A'..='Z' => u32::from(character as u8 - b'A'),
            'a'..='z' => u32::from(character as u8 - b'a') + 26,
            '0'..='9' => u32::from(character as u8 - b'0') + 52,
            '-' => 62,
            '_' => 63,
            _ => return None,
        };
        accumulator = (accumulator << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            let byte = u8::try_from((accumulator >> bits) & 0xff).ok()?;
            out.push(byte);
        }
    }
    Some(out)
}

/// The first unbroken high-entropy run inside `token`, and where it starts
/// (B-4).
///
/// A *run* rather than the whole token, and the difference is a real evasion
/// rather than a nicety. The token boundaries of [`DELIMITERS`] are the
/// characters prose puts *around* a pasted value; a person who writes
/// `secret:<value>` or `token~<value>` has glued a character that is neither
/// a boundary nor part of any credential alphabet onto one. A detector that
/// asked whether the *whole* token was made of [`Charset::Token`] characters
/// would answer no and let the credential through, which review found by
/// trying it. Measuring each maximal run instead means the punctuation a
/// human types around a secret cannot hide it, while the URL and JWT
/// detectors above keep their own view of the whole token, because a URL and
/// a JWT are made of characters this split would otherwise break apart
/// (D-12).
fn high_entropy(token: &str, rule: &EntropyRule) -> Option<(DetectorId, usize)> {
    let mut start = 0usize;
    for run in token.split(|c: char| !Charset::Token.admits(c)) {
        if is_high_entropy(run, rule) {
            return Some((crate::rules::HIGH_ENTROPY, start));
        }
        // Past this run, then past the one character that ended it. The
        // separator is whatever `split` matched, so its own width is what
        // advances the cursor; a multi-byte one advances by more than one.
        let after_run = start.saturating_add(run.len());
        start = token
            .get(after_run..)
            .and_then(|rest| rest.chars().next())
            .map_or(after_run, |sep| after_run.saturating_add(sep.len_utf8()));
    }
    None
}

/// Whether one unbroken run is a credential by the threshold rule.
fn is_high_entropy(run: &str, rule: &EntropyRule) -> bool {
    if run.len() < rule.min_len {
        return false;
    }
    if rule.require_mixed_case_and_digit {
        let mixed = run.chars().any(|c| c.is_ascii_uppercase())
            && run.chars().any(|c| c.is_ascii_lowercase())
            && run.chars().any(|c| c.is_ascii_digit());
        if !mixed {
            return false;
        }
    }
    shannon_bits(run) >= rule.threshold
}

/// The Shannon entropy of `token` in bits per character, in Q16 fixed point.
///
/// `H = log2(n) - (1/n) * sum(c_i * log2(c_i))`, which is the per-character
/// entropy written so that only one division is needed and every term is a
/// non-negative integer. Integer arithmetic throughout: the workspace denies
/// float arithmetic outside two named crates, and an integer comparison makes
/// B-10's "always the same verdict" true across targets rather than
/// approximately true (D-4).
#[must_use]
pub fn shannon_bits(token: &str) -> u64 {
    let mut counts = [0u32; 256];
    let mut total: u64 = 0;
    for byte in token.bytes() {
        if let Some(slot) = counts.get_mut(usize::from(byte)) {
            *slot = slot.saturating_add(1);
            total = total.saturating_add(1);
        }
    }
    if total < 2 {
        return 0;
    }
    let weighted: u64 = counts
        .iter()
        .filter(|count| **count > 0)
        .map(|count| u64::from(*count).saturating_mul(log2_q16(u64::from(*count))))
        .sum();
    log2_q16(total).saturating_sub(weighted / total)
}

/// `log2(x)` in Q16 fixed point, for `x >= 1`.
///
/// The integer part is the position of the leading bit. The fraction is the
/// classic repeated-squaring expansion: square the mantissa, and the bit that
/// carries out past 2 is the next fractional bit of the logarithm.
fn log2_q16(x: u64) -> u64 {
    if x == 0 {
        return 0;
    }
    let integer = u64::from(63 - x.leading_zeros());
    let mut mantissa: u128 = if integer >= 32 {
        u128::from(x) >> (integer - 32)
    } else {
        u128::from(x) << (32 - integer)
    };
    let mut fraction: u64 = 0;
    for bit in 0..crate::rules::ENTROPY_FRACTION_BITS {
        mantissa = (mantissa * mantissa) >> 32;
        if mantissa >= (2u128 << 32) {
            fraction |= 1 << (crate::rules::ENTROPY_FRACTION_BITS - 1 - bit);
            mantissa >>= 1;
        }
    }
    (integer << crate::rules::ENTROPY_FRACTION_BITS) | fraction
}
