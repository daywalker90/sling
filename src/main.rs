use cln_rpc::model::requests::GetinfoRequest;
use cln_rpc::ClnRpc;
#[cfg(not(target_env = "msvc"))]
use tikv_jemallocator::Jemalloc;

#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: Jemalloc = Jemalloc;

use anyhow::anyhow;
use cln_plugin::options::{
    ConfigOption, IntegerConfigOption, StringArrayConfigOption, StringConfigOption,
};
use cln_plugin::Builder;
use config::*;
use htlc::block_added;
use htlc::htlc_handler;
use model::*;
use notifications::*;
use rpc_sling::*;
use stats::*;
use std::path::Path;
use tokio::{self};
use util::*;

mod config;
mod dijkstra;
mod errors;
mod gossip;
mod htlc;
mod model;
mod notifications;
mod parse;
mod response;
mod rpc_sling;
mod slings;
mod stats;
mod tasks;
mod util;

#[cfg(test)]
mod tests;

const OPT_REFRESH_PEERS_INTERVAL: &str = "sling-refresh-peers-interval";
const OPT_REFRESH_ALIASMAP_INTERVAL: &str = "sling-refresh-aliasmap-interval";
const OPT_REFRESH_GOSSMAP_INTERVAL: &str = "sling-refresh-gossmap-interval";
const OPT_RESET_LIQUIDITY_INTERVAL: &str = "sling-reset-liquidity-interval";
const OPT_DEPLETEUPTOPERCENT: &str = "sling-depleteuptopercent";
const OPT_DEPLETEUPTOAMOUNT: &str = "sling-depleteuptoamount";
const OPT_MAXHOPS: &str = "sling-maxhops";
const OPT_CANDIDATES_MIN_AGE: &str = "sling-candidates-min-age";
const OPT_PARALLELJOBS: &str = "sling-paralleljobs";
const OPT_TIMEOUTPAY: &str = "sling-timeoutpay";
const OPT_MAX_HTLC_COUNT: &str = "sling-max-htlc-count";
const OPT_STATS_DELETE_FAILURES_AGE: &str = "sling-stats-delete-failures-age";
const OPT_STATS_DELETE_FAILURES_SIZE: &str = "sling-stats-delete-failures-size";
const OPT_STATS_DELETE_SUCCESSES_AGE: &str = "sling-stats-delete-successes-age";
const OPT_STATS_DELETE_SUCCESSES_SIZE: &str = "sling-stats-delete-successes-size";
const OPT_INFORM_LAYERS: &str = "sling-inform-layers";

