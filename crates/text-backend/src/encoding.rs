use chardetng::{EncodingDetector, Iso2022JpDetection, Utf8Detection};
use encoding_rs::{Encoding, UTF_16BE, UTF_16LE, UTF_8};
use nya_domain::{Error, Result};

pub(crate) struct DecodedText {
    pub text: String,
    pub encoding: &'static Encoding,
    pub bom: bool,
}

fn lookup(label: &str) -> Result<&'static Encoding> {
    Encoding::for_label_no_replacement(label.as_bytes())
        .ok_or_else(|| Error(format!("Unsupported encoding: {label}")))
}

pub(crate) fn decode(bytes: &[u8], requested: Option<&str>) -> Result<DecodedText> {
    if bytes.starts_with(&[0xff, 0xfe, 0, 0]) || bytes.starts_with(&[0, 0, 0xfe, 0xff]) {
        return Err(Error("UTF-32 is not supported".into()));
    }
    let forced = requested.map(lookup).transpose()?;
    let (encoding, bom_length) = if let Some((encoding, length)) = Encoding::for_bom(bytes) {
        if forced.is_some_and(|forced| forced != encoding) {
            return Err(Error(
                "Requested encoding conflicts with the file BOM".into(),
            ));
        }
        (encoding, length)
    } else if let Some(encoding) = forced {
        (encoding, 0)
    } else if std::str::from_utf8(bytes).is_ok() {
        (UTF_8, 0)
    } else {
        let mut detector = EncodingDetector::new(Iso2022JpDetection::Allow);
        detector.feed(bytes, true);
        (detector.guess(None, Utf8Detection::Allow), 0)
    };
    let text = encoding
        .decode_without_bom_handling_and_without_replacement(&bytes[bom_length..])
        .ok_or_else(|| Error(format!("Invalid {} input", encoding.name())))?
        .into_owned();
    Ok(DecodedText {
        text,
        encoding,
        bom: bom_length > 0,
    })
}

pub(crate) fn encode(text: &str, label: &str, bom: bool) -> Result<Vec<u8>> {
    let encoding = lookup(label)?;
    let prefix: &[u8] = if !bom {
        &[]
    } else if encoding == UTF_8 {
        &[0xef, 0xbb, 0xbf]
    } else if encoding == UTF_16LE {
        &[0xff, 0xfe]
    } else if encoding == UTF_16BE {
        &[0xfe, 0xff]
    } else {
        return Err(Error(format!("{} does not support a BOM", encoding.name())));
    };
    let mut bytes = prefix.to_vec();
    // encoding_rs treats UTF-16 as decode-only; encode its code units explicitly.
    if encoding == UTF_16LE || encoding == UTF_16BE {
        for unit in text.encode_utf16() {
            let pair = if encoding == UTF_16LE {
                unit.to_le_bytes()
            } else {
                unit.to_be_bytes()
            };
            bytes.extend_from_slice(&pair);
        }
    } else {
        let (encoded, _, had_errors) = encoding.encode(text);
        if had_errors {
            return Err(Error(format!(
                "Text cannot be represented in {}",
                encoding.name()
            )));
        }
        bytes.extend_from_slice(&encoded);
    }
    Ok(bytes)
}
