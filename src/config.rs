//! Load and validate `config.toml`, then apply command-line overrides.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};
use directories::{BaseDirs, ProjectDirs};
use serde::Deserialize;

pub const EXAMPLE_CONFIG: &str = r#"# jellydl configuration

# URL of the Jellyfin server (no trailing slash needed).
server_url = "http://localhost:8096"

# Where downloaded files go. "~" is expanded.
# For example: <download_dir>/Elodie.2001/Elodie.2001.mkv
#              <download_dir>/Garden.Diaries/S01/Garden.Diaries.S01E03.Pilot.mkv
download_dir = "~/Videos/jellyfin"

# How many downloads run at the same time. You can change it at runtime with + and -.
max_parallel = 3

# Optional proxy. Supported schemes: http://, https://, socks5://, socks5h://
# Use socks5h:// to resolve host names through the proxy.
# If not set, the HTTP_PROXY / HTTPS_PROXY / ALL_PROXY environment variables are used.
# proxy = "socks5h://user:pass@127.0.0.1:1080"
# no_proxy = "localhost,192.168.0.0/16"

# Set to true only for a server with a self-signed certificate.
accept_invalid_certs = false

# User-Agent header sent with every request. If not set: "jellydl/<version>".
# Change it only when a proxy or firewall blocks unknown clients. For example:
# user_agent = "Mozilla/5.0 (X11; Linux x86_64; rv:145.0) Gecko/20100101 Firefox/145.0"

# Limit the total download speed (all downloads together). Examples: "500K", "2MB", "1.5M".
# If not set: no limit.
# max_speed = "2MB"

# Wait a random time (in seconds, between min and max) before each new download starts.
# This spreads the load on a small server.
# start_delay = [5, 30]

# Save the login token and use it again at the next start, instead of a new login each time.
# Only used with username/password. Default: true.
# reuse_session = true

# Show the poster of the selected item in the details pane. Default: true.
# Terminals with an image protocol (kitty, Ghostty, WezTerm, iTerm2, Konsole, foot...) show the
# image. Other terminals show a preview made of colored characters.
# posters = true

# Values Jellyfin requires to identify the client (Dashboard -> Devices).
# Each one is optional: if not set, the default is used.
[identity]
# client    = "jellydl"     # default: "jellydl"
# device    = "jellydl"     # default: "jellydl"
# version   = "0.1.0"       # default: the jellydl version
# device_id = "..."         # default: generated once, then saved in the data folder

# Extra HTTP headers sent with every request.
[headers]
# "Accept-Language" = "en-US,en;q=0.5"

[auth]
# Either an API key (Dashboard -> API Keys)...
# api_key = "0123456789abcdef"
# ...or a username and a password (or a command that prints the password).
username = "me"
password = "change-me"
# password_cmd = "pass show jellyfin"
"#;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    server_url: String,
    download_dir: Option<String>,
    max_parallel: Option<usize>,
    proxy: Option<String>,
    no_proxy: Option<String>,
    #[serde(default)]
    accept_invalid_certs: bool,
    user_agent: Option<String>,
    max_speed: Option<SpeedValue>,
    start_delay: Option<Vec<u64>>,
    reuse_session: Option<bool>,
    posters: Option<bool>,
    #[serde(default)]
    headers: BTreeMap<String, String>,
    #[serde(default)]
    identity: IdentityConfig,
    auth: FileAuth,
}

/// `max_speed` can be a number (bytes per second) or a string such as "2MB".
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum SpeedValue {
    Bytes(u64),
    Text(String),
}

