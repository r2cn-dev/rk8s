mod api;
mod cli;
mod commands;
mod network;
mod protocol;
mod server;

use crate::api::xlinestore::XlineStore;
use crate::network::init;
use crate::protocol::config::load_config;
use anyhow::Context;
use clap::Parser;
use cli::{Cli, Commands};
use log::error;
use server::serve;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::init();

    let cli = Cli::parse();
    use log::info;

    env_logger::init();

    info!("server started");

    match &cli.command {
        Commands::Start { config } => {
            let cfg = load_config(config.to_str().unwrap())?;
            let xline_config = cfg.xline_config;
            let endpoints: Vec<&str> = xline_config.endpoints.iter().map(|s| s.as_str()).collect();
            let xline_store = Arc::new(XlineStore::new(&endpoints).await?);
            println!("[rks] listening on {}", cfg.addr);
            let sm = match init::new_subnet_manager(xline_config.clone()).await {
                Ok(m) => m,
                Err(e) => {
                    error!("Failed to create subnet manager: {e:?}");
                    return Err(e).context("new_subnet_manager failed");
                }
            };
            let local_manager = Arc::new(sm.clone());
            serve(cfg.addr, xline_store, local_manager).await?;
        }
    }

    Ok(())
}
