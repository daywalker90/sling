use anyhow::Error;
use cln_plugin::Plugin;
use log::debug;

use crate::{model::PluginState, util::write_graph};

pub async fn shutdown_handler(
    plugin: Plugin<PluginState>,
    _v: serde_json::Value,
) -> Result<(), Error> {
    debug!("Got shutdown notification");
    write_graph(plugin.clone()).await?;
    plugin.shutdown()
}