/// Overrides for the values Jellyfin requires in the `Authorization` header.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct IdentityConfig {
    pub client: Option<String>,
    pub device: Option<String>,
    pub version: Option<String>,
    pub device_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileAuth {
    api_key: Option<String>,
    username: Option<String>,
    password: Option<String>,
    password_cmd: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Auth {
    ApiKey(String),
    Password { username: String, password: String },
}

#[derive(Debug, Clone)]
pub struct Config {
    pub server_url: String,
    pub download_dir: PathBuf,
    pub max_parallel: usize,
    pub proxy: Option<String>,
    pub no_proxy: Option<String>,
    pub accept_invalid_certs: bool,
    pub user_agent: String,
    /// Total download speed limit in bytes per second.
    pub max_speed: Option<u64>,
    /// Random wait before each download starts, in seconds (min, max).
    pub start_delay: Option<(u64, u64)>,
    pub reuse_session: bool,
    /// Show posters in the details pane.
    pub posters: bool,
    pub headers: Vec<(String, String)>,
    pub identity: IdentityConfig,
    pub auth: Auth,
}

pub const DEFAULT_USER_AGENT: &str = concat!("jellydl/", env!("CARGO_PKG_VERSION"));

/// Values given on the command line. They win over the file.
#[derive(Debug, Default, Clone)]
pub struct Overrides {
    pub proxy: Option<String>,
    pub download_dir: Option<PathBuf>,
    pub max_parallel: Option<usize>,
    pub max_speed: Option<String>,
}

pub const MAX_PARALLEL_LIMIT: usize = 32;

pub fn project_dirs() -> Result<ProjectDirs> {
    ProjectDirs::from("", "", "jellydl").ok_or_else(|| anyhow!("cannot find the home directory"))
}

pub fn default_config_path() -> Result<PathBuf> {
    Ok(project_dirs()?.config_dir().join("config.toml"))
}

pub fn expand_tilde(p: &str) -> PathBuf {
    if (p == "~" || p.starts_with("~/"))
        && let Some(b) = BaseDirs::new()
    {
        return b
            .home_dir()
            .join(p.trim_start_matches('~').trim_start_matches('/'));
    }
    PathBuf::from(p)
}

/// Load the config file. If it does not exist, write an example and return an error
/// that tells the user to edit it.
pub fn load(path: &Path, ov: &Overrides) -> Result<Config> {
    if !path.exists() {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("cannot create {}", dir.display()))?;
        }
        std::fs::write(path, EXAMPLE_CONFIG)
            .with_context(|| format!("cannot write {}", path.display()))?;
        bail!(
            "no config found. An example was written to {}. Edit it, then start jellydl again.",
            path.display()
        );
    }
    let text =
        std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    parse(&text, ov).with_context(|| format!("invalid config {}", path.display()))
}

