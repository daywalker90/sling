extern crate serde_json;

use std::path::Path;

use anyhow::anyhow;
use cln_plugin::{options, Builder};
use cln_rpc::primitives::PublicKey;
use cln_rpc::primitives::ShortChannelId;
use log::{debug, info, warn};
use sling::htlc::htlc_handler;
use sling::util::slingjobsettings;
use sling::util::slingversion;
use sling::{
    check_lightning_dir,
    config::*,
    get_info,
    jobs::{slinggo, slingstop},
    model::{Config, PluginState},
    stats::slingstats,
    tasks,
    util::{
        make_rpc_path, read_excepts, refresh_joblists, slingdeletejob, slingexceptchan,
        slingexceptpeer, slingjob,
    },
    EXCEPTS_CHANS_FILE_NAME, EXCEPTS_PEERS_FILE_NAME, PLUGIN_NAME,
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
        .hook("htlc_accepted", htlc_handler)
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
            &defaultconfig.reset_liquidity_interval.0,
            options::Value::OptInteger,
            &format!(
                "Refresh interval for liquidity reset task. Default is {}",
                defaultconfig.reset_liquidity_interval.1
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
        .option(options::ConfigOption::new(
            &defaultconfig.lightning_cli.0,
            options::Value::OptString,
            &format!(
                "Path to lightning-cli for unsupported rpc methods. Default is {}",
                defaultconfig.lightning_cli.1
            ),
        ))
        .option(options::ConfigOption::new(
            &defaultconfig.stats_delete_failures_age.0,
            options::Value::OptInteger,
            &format!(
                "Max age of failure stats in days. Default is {}",
                defaultconfig.stats_delete_failures_age.1
            ),
        ))
        .option(options::ConfigOption::new(
            &defaultconfig.stats_delete_failures_size.0,
            options::Value::OptInteger,
            &format!(
                "Max number of failure stats per channel. Default is {}",
                defaultconfig.stats_delete_failures_size.1
            ),
        ))
        .option(options::ConfigOption::new(
            &defaultconfig.stats_delete_successes_age.0,
            options::Value::OptInteger,
            &format!(
                "Max age of success stats in days. Default is {}",
                defaultconfig.stats_delete_successes_age.1
            ),
        ))
        .option(options::ConfigOption::new(
            &defaultconfig.stats_delete_successes_size.0,
            options::Value::OptInteger,
            &format!(
                "Max number of success stats per channel. Default is {}",
                defaultconfig.stats_delete_successes_size.1
            ),
        ))
        .rpcmethod(
            &(PLUGIN_NAME.to_string() + "-job"),
            "add sling job",
            slingjob,
        )
        .rpcmethod(
            &(PLUGIN_NAME.to_string() + "-jobsettings"),
            "show job settings",
            slingjobsettings,
        )
        .rpcmethod(
            &(PLUGIN_NAME.to_string() + "-deletejob"),
            "delete sling job",
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
            &(PLUGIN_NAME.to_string() + "-except-chan"),
            "channels to avoid for all jobs",
            slingexceptchan,
        )
        .rpcmethod(
            &(PLUGIN_NAME.to_string() + "-except-peer"),
            "peers to avoid for all jobs",
            slingexceptpeer,
        )
        .rpcmethod(
            &(PLUGIN_NAME.to_string() + "-version"),
            "print version",
            slingversion,
        )
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
            match check_lightning_dir(&plugin, state.clone()).await {
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
            match tasks::refresh_listpeerchannels(peersclone).await {
                Ok(()) => (),
                Err(e) => warn!("Error in refresh_listpeers thread: {:?}", e),
            };
        });
        let sling_dir = Path::new(&plugin.configuration().lightning_dir).join(PLUGIN_NAME);
        read_excepts::<ShortChannelId>(
            plugin.state().excepts_chans.clone(),
            EXCEPTS_CHANS_FILE_NAME,
            &sling_dir,
        )
        .await?;
        read_excepts::<PublicKey>(
            plugin.state().excepts_peers.clone(),
            EXCEPTS_PEERS_FILE_NAME,
            &sling_dir,
        )
        .await?;
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
        let clearstatsclone = plugin.clone();
        tokio::spawn(async move {
            match tasks::clear_stats(clearstatsclone).await {
                Ok(()) => (),
                Err(e) => warn!("Error in clear_stats thread: {:?}", e),
            };
        });
        plugin.join().await
    } else {
        Err(anyhow!("Error starting the plugin!"))
    }
}
