use crate::commands::login::config::LoginConfig;
use crate::commands::login::oauth::OAuthFlow;
use crate::commands::login::types::{CallbackResponse, RequestClientIdResponse};
use crate::commands::{RequestBuilderExt, assert_not_sudo};
use crate::rt::block_on;
use axum::http::HeaderMap;
use clap::Parser;
use reqwest::Client;
use std::sync::OnceLock;

pub mod config;

mod oauth;

mod types;

static CLIENT: OnceLock<Client> = OnceLock::new();

fn client_ref() -> &'static Client {
    CLIENT.get_or_init(|| {
        let mut headers = HeaderMap::new();
        headers.insert("Accept", "application/json".parse().unwrap());

        Client::builder()
            .default_headers(headers)
            .build()
            .expect("Failed to build client")
    })
}

#[derive(Debug, Parser)]
pub struct LoginArgs {
    /// URL of the distribution server (optional if only one server is configured)
    url: Option<String>,
    /// Github OAuth app client id (required for first login to this server)
    client_id: Option<String>,
}

pub fn login(args: LoginArgs) -> anyhow::Result<()> {
    assert_not_sudo("login")?;
    let config = LoginConfig::load()?;

    let url = match args.url {
        Some(ref url) => url,
        None => &config.single_entry()?.url,
    };

    block_on(async move {
        let res = request_client_id(url).await?;
        let client_id = &res.client_id;

        let oauth = OAuthFlow::new(client_id);
        let res = oauth.request_token().await?;

        let req_url = format!("http://{url}/api/v1/auth/github/callback");
        let res = client_ref()
            .post(req_url)
            .json(&res)
            .send_and_json::<CallbackResponse>()
            .await?;

        LoginConfig::login(res.pat, url)?;
        println!("Logged in successfully!");
        Ok(())
    })?
}

async fn request_client_id(url: impl AsRef<str>) -> anyhow::Result<RequestClientIdResponse> {
    let url = format!("http://{}/api/v1/auth/github/client_id", url.as_ref());
    client_ref().get(url).send_and_json().await
}