pub fn parse(text: &str, ov: &Overrides) -> Result<Config> {
    let f: FileConfig = toml::from_str(text)?;

    let server_url = f.server_url.trim().trim_end_matches('/').to_string();
    if !(server_url.starts_with("http://") || server_url.starts_with("https://")) {
        bail!("server_url must start with http:// or https://");
    }

    let auth = match (f.auth.api_key, f.auth.username) {
        (Some(k), None) if !k.trim().is_empty() => Auth::ApiKey(k.trim().to_string()),
        (None, Some(u)) => {
            let password = match (f.auth.password, f.auth.password_cmd) {
                (Some(p), None) => p,
                (None, Some(cmd)) => run_password_cmd(&cmd)?,
                (None, None) => String::new(),
                (Some(_), Some(_)) => bail!("[auth]: set password or password_cmd, not both"),
            };
            Auth::Password {
                username: u,
                password,
            }
        }
        (Some(_), Some(_)) => bail!("[auth]: set api_key or username, not both"),
        _ => bail!("[auth]: set api_key, or username and password"),
    };

    let download_dir = match (&ov.download_dir, &f.download_dir) {
        (Some(d), _) => d.clone(),
        (None, Some(d)) => expand_tilde(d),
        (None, None) => expand_tilde("~/Videos/jellyfin"),
    };

    let max_parallel = ov.max_parallel.or(f.max_parallel).unwrap_or(3);
    if !(1..=MAX_PARALLEL_LIMIT).contains(&max_parallel) {
        bail!("max_parallel must be between 1 and {MAX_PARALLEL_LIMIT}");
    }

    let proxy = ov
        .proxy
        .clone()
        .or(f.proxy)
        .filter(|p| !p.trim().is_empty());
    if let Some(p) = &proxy {
        const SCHEMES: [&str; 4] = ["http://", "https://", "socks5://", "socks5h://"];
        if !SCHEMES.iter().any(|s| p.starts_with(s)) {
            bail!("proxy must start with one of: {}", SCHEMES.join(", "));
        }
    }

    let max_speed = match (&ov.max_speed, &f.max_speed) {
        (Some(s), _) => parse_speed(s)?,
        (None, Some(SpeedValue::Text(s))) => parse_speed(s)?,
        (None, Some(SpeedValue::Bytes(n))) => Some(*n).filter(|n| *n > 0),
        (None, None) => None,
    };

    let start_delay = match f.start_delay.as_deref() {
        None | Some([]) => None,
        Some([s]) => Some((*s, *s)),
        Some([a, b]) if a <= b => Some((*a, *b)),
        Some(_) => bail!("start_delay must be [min, max] with min <= max"),
    }
    .filter(|(_, max)| *max > 0);

    for (k, v) in &f.headers {
        reqwest::header::HeaderName::from_bytes(k.as_bytes())
            .map_err(|_| anyhow!("[headers]: invalid header name \"{k}\""))?;
        reqwest::header::HeaderValue::from_str(v)
            .map_err(|_| anyhow!("[headers]: invalid value for \"{k}\""))?;
    }

    Ok(Config {
        max_speed,
        start_delay,
        reuse_session: f.reuse_session.unwrap_or(true),
        posters: f.posters.unwrap_or(true),
        identity: check_identity(f.identity)?,
        headers: f.headers.into_iter().collect(),
        server_url,
        download_dir,
        max_parallel,
        proxy,
        no_proxy: f.no_proxy,
        accept_invalid_certs: f.accept_invalid_certs,
        user_agent: f
            .user_agent
            .map(|u| u.trim().to_string())
            .filter(|u| !u.is_empty())
            .unwrap_or_else(|| DEFAULT_USER_AGENT.to_string()),
        auth,
    })
}

/// Parse a speed such as "500K", "2MB", "1.5 MiB/s" or "100000" into bytes per second.
/// "0" or "" means no limit. Units are powers of 1024.
pub fn parse_speed(s: &str) -> Result<Option<u64>> {
    let t = s.trim().to_ascii_lowercase();
    let t = t.trim_end_matches("/s").trim();
    if t.is_empty() {
        return Ok(None);
    }
    let split = t
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(t.len());
    let (num, unit) = t.split_at(split);
    let num: f64 = num.parse().map_err(|_| anyhow!("invalid speed \"{s}\""))?;
    let mult = match unit.trim() {
        "" | "b" => 1.0,
        "k" | "kb" | "kib" => 1024.0,
        "m" | "mb" | "mib" => 1024.0 * 1024.0,
        "g" | "gb" | "gib" => 1024.0 * 1024.0 * 1024.0,
        _ => bail!("invalid speed unit in \"{s}\" (use K, M or G)"),
    };
    let v = (num * mult) as u64;
    Ok(if v == 0 { None } else { Some(v) })
}

/// Empty values mean "use the default". Other values must be printable ASCII without '"',
/// because they go inside a quoted HTTP header field.
fn check_identity(mut id: IdentityConfig) -> Result<IdentityConfig> {
    for (name, v) in [
        ("client", &mut id.client),
        ("device", &mut id.device),
        ("version", &mut id.version),
        ("device_id", &mut id.device_id),
    ] {
        *v = v
            .take()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        if let Some(s) = v
            && !s
                .chars()
                .all(|c| c.is_ascii_graphic() && c != '"' || c == ' ')
        {
            bail!("[identity] {name}: use printable ASCII characters only, without '\"'");
        }
    }
    Ok(id)
}

