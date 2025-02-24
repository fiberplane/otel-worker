use anyhow::{Context, Result};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

pub fn parse_rfc3339_from_str(arg: &str) -> Result<OffsetDateTime> {
    OffsetDateTime::parse(arg, &Rfc3339).context("failed to parse rfc3339 timestamp")
}
