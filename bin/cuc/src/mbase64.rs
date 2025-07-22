use anyhow::Context;
use base64::prelude::*;

pub fn encode<S>(source: S) -> String
where
    S: AsRef<str>,
{
    let encoded = BASE64_STANDARD.encode(source.as_ref());
    encoded
}

pub fn decode<S>(source: S) -> anyhow::Result<String>
where
    S: AsRef<str>,
{
    let encoded = BASE64_STANDARD
        .decode(source.as_ref())
        .context("failed to decode")?;
    Ok(String::from_utf8(encoded)?)
}
