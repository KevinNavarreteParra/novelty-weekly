//! `novelty auth`: log in to Spotify and save the refresh token.

use anyhow::Result;

use super::App;
use crate::config::secret;
use crate::integrations::http::{Http, USER_AGENT};
use crate::integrations::spotify::auth;

pub fn run(app: &App, paste: bool) -> Result<()> {
    let client_id = secret("SPOTIFY_CLIENT_ID")?;
    app.paths.ensure_home()?;
    let http = Http::new("spotify", USER_AGENT, 1.0);
    let token = auth::login(&http, &client_id, &app.config.spotify.redirect_uri, paste)?;
    let path = app.paths.spotify_token();
    token.save(&path)?;
    println!(
        "Logged in; token saved to {}. It lasts until {} (Spotify Development Mode); \
         run `novelty auth` again before then.",
        path.display(),
        token.expires_at().strftime("%Y-%m-%d"),
    );
    Ok(())
}
