extern crate serde_json;

use anyhow::anyhow;
use cln_plugin::{options, Builder};
use log::{debug, info, warn};
use sling::{
    config::*,
    get_info,
    htlc::htlc_handler,
    jobs::{slinggo, slingstop},
    model::{Config, PluginState},
    stats::slingstats,
    tasks,
    util::{make_rpc_path, read_excepts, refresh_joblists, slingdeletejob, slingexcept, slingjob},
    PLUGIN_NAME,
};
use tokio::{self};
#[cfg(all(not(windows), not(target_env = "musl")))]
#[global_allocator]
static ALLOC: jemallocator::Jemalloc = jemallocator::Jemalloc;

#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
    std::env::set_var("CLN_PLUGIN_LOG", "trace");
    let state = PluginState::new();
    let defaultconfig = Config::new();
    let confplugin;
    match Builder::new(tokio::io::stdin(), tokio::io::stdout())
        .option(options::ConfigOption::new(
            &defaultconfig.utf8.0,
            options::Value::OptBoolean,
            &format!(
                "Switch on/off special characters in node alias. Default is {}",
                defaultconfig.utf8.1
            ),
        ))
        .option(options::ConfigOption::new(
            &defaultconfig.refresh_peers_interval.0,
            options::Value::OptInteger,
            &format!(
                "Refresh interval for listpeers task. Default is {}",
                defaultconfig.refresh_peers_interval.1
            ),
        ))
        .option(options::ConfigOption::new(
            &defaultconfig.refresh_aliasmap_interval.0,
            options::Value::OptInteger,
            &format!(
                "Refresh interval for aliasmap task. Default is {}",
                defaultconfig.refresh_aliasmap_interval.1
            ),
        ))
        .option(options::ConfigOption::new(
            &defaultconfig.refresh_graph_interval.0,
            options::Value::OptInteger,
            &format!(
                "Refresh interval for graph task. Default is {}",
                defaultconfig.refresh_graph_interval.1
            ),
        ))
        .option(options::ConfigOption::new(
            &defaultconfig.refresh_liquidity_interval.0,
            options::Value::OptInteger,
            &format!(
                "Refresh interval for liquidity refresh task. Default is {}",
                defaultconfig.refresh_liquidity_interval.1
            ),
        ))
        .option(options::ConfigOption::new(
            &defaultconfig.depleteuptopercent.0,
            options::Value::OptString,
            &format!(
                "Deplete up to percent for candidate search. Default is {}",
                defaultconfig.depleteuptopercent.1
            ),
        ))
        .option(options::ConfigOption::new(
            &defaultconfig.depleteuptoamount.0,
            options::Value::OptInteger,
            &format!(
                "Deplete up to amount for candidate search. Default is {}",
                defaultconfig.depleteuptoamount.1
            ),
        ))
        .option(options::ConfigOption::new(
            &defaultconfig.max_htlc_count.0,
            options::Value::OptInteger,
            &format!(
                "Max number of htlc allowed pending in job and candidate. Default is {}",
                defaultconfig.max_htlc_count.1
            ),
        ))
        .rpcmethod(
            &(PLUGIN_NAME.to_string() + "-job"),
            "add sling job",
            slingjob,
        )
        .rpcmethod(
            &(PLUGIN_NAME.to_string() + "-deletejob"),
            "add sling job",
            slingdeletejob,
        )
        .rpcmethod(
            &(PLUGIN_NAME.to_string() + "-go"),
            "start sling jobs",
            slinggo,
        )
        .rpcmethod(
            &(PLUGIN_NAME.to_string() + "-stop"),
            "stop sling jobs",
            slingstop,
        )
        .rpcmethod(
            &(PLUGIN_NAME.to_string() + "-stats"),
            "show stats on channel(s)",
            slingstats,
        )
        .rpcmethod(
            &(PLUGIN_NAME.to_string() + "-except"),
            "channels to avoid for all jobs",
            slingexcept,
        )
        .hook("htlc_accepted", htlc_handler)
        .dynamic()
        .configure()
        .await?
    {
        Some(plugin) => {
            info!("read config");
            match read_config(&plugin, state.clone()).await {
                Ok(()) => &(),
                Err(e) => return plugin.disable(format!("{}", e).as_str()).await,
            };
            info!("startup options");
            match get_startup_options(&plugin, state.clone()) {
                Ok(()) => &(),
                Err(e) => return plugin.disable(format!("{}", e).as_str()).await,
            };

            confplugin = plugin;
        }
        None => return Err(anyhow!("Error configuring the plugin!")),
    };
    if let Ok(plugin) = confplugin.start(state).await {
        debug!("{:?}", plugin.configuration());
        let mypubkey = get_info(&make_rpc_path(&plugin)).await?.id;
        {
            plugin.state().config.lock().pubkey = Some(mypubkey);
        }
        let peersclone = plugin.clone();
        tokio::spawn(async move {
            match tasks::refresh_listpeers(peersclone).await {
                Ok(()) => (),
                Err(e) => warn!("Error in refresh_listpeers thread: {:?}", e),
            };
        });
        let except_clone = plugin.clone();
        read_excepts(except_clone).await?;
        let joblists_clone = plugin.clone();
        refresh_joblists(joblists_clone).await?;
        let channelsclone = plugin.clone();
        tokio::spawn(async move {
            match tasks::refresh_graph(channelsclone).await {
                Ok(()) => (),
                Err(e) => warn!("Error in refresh_graph thread: {:?}", e),
            };
        });
        let aliasclone = plugin.clone();
        tokio::spawn(async move {
            match tasks::refresh_aliasmap(aliasclone).await {
                Ok(()) => (),
                Err(e) => warn!("Error in refresh_aliasmap thread: {:?}", e),
            };
        });
        let liquidityclone = plugin.clone();
        tokio::spawn(async move {
            match tasks::refresh_liquidity(liquidityclone).await {
                Ok(()) => (),
                Err(e) => warn!("Error in refresh_liquidity thread: {:?}", e),
            };
        });
        let tempbanclone = plugin.clone();
        tokio::spawn(async move {
            match tasks::clear_tempbans(tempbanclone).await {
                Ok(()) => (),
                Err(e) => warn!("Error in clear_tempbans thread: {:?}", e),
            };
        });
        plugin.join().await
    } else {
        Err(anyhow!("Error starting the plugin!"))
    }
}