#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
    std::env::set_var("CLN_PLUGIN_LOG", "cln_plugin=info,cln_rpc=info,debug");
    log_panics::init();
    let state;
    let confplugin;
    let opt_refresh_peers_interval: IntegerConfigOption = ConfigOption::new_i64_no_default(
        OPT_REFRESH_PEERS_INTERVAL,
        "Refresh interval for listpeers task. Default is `1`",
    )
    .dynamic();
    let opt_refresh_aliasmap_interval: IntegerConfigOption = ConfigOption::new_i64_no_default(
        OPT_REFRESH_ALIASMAP_INTERVAL,
        "Refresh interval for aliasmap task. Default is `3600`",
    )
    .dynamic();
    let opt_refresh_gossmap_interval: IntegerConfigOption = ConfigOption::new_i64_no_default(
        OPT_REFRESH_GOSSMAP_INTERVAL,
        "Refresh interval for gossmap task. Default is `10`",
    )
    .dynamic();
    let opt_reset_liquidity_interval: IntegerConfigOption = ConfigOption::new_i64_no_default(
        OPT_RESET_LIQUIDITY_INTERVAL,
        "Refresh interval for liquidity reset task. Default is `60`",
    )
    .dynamic();
    let opt_depleteuptopercent: StringConfigOption = ConfigOption::new_str_no_default(
        OPT_DEPLETEUPTOPERCENT,
        "Deplete up to percent for candidate search. Default is `0.2`",
    )
    .dynamic();
    let opt_depleteuptoamount: IntegerConfigOption = ConfigOption::new_i64_no_default(
        OPT_DEPLETEUPTOAMOUNT,
        "Deplete up to amount for candidate search. Default is `2000000000`",
    )
    .dynamic();
    let opt_maxhops: IntegerConfigOption = ConfigOption::new_i64_no_default(
        OPT_MAXHOPS,
        "Maximum number of hops in a route. Default is `8`",
    )
    .dynamic();
    let opt_candidates_min_age: IntegerConfigOption = ConfigOption::new_i64_no_default(
        OPT_CANDIDATES_MIN_AGE,
        "Minium age of a candidate to rebalance with in days. Default is `0`",
    )
    .dynamic();
    let opt_paralleljobs: IntegerConfigOption = ConfigOption::new_i64_no_default(
        OPT_PARALLELJOBS,
        "Number of parallel tasks for a job. Default is `1`",
    )
    .dynamic();
    let opt_timeoutpay: IntegerConfigOption = ConfigOption::new_i64_no_default(
        OPT_TIMEOUTPAY,
        "Timeout for rebalances until we give up and continue. Default is `120`",
    )
    .dynamic();
    let opt_max_htlc_count: IntegerConfigOption = ConfigOption::new_i64_no_default(
        OPT_MAX_HTLC_COUNT,
        "Max number of htlc allowed pending in job and candidate. Default is `5`",
    )
    .dynamic();
    let opt_stats_delete_failures_age: IntegerConfigOption = ConfigOption::new_i64_no_default(
        OPT_STATS_DELETE_FAILURES_AGE,
        "Max age of failure stats in days. Default is `30`",
    )
    .dynamic();
    let opt_stats_delete_failures_size: IntegerConfigOption = ConfigOption::new_i64_no_default(
        OPT_STATS_DELETE_FAILURES_SIZE,
        "Max number of failure stats per channel. Default is `10000`",
    )
    .dynamic();
    let opt_stats_delete_successes_age: IntegerConfigOption = ConfigOption::new_i64_no_default(
        OPT_STATS_DELETE_SUCCESSES_AGE,
        "Max age of success stats in days. Default is `30`",
    )
    .dynamic();
    let opt_stats_delete_successes_size: IntegerConfigOption = ConfigOption::new_i64_no_default(
        OPT_STATS_DELETE_SUCCESSES_SIZE,
        "Max number of success stats per channel. Default is `10000`",
    )
    .dynamic();
    let opt_inform_layers: StringArrayConfigOption = ConfigOption::new_str_arr_no_default(
        OPT_INFORM_LAYERS,
        "Inform these layers about our information we gather from rebalances. \
         Can be stated multiple times",
    );
    match Builder::new(tokio::io::stdin(), tokio::io::stdout())
        .hook("htlc_accepted", htlc_handler)
        .subscribe("block_added", block_added)
        .option(opt_refresh_peers_interval)
        .option(opt_refresh_aliasmap_interval)
        .option(opt_refresh_gossmap_interval)
        .option(opt_reset_liquidity_interval)
        .option(opt_depleteuptopercent)
        .option(opt_depleteuptoamount)
        .option(opt_maxhops)
        .option(opt_candidates_min_age)
        .option(opt_paralleljobs)
        .option(opt_timeoutpay)
        .option(opt_max_htlc_count)
        .option(opt_stats_delete_failures_age)
        .option(opt_stats_delete_failures_size)
        .option(opt_stats_delete_successes_age)
        .option(opt_stats_delete_successes_size)
        .option(opt_inform_layers)
        .setconfig_callback(setconfig_callback)
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
            &(PLUGIN_NAME.to_string() + "-once"),
            "run sling rebalacnce once",
            slingonce,
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
            let rpc_path = Path::new(&plugin.configuration().lightning_dir)
                .join(plugin.configuration().rpc_file);
            let mut rpc = ClnRpc::new(&rpc_path).await?;
            let sling_dir = Path::new(&plugin.configuration().lightning_dir).join(PLUGIN_NAME);
            let getinfo = rpc.call_typed(&GetinfoRequest {}).await?;
            state = PluginState::new(getinfo.id, rpc_path, sling_dir, getinfo.version);
            {
                *state.blockheight.lock() = getinfo.blockheight;
            }
            match get_startup_options(&plugin, state.clone()).await {
                Ok(()) => &(),
                Err(e) => return plugin.disable(format!("{}", e).as_str()).await,
            };
            log::info!("read startup options");
            confplugin = plugin;
        }
        None => return Err(anyhow!("Error configuring the plugin!")),
    };
    if let Ok(plugin) = confplugin.start(state).await {
        log::debug!("{:?}", plugin.configuration());
        let peersclone = plugin.clone();
        tokio::spawn(async move {
            match tasks::refresh_listpeerchannels_loop(peersclone).await {
                Ok(()) => (),
                Err(e) => log::warn!("Error in refresh_listpeers thread: {:?}", e),
            };
        });
        plugin.state().read_excepts().await?;
        let joblists_clone = plugin.clone();
        refresh_joblists(joblists_clone).await?;
        let channelsclone = plugin.clone();
        tokio::spawn(async move {
            match tasks::refresh_graph(channelsclone).await {
                Ok(()) => (),
                Err(e) => log::warn!("Error in refresh_graph thread: {:?}", e),
            };
        });
        let aliasclone = plugin.clone();
        tokio::spawn(async move {
            match tasks::refresh_aliasmap(aliasclone).await {
                Ok(()) => (),
                Err(e) => log::warn!("Error in refresh_aliasmap thread: {:?}", e),
            };
        });
        let liquidityclone = plugin.clone();
        tokio::spawn(async move {
            match tasks::refresh_liquidity(liquidityclone).await {
                Ok(()) => (),
                Err(e) => log::warn!("Error in refresh_liquidity thread: {:?}", e),
            };
        });
        let tempbanclone = plugin.clone();
        tokio::spawn(async move {
            match tasks::clear_tempbans(tempbanclone).await {
                Ok(()) => (),
                Err(e) => log::warn!("Error in clear_tempbans thread: {:?}", e),
            };
        });
        let clearstatsclone = plugin.clone();
        tokio::spawn(async move {
            match tasks::clear_stats(clearstatsclone).await {
                Ok(()) => (),
                Err(e) => log::warn!("Error in clear_stats thread: {:?}", e),
            };
        });

        plugin.join().await?;
        std::process::exit(0);
    } else {
        Err(anyhow!("Error starting the plugin!"))
    }
}
