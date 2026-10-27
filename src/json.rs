//! JSON output that matches `JSON.stringify(value, null, indent) + newline`,
//! the way npm writes `package.json` and `package-lock.json`.

use serde_json::Value;
use serde_json::ser::{PrettyFormatter, Serializer};

/// Formatting detected from an existing file, as `@npmcli/package-json` and
/// arborist's Shrinkwrap do: the indent of the first indented line and the
/// first newline sequence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Style {
    pub indent: String,
    pub newline: String,
}

impl Default for Style {
    fn default() -> Self {
        Style {
            indent: "  ".to_string(),
            newline: "\n".to_string(),
        }
    }
}

impl Style {
    pub fn detect(text: &str) -> Style {
        // json-parse-even-better-errors: /^\s*[{[]((?:\r?\n)+)([\s\t]*)/
        let trimmed = text.trim_start();
        let mut style = Style::default();
        let Some(rest) = trimmed.strip_prefix('{').or_else(|| trimmed.strip_prefix('[')) else {
            return style;
        };
        let newlines_end = rest.find(|c| c != '\r' && c != '\n').unwrap_or(rest.len());
        let newlines = &rest[..newlines_end];
        if newlines.is_empty() {
            return style;
        }
        style.newline = if newlines.starts_with("\r\n") {
            "\r\n".to_string()
        } else {
            "\n".to_string()
        };
        let after = &rest[newlines_end..];
        let indent_end = after
            .find(|c: char| !c.is_whitespace() || c == '\r' || c == '\n')
            .unwrap_or(after.len());
        style.indent = after[..indent_end].to_string();
        style
    }
}

/// Writes through a temporary file and a rename, like write-file-atomic,
/// keeping the existing file's permissions.
pub fn write_atomic(path: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_file_name(format!(
        ".{}.lockstep-{}",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("file"),
        std::process::id()
    ));
    std::fs::write(&tmp, data)?;
    if let Ok(meta) = std::fs::metadata(path) {
        std::fs::set_permissions(&tmp, meta.permissions())?;
    }
    std::fs::rename(&tmp, path)
}

pub fn stringify(value: &Value, style: &Style) -> String {
    let mut out = Vec::new();
    if style.indent.is_empty() {
        serde_json::to_writer(&mut out, value).expect("serialising JSON to memory");
    } else {
        let formatter = PrettyFormatter::with_indent(style.indent.as_bytes());
        let mut serializer = Serializer::with_formatter(&mut out, formatter);
        serde::Serialize::serialize(value, &mut serializer).expect("serialising JSON to memory");
    }
    let mut text = String::from_utf8(out).expect("serde_json writes UTF-8");
    if style.newline != "\n" {
        text = text.replace('\n', &style.newline);
    }
    text.push_str(&style.newline);
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_tabs_and_crlf() {
        let style = Style::detect("{\r\n\t\"a\": 1\r\n}\r\n");
        assert_eq!(style.indent, "\t");
        assert_eq!(style.newline, "\r\n");
    }

    #[test]
    fn matches_json_stringify() {
        let value: Value = serde_json::from_str(r#"{"b":{},"a":[1,{"c":"\u0001é"}],"e":[]}"#).unwrap();
        assert_eq!(
            stringify(&value, &Style::default()),
            "{\n  \"b\": {},\n  \"a\": [\n    1,\n    {\n      \"c\": \"\\u0001é\"\n    }\n  ],\n  \"e\": []\n}\n"
        );
    }
}
