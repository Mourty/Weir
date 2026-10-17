//! Asking GitHub for Weir's newest release, when the person said yes
//! (`update_check`): once each time the daemon starts, again whenever
//! asked, and nothing more. What it finds is only told; Weir never
//! installs anything.

use crate::controller::Controller;
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;
use weir_protocol::Release;

/// How long the first check waits after the daemon starts, so that at
/// login the network is up first.
const FIRST_WAIT: Duration = Duration::from_secs(20);
/// How long each retry waits after a check that failed at start.
const RETRIES: [Duration; 2] = [Duration::from_secs(120), Duration::from_secs(600)];
/// The longest a check may take.
const TIMEOUT: Duration = Duration::from_secs(20);

/// Check when the daemon starts, if wanted, and whenever asked.
pub async fn run(c: Arc<Controller>) {
    // The wait before the next check by itself, and how many retries are
    // used up; `None` once there is none to do.
    let mut next = (c.settings().update_check == Some(true)).then_some((FIRST_WAIT, 0));
    loop {
        let wait = next.map_or(Duration::from_secs(3600), |(d, _)| d);
        tokio::select! {
            _ = c.update_wanted() => {
                if check(&c).await {
                    next = None;
                }
            }
            _ = tokio::time::sleep(wait), if next.is_some() => {
                let ok = check(&c).await;
                next = match next {
                    Some((_, n)) if !ok && n < RETRIES.len() => Some((RETRIES[n], n + 1)),
                    _ => None,
                };
            }
        }
    }
}

/// One check, start to end. Returns whether GitHub answered.
async fn check(c: &Arc<Controller>) -> bool {
    c.update_checking();
    let url = releases_url();
    let found = tokio::task::spawn_blocking(move || fetch(&url))
        .await
        .unwrap_or_else(|e| Err(e.to_string()));
    c.update_checked(found)
}

/// Where the newest release is listed: Weir's repository on GitHub, or
/// `WEIR_UPDATE_URL`, for testing.
fn releases_url() -> String {
    std::env::var("WEIR_UPDATE_URL").unwrap_or_else(|_| {
        let repo = env!("CARGO_PKG_REPOSITORY").trim_end_matches('/');
        let path = repo.trim_start_matches("https://github.com/");
        format!("https://api.github.com/repos/{path}/releases/latest")
    })
}

/// The fields of GitHub's answer that Weir reads.
#[derive(Deserialize)]
struct GitHubRelease {
    tag_name: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    body: Option<String>,
    html_url: String,
    #[serde(default)]
    published_at: Option<String>,
    #[serde(default)]
    assets: Vec<serde_json::Value>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
}

/// The newest release listed at `url`, or `None` when there is none with
/// its packages up yet.
pub fn fetch(url: &str) -> Result<Option<Release>, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        .user_agent(format!("Weir/{} (update check)", env!("CARGO_PKG_VERSION")))
        // The certificates the system trusts, kept up to date with it.
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .build(),
        )
        .build()
        .into();
    let answer = agent
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .call();
    let mut response = match answer {
        Ok(r) => r,
        // No release at all yet.
        Err(ureq::Error::StatusCode(404)) => return Ok(None),
        Err(ureq::Error::StatusCode(403 | 429)) => {
            return Err("GitHub asked Weir to wait: too many checks came from this \
                        network lately"
                .into())
        }
        Err(e) => return Err(format!("could not reach GitHub ({e})")),
    };
    let body = response
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("GitHub's answer was cut short ({e})"))?;
    parse(&body)
}

/// The release in GitHub's answer `body`, or `None` while its packages
/// are not up: a release is published first and its packages are built
/// and attached after, and until they are there is nothing to install.
fn parse(body: &str) -> Result<Option<Release>, String> {
    let r: GitHubRelease = serde_json::from_str(body)
        .map_err(|e| format!("GitHub's answer could not be read ({e})"))?;
    if r.draft || r.prerelease || r.assets.is_empty() {
        return Ok(None);
    }
    let version = r.tag_name.trim().trim_start_matches('v').to_string();
    Ok(Some(Release {
        name: r
            .name
            .map(|n| n.trim().to_string())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| format!("Weir {version}")),
        version,
        notes: r.body.unwrap_or_default().replace("\r\n", "\n"),
        url: r.html_url,
        published_at: r.published_at.unwrap_or_default(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    const ANSWER: &str = r#"{
        "tag_name": "v1.3.0", "name": "Weir 1.3.0", "draft": false, "prerelease": false,
        "body": "New:\r\n- A recorder", "html_url": "https://github.com/Mourty/Weir/releases/tag/v1.3.0",
        "published_at": "2026-10-20T18:04:11Z", "assets": [{"name": "weir-1.3.0-1.x86_64.rpm"}]
    }"#;

    #[test]
    fn a_release_is_read_once_its_packages_are_up() {
        let r = parse(ANSWER).unwrap().unwrap();
        assert_eq!(r.version, "1.3.0");
        assert_eq!(r.notes, "New:\n- A recorder");
        assert_eq!(r.url, "https://github.com/Mourty/Weir/releases/tag/v1.3.0");
        let bare = ANSWER.replace(r#"[{"name": "weir-1.3.0-1.x86_64.rpm"}]"#, "[]");
        assert_eq!(parse(&bare).unwrap(), None);
        assert!(parse("<html>").unwrap_err().contains("could not be read"));
    }

    /// Serve one HTTP answer with `status` and `body` on a port of its own.
    fn serve(status: &'static str, body: &'static str) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/releases/latest", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 4096];
            let _ = stream.read(&mut request);
            let answer = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(answer.as_bytes());
        });
        url
    }

    #[test]
    fn github_is_asked_and_its_answers_understood() {
        let found = fetch(&serve("200 OK", ANSWER)).unwrap().unwrap();
        assert_eq!(found.version, "1.3.0");
        assert_eq!(fetch(&serve("404 Not Found", "{}")).unwrap(), None);
        let limited = fetch(&serve("403 Forbidden", "{}")).unwrap_err();
        assert!(limited.contains("wait"), "{limited}");
        let nobody = fetch("http://127.0.0.1:9/releases/latest").unwrap_err();
        assert!(nobody.starts_with("could not reach GitHub"), "{nobody}");
    }
}
