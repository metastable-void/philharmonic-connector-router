//! Per-realm HTTP dispatcher for connector-service upstreams.

mod config;
mod dispatch;

pub use config::{DispatchConfig, DispatchConfigError};
pub use dispatch::{
    ForwardError, ForwardFuture, Forwarder, HyperForwarder, RouterState, dispatch_by_host,
    dispatch_by_path, router,
};
