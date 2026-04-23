use std::{env, error::Error, net::SocketAddr, sync::Arc};

use philharmonic_connector_router::{DispatchConfig, HyperForwarder, RouterState, router};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let listen_addr = env::var("PHILHARMONIC_ROUTER_LISTEN")
        .unwrap_or_else(|_| "127.0.0.1:3000".to_owned())
        .parse::<SocketAddr>()?;

    let domain = env::var("PHILHARMONIC_ROUTER_DOMAIN")?;
    let realm = env::var("PHILHARMONIC_ROUTER_REALM")?;
    let upstreams_raw = env::var("PHILHARMONIC_ROUTER_UPSTREAMS")?;

    let upstreams = upstreams_raw
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::parse)
        .collect::<Result<Vec<_>, _>>()?;

    let mut config = DispatchConfig::new(domain)?;
    config.insert_realm(realm, upstreams)?;

    let state = RouterState::new(config, Arc::new(HyperForwarder::new()));
    let listener = tokio::net::TcpListener::bind(listen_addr).await?;

    axum::serve(listener, router(state)).await?;
    Ok(())
}
