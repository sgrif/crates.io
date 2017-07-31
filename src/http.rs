use oauth2::*;
use reqwest::{self, StatusCode};
use reqwest::header::{qitem, Accept, Authorization, Bearer, UserAgent};
use serde::de::DeserializeOwned;

use app::App;
use std::str;
use util::{CargoResult, human};


/// Does all the nonsense for sending a GET to Github. Doesn't handle parsing
/// because custom error-code handling may be desirable. Use
/// `parse_github_response` to handle the "common" processing of responses.
pub fn github(app: &App, url: &str, auth: &Token) -> reqwest::Result<reqwest::Response> {
    let url = format!("{}://api.github.com{}", app.config.api_protocol, url);
    info!("GITHUB HTTP: {}", url);

    let client = app.reqwest_client()?;
    client.get(&url)?
        .header(Accept(vec![
            qitem("application/vnd.github.v3+json".parse().unwrap())
        ]))
        .header(UserAgent::new("hello!"))
        .header(Authorization(Bearer { token: auth.access_token.clone() }))
        .send()
}

/// Checks for normal responses
pub fn parse_github_response<T: DeserializeOwned>(
    resp: reqwest::Response,
) -> CargoResult<T> {
    if let StatusCode::Forbidden = resp.status() {
        return Err(human(
            "It looks like you don't have permission \
             to query a necessary property from Github \
             to complete this request. \
             You may need to re-authenticate on \
             crates.io to grant permission to read \
             github org memberships. Just go to \
             https://crates.io/login",
        ));
    }
    resp.error_for_status()?
        .json()
        .map_err(Into::into)
}

/// Gets a token with the given string as the access token, but all
/// other info null'd out. Generally, just to be fed to the `github` fn.
pub fn token(token: String) -> Token {
    Token {
        access_token: token,
        scopes: Vec::new(),
        token_type: String::new(),
    }
}
