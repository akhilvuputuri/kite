//! npm configuration that changes what gets installed: `npm_config_*`
//! environment variables, the project and user `.npmrc`, and `NODE_ENV`.
//!
//! Settings kite implements are applied. Settings that only affect
//! npm's own output, caching or credentials are ignored. Anything else that
//! would make npm behave differently is refused, so kite never quietly
//! produces a different result.

use std::collections::BTreeMap;
use std::path::Path;

#[derive(Clone, Debug, Default)]
pub struct Config {
    pub omit_dev: bool,
    pub omit_optional: bool,
    pub omit_peer: bool,
    pub ignore_scripts: bool,
    pub save_exact: bool,
}

/// Command-line settings, the highest configuration layer.
#[derive(Clone, Debug, Default)]
pub struct Cli {
    pub omit: Vec<String>,
    pub include: Vec<String>,
    pub production: bool,
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

/// The global npmrc: `<node prefix>/etc/npmrc`, where the prefix is the
/// folder above the `bin` holding `node`.
fn global_npmrc() -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    let node = std::env::split_paths(&path)
        .map(|d| d.join("node"))
        .find(|p| p.is_file())?;
    Some(node.parent()?.parent()?.join("etc").join("npmrc"))
}

/// The node-gyp npm bundles: `<prefix>/lib/node_modules/npm/node_modules/
/// node-gyp/bin/node-gyp.js`, next to the `node` on PATH.
fn bundled_node_gyp() -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    let node = std::env::split_paths(&path)
        .map(|d| d.join("node"))
        .find(|p| p.is_file())?;
    Some(
        node.parent()?
            .parent()?
            .join("lib/node_modules/npm/node_modules/node-gyp/bin/node-gyp.js"),
    )
}

fn read_layer(path: Option<std::path::PathBuf>, label: &str) -> BTreeMap<String, (String, String)> {
    let mut layer = BTreeMap::new();
    if let Some(path) = path
        && let Ok(text) = std::fs::read_to_string(&path)
    {
        parse_npmrc(&text, &format!("{label} ({})", path.display()), &mut layer);
    }
    layer
}

