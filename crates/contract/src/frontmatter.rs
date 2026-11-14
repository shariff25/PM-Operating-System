//! Splitting a markdown file into YAML front matter and body.

use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum FrontMatterError {
    #[error("file does not start with a `---` front matter block")]
    Missing,
    #[error("front matter block is not closed with `---`")]
    Unclosed,
}

/// A markdown file split into its front matter (raw YAML) and body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document<'a> {
    pub yaml: &'a str,
    pub body: &'a str,
}

/// Split `text` into front matter and body.
///
/// The file must start with a line that is exactly `---`; the front matter runs to the
/// next line that is exactly `---`. A leading UTF-8 BOM and CRLF line endings are accepted.
pub fn split(text: &str) -> Result<Document<'_>, FrontMatterError> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let rest = text
        .strip_prefix("---\r\n")
        .or_else(|| text.strip_prefix("---\n"))
        .ok_or(FrontMatterError::Missing)?;

    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        if line.trim_end_matches(['\r', '\n']) == "---" {
            let yaml = &rest[..offset];
            let body = &rest[offset + line.len()..];
            return Ok(Document { yaml, body });
        }
        offset += line.len();
    }
    // A closing `---` on the last line with no trailing newline.
    if rest[offset..].trim_end() == "---" {
        return Ok(Document {
            yaml: &rest[..offset],
            body: "",
        });
    }
    Err(FrontMatterError::Unclosed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_front_matter_and_body() {
        let doc = split("---\nname: a\n---\n# Body\n").unwrap();
        assert_eq!(doc.yaml, "name: a\n");
        assert_eq!(doc.body, "# Body\n");
    }

    #[test]
    fn accepts_crlf_and_bom() {
        let doc = split("\u{feff}---\r\nname: a\r\n---\r\nbody").unwrap();
        assert_eq!(doc.yaml, "name: a\r\n");
        assert_eq!(doc.body, "body");
    }

    #[test]
    fn closing_marker_at_end_of_file() {
        let doc = split("---\nname: a\n---").unwrap();
        assert_eq!(doc.yaml, "name: a\n");
        assert_eq!(doc.body, "");
    }

    #[test]
    fn missing_and_unclosed() {
        assert_eq!(split("# no front matter"), Err(FrontMatterError::Missing));
        assert_eq!(split("---\nname: a\n"), Err(FrontMatterError::Unclosed));
    }
}
