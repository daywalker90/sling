use anyhow::Error;
use cln_plugin::Plugin;

use crate::{model::PluginState, write_graph};

pub async fn shutdown_handler(
    plugin: Plugin<PluginState>,
    _v: serde_json::Value,
) -> Result<(), Error> {
    log::debug!("Got shutdown notification");
    write_graph(plugin.clone()).await?;
    plugin.shutdown()
}
