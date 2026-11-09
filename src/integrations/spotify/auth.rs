//! Spotify login: the authorization-code flow with PKCE, so no client secret
//! is needed, and refreshing the access token from the stored refresh token.
//!
//! In Development Mode a refresh token dies six months after the original
//! login, however often it is used; [`Token::authorized_at`] keeps that date
//! so the app can warn ahead of time.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use jiff::{SignedDuration, Timestamp};
use rand::RngExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::integrations::http::Http;

const AUTHORIZE_URL: &str = "https://accounts.spotify.com/authorize";
const TOKEN_URL: &str = "https://accounts.spotify.com/api/token";

/// Read listens, top items and the library; write the playlist.
const SCOPES: &str = "user-read-recently-played user-top-read user-library-read \
                      playlist-read-private playlist-modify-private playlist-modify-public";

/// How long a refresh token lives in Development Mode.
pub const REFRESH_TOKEN_LIFETIME: SignedDuration = SignedDuration::from_hours(183 * 24);

/// What `spotify-token.json` holds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Token {
    pub refresh_token: String,
    /// When the original login happened; the refresh token expires six months later.
    pub authorized_at: Timestamp,
}

impl Token {
    pub fn load(path: &Path) -> Result<Token> {
        let text = fs::read_to_string(path)
            .with_context(|| format!("reading {} (run `novelty auth` first)", path.display()))?;
        serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    /// Writes the token readable by its owner only, replacing any previous one
    /// in a single rename so a crash can't leave half a file.
    pub fn save(&self, path: &Path) -> Result<()> {
        use std::os::unix::fs::OpenOptionsExt;
        let tmp = path.with_extension("json.tmp");
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)
            .with_context(|| format!("writing {}", tmp.display()))?;
        file.write_all(serde_json::to_string_pretty(self)?.as_bytes())?;
        file.sync_all()?;
        fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn expires_at(&self) -> Timestamp {
        self.authorized_at
            .checked_add(REFRESH_TOKEN_LIFETIME)
            .unwrap_or(Timestamp::MAX)
    }
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    /// Spotify may rotate the refresh token on refresh; keep the new one if so.
    refresh_token: Option<String>,
}

/// Runs the browser login and returns the new token.
///
/// Prints the authorization URL, then waits for Spotify to redirect the
/// browser to `redirect_uri` on this machine. When the browser is elsewhere
/// (the app on a server, the browser on a laptop), either forward the port
/// (`ssh -L 8765:127.0.0.1:8765 server`) or set `paste` and paste the address
/// the browser ends up on.
pub fn login(http: &Http, client_id: &str, redirect_uri: &str, paste: bool) -> Result<Token> {
    let verifier = code_verifier();
    let state = URL_SAFE_NO_PAD.encode(rand::rng().random::<[u8; 16]>());
    let url = format!(
        "{AUTHORIZE_URL}?response_type=code&client_id={}&redirect_uri={}&scope={}\
         &code_challenge_method=S256&code_challenge={}&state={state}",
        encode(client_id),
        encode(redirect_uri),
        encode(SCOPES),
        code_challenge(&verifier),
    );
    println!("Open this in a browser and approve access:\n\n{url}\n");

    let redirected = if paste {
        println!("Then paste the address the browser lands on:");
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        line
    } else {
        wait_for_redirect(redirect_uri)?
    };
    let code = code_from_redirect(&redirected, &state)?;

    let response = http.post_form(
        TOKEN_URL,
        &[
            ("grant_type", "authorization_code"),
            ("code", &code),
            ("redirect_uri", redirect_uri),
            ("client_id", client_id),
            ("code_verifier", &verifier),
        ],
    )?;
    if !response.is_success() {
        bail!(
            "spotify token exchange failed ({}): {}",
            response.status,
            response.body
        );
    }
    let tokens: TokenResponse = serde_json::from_str(&response.body)?;
    Ok(Token {
        refresh_token: tokens
            .refresh_token
            .ok_or_else(|| anyhow!("spotify returned no refresh token"))?,
        authorized_at: Timestamp::now(),
    })
}

/// Trades the refresh token for a fresh access token. Updates `token` if
/// Spotify rotated the refresh token; the caller saves it.
pub fn refresh(http: &Http, client_id: &str, token: &mut Token) -> Result<String> {
    let response = http.post_form(
        TOKEN_URL,
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", &token.refresh_token),
            ("client_id", client_id),
        ],
    )?;
    if !response.is_success() {
        bail!(
            "spotify token refresh failed ({}): {}. If the token expired, run `novelty auth`.",
            response.status,
            response.body
        );
    }
    let tokens: TokenResponse = serde_json::from_str(&response.body)?;
    if let Some(rotated) = tokens.refresh_token {
        token.refresh_token = rotated;
    }
    Ok(tokens.access_token)
}

