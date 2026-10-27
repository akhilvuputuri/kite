//! The dependency spec forms lockstep supports, classified the way
//! npm-package-arg does for registry and directory specs.

use crate::semver::{Range, Version};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Spec {
    /// A semver range (including `*` and the empty spec).
    Range { range: String },
    /// An exact version.
    Version { version: String },
    /// A dist-tag such as `latest`.
    Tag { tag: String },
    /// `npm:<name>@<spec>`
    Alias { name: String, spec: Box<Spec> },
    /// `file:<dir>` (or a relative path) to a local folder.
    Directory { path: String },
}

pub fn parse(name: &str, raw: &str) -> Result<Spec, String> {
    let spec = raw.trim();
    if let Some(rest) = spec.strip_prefix("npm:") {
        let (real, sub) = split_name_spec(rest);
        if real.is_empty() {
            return Err(format!("{name}@{raw}: alias without a package name"));
        }
        let sub =
            parse_registry(sub.unwrap_or("*")).ok_or_else(|| format!("{name}@{raw}: unsupported alias target"))?;
        return Ok(Spec::Alias {
            name: real.to_string(),
            spec: Box::new(sub),
        });
    }
    if let Some(path) = spec.strip_prefix("file:") {
        if path.ends_with(".tgz") || path.ends_with(".tar.gz") || path.ends_with(".tar") {
            return Err(format!("{name}@{raw}: tarball file dependencies are not supported"));
        }
        return Ok(Spec::Directory { path: path.to_string() });
    }
    if spec.starts_with("./") || spec.starts_with("../") || spec.starts_with('/') || spec.starts_with("~/") {
        return Ok(Spec::Directory { path: spec.to_string() });
    }
    if spec.contains("://")
        || spec.starts_with("git+")
        || spec.starts_with("github:")
        || spec.starts_with("gitlab:")
        || spec.starts_with("bitbucket:")
        || spec.starts_with("gist:")
        || spec.starts_with("link:")
        || spec.starts_with("workspace:")
        || (spec.contains('/') && !spec.contains(' ') && !spec.starts_with('@') && Range::parse(spec).is_none())
    {
        return Err(format!(
            "{name}@{raw}: only registry versions, ranges, tags, npm: aliases and file: folders are supported"
        ));
    }
    parse_registry(spec).ok_or_else(|| format!("{name}@{raw}: invalid version, range or tag"))
}

fn parse_registry(spec: &str) -> Option<Spec> {
    if Version::parse(spec).is_some() {
        return Some(Spec::Version {
            version: spec.to_string(),
        });
    }
    if spec.is_empty() || Range::parse(spec).is_some() {
        return Some(Spec::Range {
            range: if spec.is_empty() {
                "*".to_string()
            } else {
                spec.to_string()
            },
        });
    }
    // encodeURIComponent(spec) === spec
    if spec
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b))
    {
        return Some(Spec::Tag { tag: spec.to_string() });
    }
    None
}

/// Splits `name@spec` (scoped names keep their leading `@`).
pub fn split_name_spec(text: &str) -> (&str, Option<&str>) {
    let search_from = if text.starts_with('@') { 1 } else { 0 };
    match text[search_from..].find('@') {
        Some(i) => (&text[..search_from + i], Some(&text[search_from + i + 1..])),
        None => (text, None),
    }
}

/// Resolves a directory spec against the folder of the package declaring it.
pub fn resolve_path(base: &str, path: &str) -> String {
    let joined = if path.starts_with('/') {
        path.to_string()
    } else {
        format!("{base}/{path}")
    };
    let mut parts: Vec<&str> = Vec::new();
    for part in joined.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    format!("/{}", parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies() {
        assert_eq!(parse("a", "^1.2.3").unwrap(), Spec::Range { range: "^1.2.3".into() });
        assert_eq!(
            parse("a", "1.2.3").unwrap(),
            Spec::Version {
                version: "1.2.3".into()
            }
        );
        assert_eq!(parse("a", "").unwrap(), Spec::Range { range: "*".into() });
        assert_eq!(parse("a", "latest").unwrap(), Spec::Tag { tag: "latest".into() });
        assert_eq!(
            parse("a", "file:./pkg").unwrap(),
            Spec::Directory { path: "./pkg".into() }
        );
        assert!(matches!(parse("a", "npm:@s/b@^2").unwrap(), Spec::Alias { name, .. } if name == "@s/b"));
        assert!(parse("a", "github:x/y").is_err());
        assert!(parse("a", "https://x/y.tgz").is_err());
    }

    #[test]
    fn splits() {
        assert_eq!(split_name_spec("@s/a@^1"), ("@s/a", Some("^1")));
        assert_eq!(split_name_spec("a"), ("a", None));
        assert_eq!(split_name_spec("a@"), ("a", Some("")));
        assert_eq!(resolve_path("/p", "./coding_runtime_pi"), "/p/coding_runtime_pi");
        assert_eq!(resolve_path("/p/a", "../b"), "/p/b");
    }
}