impl Config {
    /// Reads npm's configuration layers (global, user, project npmrc and the
    /// environment; the command line is applied by `omit`) and refuses
    /// settings kite does not implement.
    pub fn load(project: &Path, cli: &Cli) -> Result<Config, String> {
        // @npmcli/config: the environment and the project .npmrc can name
        // the user config, and they plus the user config can name the
        // global config or the prefix it lives under.
        let env = |k: &str| {
            std::env::var(format!("npm_config_{k}"))
                .or_else(|_| std::env::var(format!("NPM_CONFIG_{}", k.to_ascii_uppercase())))
                .ok()
                .filter(|v| !v.is_empty())
        };
        let project_layer = read_layer(Some(project.join(".npmrc")), "project .npmrc");
        let user = env("userconfig")
            .or_else(|| project_layer.get("userconfig").map(|v| v.0.clone()))
            .map(std::path::PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".npmrc")));
        let user_layer = read_layer(user, "user .npmrc");
        let from_layers = |k: &str| {
            env(k)
                .or_else(|| project_layer.get(k).map(|v| v.0.clone()))
                .or_else(|| user_layer.get(k).map(|v| v.0.clone()))
        };
        let global = from_layers("globalconfig").map(std::path::PathBuf::from).or_else(|| {
            from_layers("prefix")
                .map(|p| std::path::PathBuf::from(p).join("etc").join("npmrc"))
                .or_else(global_npmrc)
        });
        // Each key takes its value from the highest layer that sets it.
        let mut settings: BTreeMap<String, (String, String)> = BTreeMap::new();
        settings.extend(read_layer(global, "global npmrc"));
        settings.extend(user_layer);
        settings.extend(project_layer);
        let mut env_layer: BTreeMap<String, (String, String)> = BTreeMap::new();
        for (key, value) in std::env::vars() {
            let lower = key.to_ascii_lowercase();
            if let Some(name) = lower.strip_prefix("npm_config_") {
                // npm ignores empty environment values, and turns every `_`
                // but a leading one into `-` (`_auth` stays a credential).
                if name.is_empty() || value.is_empty() {
                    continue;
                }
                let name = match name.strip_prefix('_') {
                    Some(rest) => format!("_{}", rest.replace('_', "-")),
                    None => name.replace('_', "-"),
                };
                env_layer.insert(name, (value, format!("environment variable {key}")));
            }
        }
        settings.extend(env_layer);

        let mut refused: Vec<String> = Vec::new();
        let mut omit: Vec<String> = if std::env::var("NODE_ENV").as_deref() == Ok("production") {
            vec!["dev".into()]
        } else {
            Vec::new()
        };
        let mut include: Vec<String> = Vec::new();
        let mut production: Option<bool> = None;
        let mut only: Option<String> = None;
        let mut also: Option<String> = None;
        let mut dev = false;
        let mut optional: Option<bool> = None;
        let mut c = Config::default();
        let list = |value: &str| -> Vec<String> {
            value
                .split([',', ' '])
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        };
        for (key, (value, source)) in &settings {
            // Credentials and per-registry settings are scoped `//host/:key`.
            if key.starts_with("//") || key.starts_with('_') || HARMLESS.contains(&key.as_str()) {
                continue;
            }
            match key.as_str() {
                "omit" => omit = list(value),
                "include" => include = list(value),
                "production" => production = Some(parse_bool(key, value)?),
                "only" => only = Some(value.clone()),
                "also" => also = Some(value.clone()),
                "dev" => dev = parse_bool(key, value)?,
                "optional" => optional = Some(parse_bool(key, value)?),
                "ignore-scripts" => c.ignore_scripts = parse_bool(key, value)?,
                "save-exact" => c.save_exact = parse_bool(key, value)?,
                "save" | "package-lock" | "bin-links" if parse_bool(key, value)? => {}
                "engine-strict" | "legacy-bundling" | "global-style" if !parse_bool(key, value)? => {}
                "save-prefix" if value == "^" => {}
                "registry" if value.trim_end_matches('/') == "https://registry.npmjs.org" => {}
                "install-strategy" if value == "hoisted" => {}
                "script-shell" if value == "sh" || value == "/bin/sh" => {}
                // npm passes its own bundled node-gyp to scripts it runs.
                "node-gyp" if bundled_node_gyp().is_some_and(|p| p == std::path::Path::new(value)) => {}
                "legacy-peer-deps" | "strict-peer-deps" | "force" | "prefer-dedupe" if !parse_bool(key, value)? => {}
                _ => refused.push(format!("{key}={value} ({source})")),
            }
        }
        // The command line replaces lower layers for list settings.
        if !cli.omit.is_empty() {
            omit = cli.omit.clone();
        }
        if !cli.include.is_empty() {
            include = cli.include.clone();
        }
        if cli.production {
            production = Some(true);
        }
        // @npmcli/config buildOmitList.
        if only.as_deref().is_some_and(|o| o == "prod" || o == "production") || production == Some(true) {
            omit.push("dev".into());
        } else if production == Some(false) {
            include.push("dev".into());
        }
        if also.as_deref().is_some_and(|a| a.starts_with("dev")) || dev {
            include.push("dev".into());
        }
        match optional {
            Some(false) => omit.push("optional".into()),
            Some(true) => include.push("optional".into()),
            None => {}
        }
        for item in omit.iter().chain(include.iter()) {
            if !matches!(item.as_str(), "dev" | "optional" | "peer" | "prod") {
                refused.push(format!("omit/include value {item:?}"));
            }
        }
        let omitted = |t: &str| omit.iter().any(|o| o == t) && !include.iter().any(|i| i == t);
        c.omit_dev = omitted("dev");
        c.omit_optional = omitted("optional");
        c.omit_peer = omitted("peer");
        if !refused.is_empty() {
            return Err(format!(
                "these npm settings would change npm's result, and kite does not support them yet:\n  {}",
                refused.join("\n  ")
            ));
        }
        Ok(c)
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