/// 64 random bytes as unpadded base64url: 86 characters, inside PKCE's 43-128.
fn code_verifier() -> String {
    URL_SAFE_NO_PAD.encode(rand::rng().random::<[u8; 64]>())
}

fn code_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// Accepts one request on the redirect URI's port and returns its target,
/// e.g. `/callback?code=...&state=...`.
fn wait_for_redirect(redirect_uri: &str) -> Result<String> {
    let authority = redirect_uri
        .strip_prefix("http://")
        .and_then(|rest| rest.split('/').next())
        .ok_or_else(|| anyhow!("redirect_uri must be http://127.0.0.1:<port>/..."))?;
    let listener =
        TcpListener::bind(authority).with_context(|| format!("listening on {authority}"))?;
    println!("Waiting for the redirect on {authority}...");
    let (mut stream, _) = listener.accept()?;
    let mut request_line = String::new();
    BufReader::new(&stream).read_line(&mut request_line)?;
    let body = "Logged in. You can close this tab.";
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    // "GET /callback?code=...&state=... HTTP/1.1"
    request_line
        .split_whitespace()
        .nth(1)
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("unexpected request: {request_line:?}"))
}

/// Pulls `code` out of a redirect URL or path, after checking `state`.
fn code_from_redirect(redirected: &str, expected_state: &str) -> Result<String> {
    let query = redirected
        .trim()
        .split_once('?')
        .map(|(_, q)| q)
        .ok_or_else(|| anyhow!("no query string in {redirected:?}"))?;
    let param = |name: &str| {
        query
            .split('&')
            .filter_map(|pair| pair.split_once('='))
            .find(|(k, _)| *k == name)
            .map(|(_, v)| decode(v))
    };
    if let Some(error) = param("error") {
        bail!("spotify login was refused: {error}");
    }
    if param("state").as_deref() != Some(expected_state) {
        bail!("the redirect's state doesn't match this login; try again");
    }
    param("code").ok_or_else(|| anyhow!("no code in the redirect"))
}

/// Percent-encodes everything but RFC 3986's unreserved characters.
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Undoes percent-encoding (and `+` for space, as form encoding writes it).
fn decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let escaped = (bytes[i] == b'%')
            .then(|| s.get(i + 1..i + 3))
            .flatten()
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        match (escaped, bytes[i]) {
            (Some(byte), _) => {
                out.push(byte);
                i += 3;
                continue;
            }
            (None, b'+') => out.push(b' '),
            (None, b) => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_challenge_matches_rfc_7636s_example() {
        // RFC 7636, appendix B.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            code_challenge(verifier),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn the_verifier_is_a_valid_length() {
        assert_eq!(code_verifier().len(), 86);
    }

    #[test]
    fn the_code_is_read_from_a_pasted_url_or_a_path() {
        let url = "http://127.0.0.1:8765/callback?code=AQB%2Fx&state=s1";
        assert_eq!(code_from_redirect(url, "s1").unwrap(), "AQB/x");
        assert_eq!(
            code_from_redirect("/callback?state=s1&code=abc", "s1").unwrap(),
            "abc"
        );
    }

    #[test]
    fn a_mismatched_state_or_a_refusal_is_an_error() {
        assert!(code_from_redirect("/callback?code=abc&state=other", "s1").is_err());
        assert!(code_from_redirect("/callback?error=access_denied&state=s1", "s1").is_err());
    }

    #[test]
    fn encoding_round_trips() {
        let s = "http://127.0.0.1:8765/callback user-read playlist~x";
        assert_eq!(decode(&encode(s)), s);
        assert_eq!(encode("a b/c"), "a%20b%2Fc");
    }

    #[test]
    fn the_token_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("novelty-token-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("spotify-token.json");
        let token = Token {
            refresh_token: "r".into(),
            authorized_at: Timestamp::UNIX_EPOCH,
        };
        token.save(&path).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        let loaded = Token::load(&path).unwrap();
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(mode, 0o600);
        assert_eq!(loaded.refresh_token, "r");
    }
}
