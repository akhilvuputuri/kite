//! npm configuration that changes what gets installed: `npm_config_*`
//! environment variables, the project and user `.npmrc`, and `NODE_ENV`.
//!
//! Settings lockstep implements are applied. Settings that only affect
//! npm's own output, caching or credentials are ignored. Anything else that
//! would make npm behave differently is refused, so lockstep never quietly
//! produces a different result.

use std::collections::BTreeMap;
use std::path::Path;

#[derive(Clone, Debug, Default)]
pub struct Config {
    pub omit_dev: bool,
    pub omit_optional: bool,
    pub omit_peer: bool,
    pub include_dev: bool,
    pub include_optional: bool,
    pub include_peer: bool,
    pub ignore_scripts: bool,
    pub save_exact: bool,
}

/// Keys with no effect on what is resolved or installed.
const HARMLESS: &[&str] = &[
    "audit",
    "audit-level",
    "cache",
    "cache-max",
    "cache-min",
    "color",
    "editor",
    "fetch-retries",
    "fetch-retry-factor",
    "fetch-retry-maxtimeout",
    "fetch-retry-mintimeout",
    "fetch-timeout",
    "fund",
    "global-prefix",
    "globalconfig",
    "heading",
    "init-author-email",
    "init-author-name",
    "init-author-url",
    "init-license",
    "init-module",
    "init-version",
    "local-prefix",
    "loglevel",
    "logs-dir",
    "logs-max",
    "maxsockets",
    "node-gyp",
    "noproxy",
    "npm-version",
    "prefer-offline",
    "prefer-online",
    "prefix",
    "progress",
    "proxy",
    "https-proxy",
    "sign-git-commit",
    "sign-git-tag",
    "unicode",
    "update-notifier",
    "user-agent",
    "userconfig",
    "usage",
    "user",
    "always-auth",
    "email",
    "git-tag-version",
    "message",
    "tag-version-prefix",
    "version-git-tag",
    "foreground-scripts",
    "script-shell",
];

fn parse_bool(key: &str, value: &str) -> Result<bool, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "" => Ok(true),
        "false" | "0" => Ok(false),
        other => Err(format!("npm config {key}={other:?} is not a boolean")),
    }
}

/// `.npmrc` lines are `key = value`; `;` and `#` start comments.
fn parse_npmrc(text: &str, source: &str, out: &mut BTreeMap<String, (String, String)>) {
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
            continue;
        }
        let (key, value) = match line.split_once('=') {
            Some((k, v)) => (k.trim(), v.trim()),
            None => (line, "true"),
        };
        let key = key.trim_end_matches("[]").to_ascii_lowercase();
        let value = value.trim_matches('"').to_string();
        // Later sources override earlier ones, except list keys accumulate.
        match out.get_mut(&key) {
            Some(existing) if key == "omit" || key == "include" => {
                existing.0.push(',');
                existing.0.push_str(&value);
            }
            _ => {
                out.insert(key, (value, source.to_string()));
            }
        }
    }
}

impl Config {
    pub fn load(project: &Path) -> Result<Config, String> {
        // Precedence low to high: user .npmrc, project .npmrc, environment.
        let mut settings: BTreeMap<String, (String, String)> = BTreeMap::new();
        let user = std::env::var_os("NPM_CONFIG_USERCONFIG")
            .or_else(|| std::env::var_os("npm_config_userconfig"))
            .map(std::path::PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".npmrc")));
        for (path, label) in [(user, "user .npmrc"), (Some(project.join(".npmrc")), "project .npmrc")] {
            if let Some(path) = path
                && let Ok(text) = std::fs::read_to_string(&path)
            {
                let mut file = BTreeMap::new();
                parse_npmrc(&text, &format!("{label} ({})", path.display()), &mut file);
                settings.extend(file);
            }
        }
        for (key, value) in std::env::vars() {
            let lower = key.to_ascii_lowercase();
            if let Some(name) = lower.strip_prefix("npm_config_") {
                if name.is_empty() {
                    continue;
                }
                settings.insert(name.replace('_', "-"), (value, format!("environment variable {key}")));
            }
        }
        let mut c = Config::default();
        let mut refused: Vec<String> = Vec::new();
        for (key, (value, source)) in &settings {
            // Credentials and per-registry settings are scoped `//host/:key`.
            if key.starts_with("//") || key.starts_with('_') {
                continue;
            }
            if HARMLESS.contains(&key.as_str()) {
                continue;
            }
            match key.as_str() {
                "omit" | "include" => {
                    for item in value.split([',', ' ']).filter(|s| !s.is_empty()) {
                        let (dev, opt, peer) = match item {
                            "dev" => (true, false, false),
                            "optional" => (false, true, false),
                            "peer" => (false, false, true),
                            "prod" => continue,
                            other => {
                                refused.push(format!("{key}={other} ({source})"));
                                continue;
                            }
                        };
                        if key == "omit" {
                            c.omit_dev |= dev;
                            c.omit_optional |= opt;
                            c.omit_peer |= peer;
                        } else {
                            c.include_dev |= dev;
                            c.include_optional |= opt;
                            c.include_peer |= peer;
                        }
                    }
                }
                "production" => c.omit_dev |= parse_bool(key, value)?,
                "only" if matches!(value.as_str(), "prod" | "production") => c.omit_dev = true,
                "dev" => c.include_dev |= parse_bool(key, value)?,
                "optional" if !parse_bool(key, value)? => c.omit_optional = true,
                "optional" => {}
                "ignore-scripts" => c.ignore_scripts = parse_bool(key, value)?,
                "save-exact" => c.save_exact = parse_bool(key, value)?,
                "save" | "package-lock" | "bin-links" if parse_bool(key, value)? => {}
                "engine-strict" | "legacy-bundling" | "global-style" if !parse_bool(key, value)? => {}
                "save-prefix" if value == "^" => {}
                "registry" if value.trim_end_matches('/') == "https://registry.npmjs.org" => {}
                "install-strategy" if value == "hoisted" => {}
                "legacy-peer-deps" | "strict-peer-deps" | "force" | "prefer-dedupe" if !parse_bool(key, value)? => {}
                _ => refused.push(format!("{key}={value} ({source})")),
            }
        }
        if std::env::var("NODE_ENV").as_deref() == Ok("production") {
            c.omit_dev = true;
        }
        if !refused.is_empty() {
            return Err(format!(
                "these npm settings would change npm's result, and lockstep does not support them yet:\n  {}",
                refused.join("\n  ")
            ));
        }
        Ok(c)
    }

    /// `omit = unique(omit) - include`, as npm computes it.
    pub fn omit(&self, cli: (bool, bool, bool), cli_include: (bool, bool, bool)) -> (bool, bool, bool) {
        let dev = (self.omit_dev || cli.0) && !(self.include_dev || cli_include.0);
        let optional = (self.omit_optional || cli.1) && !(self.include_optional || cli_include.1);
        let peer = (self.omit_peer || cli.2) && !(self.include_peer || cli_include.2);
        (dev, optional, peer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_npmrc() {
        let mut m = BTreeMap::new();
        parse_npmrc(
            "; comment\nomit=dev\n//registry.npmjs.org/:_authToken=x\nfund = false\nomit[]=optional\n",
            "t",
            &mut m,
        );
        assert_eq!(m["omit"].0, "dev,optional");
        assert_eq!(m["fund"].0, "false");
        assert!(m.contains_key("//registry.npmjs.org/:_authtoken"));
    }
}
