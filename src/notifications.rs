use anyhow::Error;
use cln_plugin::Plugin;

use crate::{model::PluginState, util::write_liquidity};

pub async fn shutdown_handler(
    plugin: Plugin<PluginState>,
    _v: serde_json::Value,
) -> Result<(), Error> {
    log::debug!("Got shutdown notification");
    write_liquidity(plugin.clone()).await?;
    plugin.shutdown()
}
