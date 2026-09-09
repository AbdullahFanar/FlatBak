//! The `.flatbak` backup format: container framing, manifest, writer and reader.

pub mod container;
pub mod manifest;
pub mod reader;
pub mod writer;

/// Top-level directory inside the payload holding application data.
pub const PAYLOAD_DATA_DIR: &str = "data";

/// Current time as an RFC 3339 timestamp in UTC.
pub fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Formats an RFC 3339 timestamp for display in the user's local time.
///
/// Falls back to the raw string if it cannot be parsed, so a timestamp written
/// by a future version is shown rather than hidden.
pub fn format_timestamp(raw: &str) -> String {
    match chrono::DateTime::parse_from_rfc3339(raw) {
        Ok(parsed) => parsed
            .with_timezone(&chrono::Local)
            .format("%-d %B %Y at %H:%M")
            .to_string(),
        Err(_) => raw.to_owned(),
    }
}

/// A short relative description such as "2 days ago", for recent backup rows.
pub fn format_relative(raw: &str) -> String {
    let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(raw) else {
        return raw.to_owned();
    };
    let elapsed = chrono::Utc::now().signed_duration_since(parsed.with_timezone(&chrono::Utc));
    let minutes = elapsed.num_minutes();
    if minutes < 1 {
        "just now".to_owned()
    } else if minutes < 60 {
        plural(minutes, "minute")
    } else if elapsed.num_hours() < 24 {
        plural(elapsed.num_hours(), "hour")
    } else if elapsed.num_days() < 30 {
        plural(elapsed.num_days(), "day")
    } else if elapsed.num_days() < 365 {
        plural(elapsed.num_days() / 30, "month")
    } else {
        plural(elapsed.num_days() / 365, "year")
    }
}

fn plural(count: i64, unit: &str) -> String {
    if count == 1 {
        format!("1 {unit} ago")
    } else {
        format!("{count} {unit}s ago")
    }
}