fn run_password_cmd(cmd: &str) -> Result<String> {
    let out = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .output()
        .with_context(|| format!("cannot run password_cmd `{cmd}`"))?;
    if !out.status.success() {
        bail!("password_cmd `{cmd}` failed with {}", out.status);
    }
    let s = String::from_utf8(out.stdout).context("password_cmd output is not UTF-8")?;
    Ok(s.lines().next().unwrap_or_default().to_string())
}

/// Proxy URL without the password, for display.
pub fn redact_proxy(p: &str) -> String {
    match (p.find("://"), p.rfind('@')) {
        (Some(s), Some(at)) if at > s => format!("{}{}", &p[..s + 3], &p[at + 1..]),
        _ => p.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = r#"
server_url = "https://jf.example.org/"
download_dir = "/data/dl"
max_parallel = 4
proxy = "socks5h://u:p@127.0.0.1:1080"
[auth]
api_key = "abc"
"#;

    #[test]
    fn parses_full_config() {
        let c = parse(BASE, &Overrides::default()).unwrap();
        assert_eq!(c.server_url, "https://jf.example.org");
        assert_eq!(c.download_dir, PathBuf::from("/data/dl"));
        assert_eq!(c.max_parallel, 4);
        assert_eq!(c.proxy.as_deref(), Some("socks5h://u:p@127.0.0.1:1080"));
        assert_eq!(c.auth, Auth::ApiKey("abc".into()));
    }

    #[test]
    fn user_agent_default_and_custom() {
        let c = parse(BASE, &Overrides::default()).unwrap();
        assert_eq!(c.user_agent, DEFAULT_USER_AGENT);
        assert!(c.user_agent.starts_with("jellydl/"));
        let t = format!("user_agent = \"Mozilla/5.0 Test\"\n{BASE}");
        assert_eq!(
            parse(&t, &Overrides::default()).unwrap().user_agent,
            "Mozilla/5.0 Test"
        );
        let t = format!("user_agent = \"  \"\n{BASE}");
        assert_eq!(
            parse(&t, &Overrides::default()).unwrap().user_agent,
            DEFAULT_USER_AGENT
        );
    }

    #[test]
    fn speeds() {
        assert_eq!(parse_speed("100000").unwrap(), Some(100_000));
        assert_eq!(parse_speed("500K").unwrap(), Some(512_000));
        assert_eq!(parse_speed("2MB").unwrap(), Some(2 * 1024 * 1024));
        assert_eq!(parse_speed("1.5 MiB/s").unwrap(), Some(1_572_864));
        assert_eq!(parse_speed("0").unwrap(), None);
        assert_eq!(parse_speed("").unwrap(), None);
        assert!(parse_speed("2TB").is_err());
        assert!(parse_speed("fast").is_err());
    }

    #[test]
    fn identity_overrides() {
        let d = parse(BASE, &Overrides::default()).unwrap();
        assert_eq!(d.identity, IdentityConfig::default());

        let t =
            format!("{BASE}[identity]\ndevice = \"tower\"\ndevice_id = \" abc \"\nclient = \"\"\n");
        let c = parse(&t, &Overrides::default()).unwrap();
        assert_eq!(c.identity.device.as_deref(), Some("tower"));
        assert_eq!(c.identity.device_id.as_deref(), Some("abc"));
        assert_eq!(c.identity.client, None, "empty means default");

        let err = |extra: &str| {
            parse(
                &format!("{BASE}[identity]\n{extra}\n"),
                &Overrides::default(),
            )
            .is_err()
        };
        assert!(err("device = \"a\\\"b\""));
        assert!(err("device = \"é\""));
        assert!(err("preset = \"web\""), "unknown keys are rejected");
    }

    #[test]
    fn download_and_header_options() {
        let t = format!(
            "max_speed = 2048\nstart_delay = [3, 9]\nreuse_session = false\n{}\n[headers]\n\"Accept-Language\" = \"fr-FR\"\n",
            BASE.replace("[auth]\napi_key = \"abc\"\n", "")
        ) + "[auth]\napi_key = \"abc\"\n";
        let c = parse(&t, &Overrides::default()).unwrap();
        assert_eq!(c.max_speed, Some(2048));
        assert_eq!(c.start_delay, Some((3, 9)));
        assert!(!c.reuse_session);
        assert_eq!(c.headers, vec![("Accept-Language".into(), "fr-FR".into())]);

        let ov = Overrides {
            max_speed: Some("1M".into()),
            ..Default::default()
        };
        assert_eq!(parse(&t, &ov).unwrap().max_speed, Some(1024 * 1024));

        let d = parse(BASE, &Overrides::default()).unwrap();
        assert!(d.reuse_session);
        assert_eq!((d.max_speed, d.start_delay), (None, None));

        // Top-level keys go before BASE; tables go after it.
        let err = |t: String| parse(&t, &Overrides::default()).unwrap_err().to_string();
        assert!(err(format!("start_delay = [9, 3]\n{BASE}")).contains("start_delay"));
        assert!(err(format!("{BASE}[headers]\n\"Bad Header\" = \"x\"\n")).contains("header name"));
    }

    #[test]
    fn overrides_win() {
        let ov = Overrides {
            proxy: Some("http://proxy:3128".into()),
            download_dir: Some("/tmp/x".into()),
            max_parallel: Some(1),
            max_speed: None,
        };
        let c = parse(BASE, &ov).unwrap();
        assert_eq!(c.proxy.as_deref(), Some("http://proxy:3128"));
        assert_eq!(c.download_dir, PathBuf::from("/tmp/x"));
        assert_eq!(c.max_parallel, 1);
    }

    #[test]
    fn password_auth_and_cmd() {
        let t = "server_url = \"http://a\"\n[auth]\nusername = \"bob\"\npassword_cmd = \"echo s3cret\"\n";
        let c = parse(t, &Overrides::default()).unwrap();
        assert_eq!(
            c.auth,
            Auth::Password {
                username: "bob".into(),
                password: "s3cret".into()
            }
        );
    }

    #[test]
    fn rejects_bad_values() {
        let ov = Overrides::default();
        assert!(parse("server_url = \"ftp://a\"\n[auth]\napi_key=\"k\"", &ov).is_err());
        assert!(parse("server_url = \"http://a\"\n[auth]\n", &ov).is_err());
        assert!(
            parse(
                "server_url = \"http://a\"\nproxy=\"ftp://p\"\n[auth]\napi_key=\"k\"",
                &ov
            )
            .is_err()
        );
        assert!(
            parse(
                "server_url = \"http://a\"\nmax_parallel=0\n[auth]\napi_key=\"k\"",
                &ov
            )
            .is_err()
        );
        assert!(
            parse(
                "server_url = \"http://a\"\n[auth]\napi_key=\"k\"\nusername=\"u\"",
                &ov
            )
            .is_err()
        );
    }

    #[test]
    fn sample_file_is_valid() {
        let c = parse(
            include_str!("../config.example.toml"),
            &Overrides::default(),
        )
        .unwrap();
        assert_eq!(c.server_url, "https://jellyfin.example.org");
        assert!(c.reuse_session);
    }

    #[test]
    fn example_config_is_valid() {
        parse(EXAMPLE_CONFIG, &Overrides::default()).unwrap();
    }

    #[test]
    fn redacts_proxy_credentials() {
        assert_eq!(redact_proxy("socks5h://u:p@h:1080"), "socks5h://h:1080");
        assert_eq!(redact_proxy("http://h:3128"), "http://h:3128");
    }
}
