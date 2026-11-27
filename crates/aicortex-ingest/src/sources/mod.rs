//! The adapters that bring a history in (spec 031).
//!
//! Each module is a [`crate::SourceAdapter`] over a file the user uploaded
//! ([`crate::upload`]): parsing and extraction happen in this process and
//! nothing here reaches a host (B-8), so every adapter declares none.
//!
//! This module holds what they share: reading a staged upload, the cursor
//! over its items, and the time and identifier helpers the conversation
//! rules of B-9 need.

use std::sync::{Arc, Mutex};

use aicortex_types::Scope;
use rahi_types::UnixSeconds;
use ring::digest;
use serde_json::Value;

use crate::adapter::{AdapterContext, AdapterError, AdapterResult, Page, RawItem};
use crate::cursor::Cursor;
use crate::upload::{Staging, UploadId};

pub mod chat_export;

/// The option every upload backed adapter takes: the id of a staged file.
pub const UPLOAD_OPTION: &str = "upload";

/// Parsed items, kept between the pages of one run so a large export is read
/// and parsed once rather than once per page.
#[derive(Debug, Default)]
pub(crate) struct Loaded(Mutex<Option<Held>>);

type Held = (Scope, UploadId, Arc<Vec<RawItem>>);

impl Loaded {
    /// The items of the upload the run names, parsed with `parse` on first
    /// use.
    pub(crate) fn items(
        &self,
        staging: &Staging,
        ctx: &AdapterContext,
        parse: impl FnOnce(&[u8]) -> AdapterResult<Vec<RawItem>>,
    ) -> AdapterResult<Arc<Vec<RawItem>>> {
        let (scope, id) = upload_of(ctx)?;
        let mut held = self.0.lock().map_err(|_| poisoned())?;
        if let Some((held_scope, held_id, items)) = held.as_ref()
            && *held_scope == scope
            && *held_id == id
        {
            return Ok(Arc::clone(items));
        }
        let bytes = staging.read(&scope, &id).map_err(|_| {
            AdapterError::Fatal("the upload is not staged for this scope".to_owned())
        })?;
        let items = Arc::new(parse(&bytes)?);
        *held = Some((scope, id, Arc::clone(&items)));
        Ok(items)
    }

    /// Forget what was parsed.
    pub(crate) fn clear(&self) {
        if let Ok(mut held) = self.0.lock() {
            *held = None;
        }
    }
}

fn poisoned() -> AdapterError {
    AdapterError::Fatal("an adapter's cache is unusable".to_owned())
}

/// The scope and the upload a run names (B-1).
pub(crate) fn upload_of(ctx: &AdapterContext) -> AdapterResult<(Scope, UploadId)> {
    let scope = ctx
        .scope()
        .cloned()
        .ok_or_else(|| AdapterError::Fatal("the run names no scope".to_owned()))?;
    let id = ctx
        .options()
        .get(UPLOAD_OPTION)
        .and_then(Value::as_str)
        .ok_or_else(|| AdapterError::Fatal("the run names no upload".to_owned()))
        .and_then(|text| {
            UploadId::parse(text)
                .map_err(|_| AdapterError::Fatal("the run names a malformed upload id".to_owned()))
        })?;
    Ok((scope, id))
}

/// The page of `items` that starts where `cursor` says (an item offset).
pub(crate) fn page_of(
    items: &[RawItem],
    ctx: &AdapterContext,
    cursor: Option<&Cursor>,
) -> AdapterResult<Page> {
    let start = match cursor {
        None => 0,
        Some(cursor) => cursor.as_str().parse::<usize>().map_err(|_| {
            AdapterError::Fatal("the cursor is not one this source wrote".to_owned())
        })?,
    };
    let end = start.saturating_add(ctx.page_limit()).min(items.len());
    let next = if end < items.len() {
        Some(Cursor::new(end.to_string()).map_err(|error| AdapterError::Fatal(error.to_string()))?)
    } else {
        None
    };
    Ok(Page {
        items: items.get(start..end).unwrap_or_default().to_vec(),
        next,
    })
}

/// A time an export states, as Unix seconds: a number (seconds, fractions
/// dropped) or an RFC 3339 string. `None` for anything else.
pub(crate) fn time_of(value: &Value) -> Option<u64> {
    match value {
        Value::Number(number) => number
            .as_u64()
            .or_else(|| number.as_f64().filter(|f| *f >= 0.0).map(|f| f as u64)),
        Value::String(text) => parse_rfc3339(text),
        _ => None,
    }
}

/// `YYYY-MM-DDTHH:MM:SS[.f](Z|+hh:mm|-hh:mm)` as Unix seconds, or `None`.
pub(crate) fn parse_rfc3339(text: &str) -> Option<u64> {
    let bytes = text.as_bytes();
    let num = |from: usize, to: usize| -> Option<i64> {
        let part = text.get(from..to)?;
        part.bytes()
            .all(|b| b.is_ascii_digit())
            .then(|| part.parse().ok())
            .flatten()
    };
    if bytes.len() < 20
        || !matches!(bytes.get(4), Some(b'-'))
        || !matches!(bytes.get(7), Some(b'-'))
        || !matches!(bytes.get(10), Some(b'T' | b't' | b' '))
        || !matches!(bytes.get(13), Some(b':'))
        || !matches!(bytes.get(16), Some(b':'))
    {
        return None;
    }
    let (year, month, day) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (hour, minute, second) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    let mut at = 19;
    if matches!(bytes.get(at), Some(b'.')) {
        at += 1;
        while bytes.get(at).is_some_and(u8::is_ascii_digit) {
            at += 1;
        }
    }
    let offset = match bytes.get(at)? {
        b'Z' | b'z' if at + 1 == bytes.len() => 0,
        sign @ (b'+' | b'-')
            if at + 6 == bytes.len() && matches!(bytes.get(at + 3), Some(b':')) =>
        {
            let seconds = num(at + 1, at + 3)? * 3600 + num(at + 4, at + 6)? * 60;
            if *sign == b'+' { seconds } else { -seconds }
        }
        _ => return None,
    };
    let total =
        days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second - offset;
    u64::try_from(total).ok()
}

/// Days since 1970-01-01 of a proleptic Gregorian date.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year.rem_euclid(400);
    let doy = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Unix seconds as `YYYY-MM-DDTHH:MM:SSZ`.
pub(crate) fn rfc3339_utc(at: u64) -> String {
    let secs = i64::try_from(at).unwrap_or(i64::MAX / 2);
    let (days, rest) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let doe = shifted.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

/// The derived conversation id of B-9: `d:` and the lowercase hex SHA-256 of
/// the parser's identifier, the creation time in RFC 3339 UTC, and the first
/// text, joined by NUL.
#[must_use]
pub fn derived_conversation_id(parser: &str, created: UnixSeconds, first_text: &str) -> String {
    let mut material = Vec::new();
    material.extend_from_slice(parser.as_bytes());
    material.push(0);
    material.extend_from_slice(rfc3339_utc(created.get()).as_bytes());
    material.push(0);
    material.extend_from_slice(first_text.as_bytes());
    let hash = digest::digest(&digest::SHA256, &material);
    let mut id = String::from("d:");
    for byte in hash.as_ref() {
        id.push_str(&format!("{byte:02x}"));
    }
    id
}
