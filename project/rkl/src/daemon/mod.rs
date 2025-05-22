pub mod sync_loop;
use sync_loop::SyncLoop;
pub mod static_pods;
mod status_access;

#[tokio::main]
pub async fn main() -> Result<(), anyhow::Error> {
    tokio::spawn(status_access::init());
    let sync_loop = SyncLoop::default().register_event(static_pods::handler);
    sync_loop.run().await;
    Ok(())
}
