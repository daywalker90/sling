use anyhow::anyhow;
use cln_plugin::options::{
    BooleanConfigOption, ConfigOption, IntegerConfigOption, StringConfigOption,
};
use cln_plugin::Builder;
use cln_rpc::primitives::PublicKey;
use cln_rpc::primitives::ShortChannelId;
use config::*;
use htlc::block_added;
use htlc::htlc_handler;
use jobs::*;
use log::{debug, info, warn};
use model::*;
use notifications::*;
use rpc::*;
use stats::*;
use std::path::Path;
use tokio::{self};
use util::*;

mod config;
mod dijkstra;
mod errors;
mod htlc;
mod jobs;
mod model;
mod notifications;
mod rpc;
mod slings;
mod stats;
mod tasks;
mod util;

#[cfg(test)]
mod tests;

const OPT_UTF8: BooleanConfigOption = ConfigOption::new_bool_no_default(
    "sling-utf8",
    "Switch on/off special characters in node alias. Default is `true`",
);
const OPT_REFRESH_PEERS_INTERVAL: IntegerConfigOption = ConfigOption::new_i64_no_default(
    "sling-refresh-peers-interval",
    "Refresh interval for listpeers task. Default is `1`",
);
const OPT_REFRESH_ALIASMAP_INTERVAL: IntegerConfigOption = ConfigOption::new_i64_no_default(
    "sling-refresh-aliasmap-interval",
    "Refresh interval for aliasmap task. Default is `3600`",
);
const OPT_REFRESH_GRAPH_INTERVAL: IntegerConfigOption = ConfigOption::new_i64_no_default(
    "sling-refresh-graph-interval",
    "Refresh interval for graph task. Default is `600`",
);
const OPT_RESET_LIQUIDITY_INTERVAL: IntegerConfigOption = ConfigOption::new_i64_no_default(
    "sling-reset-liquidity-interval",
    "Refresh interval for liquidity reset task. Default is `360`",
);
const OPT_DEPLETEUPTOPERCENT: StringConfigOption = ConfigOption::new_str_no_default(
    "sling-depleteuptopercent",
    "Deplete up to percent for candidate search. Default is `0.2`",
);
const OPT_DEPLETEUPTOAMOUNT: IntegerConfigOption = ConfigOption::new_i64_no_default(
    "sling-depleteuptoamount",
    "Deplete up to amount for candidate search. Default is `2000000000`",
);
const OPT_MAXHOPS: IntegerConfigOption = ConfigOption::new_i64_no_default(
    "sling-maxhops",
    "Maximum number of hops in a route. Default is `8`",
);
const OPT_CANDIDATES_MIN_AGE: IntegerConfigOption = ConfigOption::new_i64_no_default(
    "sling-candidates-min-age",
    "Minium age of a candidate to rebalance with in days. Default is `0`",
);
const OPT_PARALLELJOBS: IntegerConfigOption = ConfigOption::new_i64_no_default(
    "sling-paralleljobs",
    "Number of parallel tasks for a job. Default is `1`",
);
const OPT_TIMEOUTPAY: IntegerConfigOption = ConfigOption::new_i64_no_default(
    "sling-timeoutpay",
    "Timeout for rebalances until we give up and continue. Default is `120`",
);
const OPT_MAX_HTLC_COUNT: IntegerConfigOption = ConfigOption::new_i64_no_default(
    "sling-max-htlc-count",
    "Max number of htlc allowed pending in job and candidate. Default is `5`",
);
const OPT_LIGHTNING_CONF: StringConfigOption = ConfigOption::new_str_no_default(
    "sling-lightning-conf",
    "Path to lightning_conf for unsupported rpc methods. Default is ``",
);
const OPT_STATS_DELETE_FAILURES_AGE: IntegerConfigOption = ConfigOption::new_i64_no_default(
    "sling-stats-delete-failures-age",
    "Max age of failure stats in days. Default is `30`",
);
const OPT_STATS_DELETE_FAILURES_SIZE: IntegerConfigOption = ConfigOption::new_i64_no_default(
    "sling-stats-delete-failures-size",
    "Max number of failure stats per channel. Default is `10000`",
);
const OPT_STATS_DELETE_SUCCESSES_AGE: IntegerConfigOption = ConfigOption::new_i64_no_default(
    "sling-stats-delete-successes-age",
    "Max age of success stats in days. Default is `30`",
);
const OPT_STATS_DELETE_SUCCESSES_SIZE: IntegerConfigOption = ConfigOption::new_i64_no_default(
    "sling-stats-delete-successes-size",
    "Max number of success stats per channel. Default is `10000`",
);

#[cfg(all(not(windows), not(target_env = "musl")))]
#[global_allocator]
static ALLOC: jemallocator::Jemalloc = jemallocator::Jemalloc;

#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
    std::env::set_var("CLN_PLUGIN_LOG", "cln_plugin=info,cln_rpc=info,debug");
    log_panics::init();
    let state = PluginState::new();
    let confplugin;
    match Builder::new(tokio::io::stdin(), tokio::io::stdout())
        .hook("htlc_accepted", htlc_handler)
        .subscribe("block_added", block_added)
        .option(OPT_UTF8)
        .option(OPT_REFRESH_PEERS_INTERVAL)
        .option(OPT_REFRESH_ALIASMAP_INTERVAL)
        .option(OPT_REFRESH_GRAPH_INTERVAL)
        .option(OPT_RESET_LIQUIDITY_INTERVAL)
        .option(OPT_DEPLETEUPTOPERCENT)
        .option(OPT_DEPLETEUPTOAMOUNT)
        .option(OPT_MAXHOPS)
        .option(OPT_CANDIDATES_MIN_AGE)
        .option(OPT_PARALLELJOBS)
        .option(OPT_TIMEOUTPAY)
        .option(OPT_MAX_HTLC_COUNT)
        .option(OPT_LIGHTNING_CONF)
        .option(OPT_STATS_DELETE_FAILURES_AGE)
        .option(OPT_STATS_DELETE_FAILURES_SIZE)
        .option(OPT_STATS_DELETE_SUCCESSES_AGE)
        .option(OPT_STATS_DELETE_SUCCESSES_SIZE)
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
        .subscribe("shutdown", shutdown_handler)
        .dynamic()
        .configure()
        .await?
    {
        Some(plugin) => {
            info!("get prestart config");
            match get_prestart_configs(&plugin, state.clone()) {
                Ok(()) => &(),
                Err(e) => return plugin.disable(format!("{}", e).as_str()).await,
            };
            info!("read config");
            match read_config(&plugin, state.clone()).await {
                Ok(()) => &(),
                Err(e) => return plugin.disable(format!("{}", e).as_str()).await,
            };
            info!("read startup options");
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
        let getinfo = get_info(&make_rpc_path(&plugin)).await?;
        {
            plugin.state().config.lock().pubkey = Some(getinfo.id);
            *plugin.state().blockheight.lock() = getinfo.blockheight;
        }
        let peersclone = plugin.clone();
        tokio::spawn(async move {
            match tasks::refresh_listpeerchannels_loop(peersclone).await {
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

        plugin.join().await?;
        std::process::exit(0);
    } else {
        Err(anyhow!("Error starting the plugin!"))
    }
}
